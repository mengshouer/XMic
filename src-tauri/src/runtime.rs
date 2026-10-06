use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
use xmic_platform_windows::atomic_replace;

pub(crate) const APP_IDENTIFIER: &str = "com.mengshouer.xmic";
const PORTABLE_MARKER: &str = "portable.flag";
const DATA_DIRECTORY: &str = "data";
const WEBVIEW_DIRECTORY: &str = "webview";
const RUNTIME_DIRECTORY: &str = "runtime";
const OVERRIDE_DIRECTORY: &str = "overrides";
const DSP_LIBRARY_NAME: &str = "xmic_webrtc_dsp.dll";
const HELPER_EXECUTABLE_NAME: &str = "xmic-audio-engine.exe";
const RUN_LOCK_NAME: &str = ".xmic-run.lock";
const MAX_STALE_RUNTIME_ENTRIES: usize = 64;
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);
static RUN_DIRECTORY_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) static EMBEDDED_DSP_LIBRARY: &[u8] = include_bytes!(env!("XMIC_EMBEDDED_DSP_PATH"));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum AppMode {
    Standard,
    Portable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePaths {
    pub(crate) mode: AppMode,
    pub(crate) executable: PathBuf,
    pub(crate) executable_dir: PathBuf,
    pub(crate) data_dir: PathBuf,
    pub(crate) config_dir: PathBuf,
    pub(crate) runtime_root: PathBuf,
    pub(crate) persistent_override_dir: PathBuf,
    pub(crate) webview_data_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DspLibrarySource {
    CommandLine,
    ExecutableSidecar,
    PersistentOverride,
    Embedded,
}

impl DspLibrarySource {
    pub(crate) fn is_custom(self) -> bool {
        self != Self::Embedded
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DspLibrarySelection {
    pub(crate) source: DspLibrarySource,
    pub(crate) path: PathBuf,
}

pub(crate) struct RuntimeSession {
    pub(crate) paths: RuntimePaths,
    pub(crate) run_dir: PathBuf,
    pub(crate) helper_executable: PathBuf,
    pub(crate) managed_dsp: PathBuf,
    pub(crate) selection: DspLibrarySelection,
    selection_pin: Mutex<Option<File>>,
    selection_guard_error: Option<String>,
    run_lock: Mutex<Option<File>>,
    cleaned: AtomicBool,
}

impl RuntimeSession {
    /// The caller must stop and wait for the helper before cleanup so its executable and loaded DSP
    /// module are no longer in use.
    pub(crate) fn cleanup(&self) {
        if self.cleaned.swap(true, Ordering::AcqRel) {
            return;
        }
        self.selection_pin
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        self.run_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let _ = fs::remove_dir_all(&self.run_dir);
    }

    pub(crate) fn selected_file_name(&self) -> String {
        self.selection
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    pub(crate) fn selection_is_pinned(&self) -> bool {
        self.selection_pin
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }

    pub(crate) fn selection_guard_error(&self) -> Option<String> {
        self.selection_guard_error.clone()
    }

    pub(crate) fn managed_dsp_matches_embedded(&self) -> bool {
        file_matches(&self.managed_dsp, EMBEDDED_DSP_LIBRARY).unwrap_or(false)
    }
}

impl Drop for RuntimeSession {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UiArguments {
    pub(crate) dsp_library: Option<PathBuf>,
}

pub(crate) fn parse_ui_arguments(
    args: impl IntoIterator<Item = OsString>,
    invocation_cwd: &Path,
) -> Result<UiArguments, String> {
    let mut dsp_library = None;
    let mut args = args.into_iter();
    while let Some(argument) = args.next() {
        let value = if argument == "--dsp-library" {
            Some(
                args.next()
                    .ok_or_else(|| "missing value for --dsp-library".to_owned())?,
            )
        } else {
            inline_dsp_library_value(&argument)
        };
        let Some(value) = value else { continue };
        if dsp_library.is_some() {
            return Err("--dsp-library may only be specified once".to_owned());
        }
        if value.is_empty() {
            return Err("--dsp-library requires a non-empty path".to_owned());
        }
        let path = PathBuf::from(value);
        dsp_library = Some(if path.is_absolute() {
            path
        } else {
            invocation_cwd.join(path)
        });
    }
    Ok(UiArguments { dsp_library })
}

fn inline_dsp_library_value(argument: &OsStr) -> Option<OsString> {
    let argument = argument.encode_wide().collect::<Vec<_>>();
    let prefix = "--dsp-library=".encode_utf16().collect::<Vec<_>>();
    argument
        .starts_with(&prefix)
        .then(|| OsString::from_wide(&argument[prefix.len()..]))
}

pub(crate) fn invocation_requests_dsp_override(args: &[String]) -> bool {
    args.iter()
        .any(|argument| argument == "--dsp-library" || argument.starts_with("--dsp-library="))
}

pub(crate) fn discover_runtime_paths_from_environment() -> Result<RuntimePaths, String> {
    let executable =
        env::current_exe().map_err(|error| format!("locate XMic executable: {error}"))?;
    let app_data = env::var_os("APPDATA").map(PathBuf::from);
    let local_app_data = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    discover_runtime_paths(executable, app_data.as_deref(), local_app_data.as_deref())
}

pub(crate) fn discover_runtime_paths(
    executable: PathBuf,
    app_data: Option<&Path>,
    local_app_data: Option<&Path>,
) -> Result<RuntimePaths, String> {
    let executable_dir = executable
        .parent()
        .ok_or_else(|| "XMic executable has no parent directory".to_owned())?
        .to_path_buf();
    let marker = executable_dir.join(PORTABLE_MARKER);
    let mode = match fs::symlink_metadata(&marker) {
        Ok(_) => AppMode::Portable,
        Err(error) if error.kind() == io::ErrorKind::NotFound => AppMode::Standard,
        Err(error) => {
            return Err(format!(
                "inspect portable marker {}: {error}",
                marker.display()
            ))
        }
    };

    match mode {
        AppMode::Portable => {
            let data_dir = executable_dir.join(DATA_DIRECTORY);
            Ok(RuntimePaths {
                mode,
                executable,
                executable_dir,
                config_dir: data_dir.clone(),
                runtime_root: data_dir.join(RUNTIME_DIRECTORY),
                persistent_override_dir: data_dir.join(OVERRIDE_DIRECTORY),
                webview_data_dir: data_dir.join(WEBVIEW_DIRECTORY),
                data_dir,
            })
        }
        AppMode::Standard => {
            let app_data =
                app_data.ok_or_else(|| "APPDATA is unavailable for Standard mode".to_owned())?;
            let local_app_data = local_app_data
                .ok_or_else(|| "LOCALAPPDATA is unavailable for Standard mode".to_owned())?;
            let data_dir = app_data.join(APP_IDENTIFIER);
            Ok(RuntimePaths {
                mode,
                executable,
                executable_dir,
                config_dir: data_dir.clone(),
                runtime_root: local_app_data.join(APP_IDENTIFIER).join(RUNTIME_DIRECTORY),
                persistent_override_dir: data_dir.join(OVERRIDE_DIRECTORY),
                webview_data_dir: local_app_data.join(APP_IDENTIFIER),
                data_dir,
            })
        }
    }
}

pub(crate) fn prepare_runtime(
    paths: RuntimePaths,
    command_line_override: Option<PathBuf>,
) -> Result<RuntimeSession, String> {
    if paths.mode == AppMode::Portable {
        probe_portable_data_directory(&paths.data_dir)?;
    }
    fs::create_dir_all(&paths.config_dir).map_err(|error| {
        format!(
            "create data directory {}: {error}",
            paths.config_dir.display()
        )
    })?;
    fs::create_dir_all(&paths.runtime_root).map_err(|error| {
        format!(
            "create runtime directory {}: {error}",
            paths.runtime_root.display()
        )
    })?;
    cleanup_stale_runtime(&paths.runtime_root);
    if paths.mode == AppMode::Standard {
        cleanup_legacy_runtime(&paths.config_dir.join("engine-runtime"));
    }

    let run_sequence = RUN_DIRECTORY_COUNTER.fetch_add(1, Ordering::Relaxed);
    let run_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let run_dir = paths.runtime_root.join(format!(
        "run-{}-{run_timestamp:x}-{run_sequence}",
        std::process::id()
    ));
    fs::create_dir(&run_dir)
        .map_err(|error| format!("create runtime directory {}: {error}", run_dir.display()))?;
    let run_lock = match create_run_lock(&run_dir) {
        Ok(lock) => lock,
        Err(error) => {
            let _ = fs::remove_dir_all(&run_dir);
            return Err(format!(
                "lock runtime directory {}: {error}",
                run_dir.display()
            ));
        }
    };

    let prepared = (|| {
        let helper_executable = run_dir.join(HELPER_EXECUTABLE_NAME);
        atomic_copy(&paths.executable, &helper_executable).map_err(|error| {
            format!(
                "materialize audio helper {}: {error}",
                helper_executable.display()
            )
        })?;
        let managed_dsp = run_dir.join(DSP_LIBRARY_NAME);
        write_embedded_dsp_if_needed(&managed_dsp, EMBEDDED_DSP_LIBRARY).map_err(|error| {
            format!(
                "materialize embedded WebRTC DSP {}: {error}",
                managed_dsp.display()
            )
        })?;

        let mut selection = resolve_dsp_library(&paths, command_line_override, &managed_dsp);
        let (selection_pin, selection_guard_error) = guard_selected_library(&mut selection);
        Ok::<_, String>((
            helper_executable,
            managed_dsp,
            selection,
            selection_pin,
            selection_guard_error,
        ))
    })();

    let (helper_executable, managed_dsp, selection, selection_pin, selection_guard_error) =
        match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                drop(run_lock);
                let _ = fs::remove_dir_all(&run_dir);
                return Err(error);
            }
        };

    Ok(RuntimeSession {
        paths,
        run_dir,
        helper_executable,
        managed_dsp,
        selection,
        selection_pin: Mutex::new(selection_pin),
        selection_guard_error,
        run_lock: Mutex::new(Some(run_lock)),
        cleaned: AtomicBool::new(false),
    })
}

pub(crate) fn resolve_dsp_library(
    paths: &RuntimePaths,
    command_line_override: Option<PathBuf>,
    managed_dsp: &Path,
) -> DspLibrarySelection {
    if let Some(path) = command_line_override {
        return DspLibrarySelection {
            source: DspLibrarySource::CommandLine,
            path,
        };
    }

    let executable_sidecar = paths.executable_dir.join(DSP_LIBRARY_NAME);
    if candidate_is_present_or_inaccessible(&executable_sidecar) {
        return DspLibrarySelection {
            source: DspLibrarySource::ExecutableSidecar,
            path: executable_sidecar,
        };
    }

    let persistent_override = paths.persistent_override_dir.join(DSP_LIBRARY_NAME);
    if candidate_is_present_or_inaccessible(&persistent_override) {
        return DspLibrarySelection {
            source: DspLibrarySource::PersistentOverride,
            path: persistent_override,
        };
    }

    DspLibrarySelection {
        source: DspLibrarySource::Embedded,
        path: managed_dsp.to_path_buf(),
    }
}

fn candidate_is_present_or_inaccessible(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    }
}

fn probe_portable_data_directory(data_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(data_dir).map_err(|error| {
        format!(
            "Portable mode requires a writable data directory at {}: {error}",
            data_dir.display()
        )
    })?;
    let probe_timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let probe = data_dir.join(format!(
        ".xmic-write-probe-{}-{probe_timestamp:x}-{}",
        std::process::id(),
        TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)?;
        file.write_all(b"xmic-portable-write-probe")?;
        file.sync_all()?;
        drop(file);
        fs::remove_file(&probe)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&probe);
        return Err(format!(
            "Portable mode requires a writable data directory at {}: {error}",
            data_dir.display()
        ));
    }
    Ok(())
}

fn create_run_lock(run_dir: &Path) -> io::Result<File> {
    let mut lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(run_dir.join(RUN_LOCK_NAME))?;
    write!(lock, "{}", std::process::id())?;
    lock.sync_all()?;
    Ok(lock)
}

fn cleanup_stale_runtime(runtime_root: &Path) {
    let Ok(entries) = fs::read_dir(runtime_root) else {
        return;
    };
    for entry in entries
        .take(MAX_STALE_RUNTIME_ENTRIES)
        .filter_map(Result::ok)
    {
        let path = entry.path();
        let is_directory = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if is_directory && runtime_directory_is_locked(&path) {
            continue;
        }
        let _ = if is_directory {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
    }
}

fn runtime_directory_is_locked(path: &Path) -> bool {
    match OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path.join(RUN_LOCK_NAME))
    {
        Ok(_) => false,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        // A sharing violation identifies a live RuntimeSession. Other access errors are also kept
        // conservatively: bounded cleanup must never damage an entry it cannot prove is stale.
        Err(_) => true,
    }
}

fn cleanup_legacy_runtime(legacy_root: &Path) {
    if legacy_root.is_dir() {
        let _ = fs::remove_dir_all(legacy_root);
    } else {
        let _ = fs::remove_file(legacy_root);
    }
}

fn write_embedded_dsp_if_needed(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if file_matches(path, bytes).unwrap_or(false) {
        return Ok(());
    }
    atomic_write(path, bytes)
}

fn file_matches(path: &Path, expected: &[u8]) -> io::Result<bool> {
    let metadata = fs::metadata(path)?;
    if metadata.len() != expected.len() as u64 {
        return Ok(false);
    }
    let mut file = File::open(path)?;
    let mut actual = Vec::with_capacity(expected.len());
    file.read_to_end(&mut actual)?;
    Ok(actual == expected)
}

fn atomic_copy(source: &Path, destination: &Path) -> io::Result<()> {
    let temporary = temporary_sibling(destination);
    let result = (|| {
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        io::copy(&mut input, &mut output)?;
        output.sync_all()?;
        drop(output);
        atomic_replace(&temporary, destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn atomic_write(destination: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = temporary_sibling(destination);
    let result = (|| {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        output.write_all(bytes)?;
        output.sync_all()?;
        drop(output);
        atomic_replace(&temporary, destination)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn temporary_sibling(destination: &Path) -> PathBuf {
    let sequence = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = destination
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    destination.with_file_name(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ))
}

fn guard_selected_library(selection: &mut DspLibrarySelection) -> (Option<File>, Option<String>) {
    let canonical_path = match fs::canonicalize(&selection.path) {
        Ok(path) => path,
        Err(error) => {
            return (
                None,
                Some(format!(
                    "pin selected DSP library {} for this application run: {error}",
                    selection.path.display()
                )),
            )
        }
    };
    selection.path = canonical_path;
    match pin_selected_library(&selection.path) {
        Ok(file) => (Some(file), None),
        Err(error) => (
            None,
            Some(format!(
                "pin selected DSP library {} for this application run: {error}",
                selection.path.display()
            )),
        ),
    }
}

fn pin_selected_library(path: &Path) -> io::Result<File> {
    // Denying write and delete sharing freezes the selected file's identity and bytes across helper
    // crashes/restarts without ever loading native code in the UI process.
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "xmic-runtime-{name}-{}-{}",
            std::process::id(),
            TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn standard_paths(root: &Path) -> RuntimePaths {
        let executable = root.join("bin").join("XMic.exe");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"test executable").unwrap();
        discover_runtime_paths(
            executable,
            Some(&root.join("roaming")),
            Some(&root.join("local")),
        )
        .unwrap()
    }

    #[test]
    fn marker_alone_selects_portable_mode_and_isolates_paths() {
        let root = test_root("portable-paths");
        let executable = root.join("XMic.exe");
        fs::create_dir_all(&root).unwrap();
        fs::write(&executable, b"exe").unwrap();
        fs::create_dir(root.join("data")).unwrap();

        let standard = discover_runtime_paths(
            executable.clone(),
            Some(&root.join("roaming")),
            Some(&root.join("local")),
        )
        .unwrap();
        assert_eq!(standard.mode, AppMode::Standard);
        assert_eq!(standard.data_dir, root.join("roaming").join(APP_IDENTIFIER));

        fs::write(root.join(PORTABLE_MARKER), []).unwrap();
        let portable = discover_runtime_paths(
            executable,
            Some(&root.join("roaming")),
            Some(&root.join("local")),
        )
        .unwrap();
        assert_eq!(portable.mode, AppMode::Portable);
        assert_eq!(portable.config_dir, root.join("data"));
        assert_eq!(portable.runtime_root, root.join("data/runtime"));
        assert_eq!(portable.webview_data_dir, root.join("data/webview"));
        assert_eq!(
            portable.persistent_override_dir,
            root.join("data/overrides")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dsp_resolution_uses_the_approved_precedence() {
        let root = test_root("precedence");
        let paths = standard_paths(&root);
        let managed = root.join("managed.dll");
        fs::write(&managed, b"embedded").unwrap();
        let persistent = paths.persistent_override_dir.join(DSP_LIBRARY_NAME);
        fs::create_dir_all(persistent.parent().unwrap()).unwrap();
        fs::write(&persistent, b"persistent").unwrap();
        let executable = paths.executable_dir.join(DSP_LIBRARY_NAME);
        fs::write(&executable, b"sidecar").unwrap();
        let cli = root.join("cli.dll");

        assert_eq!(
            resolve_dsp_library(&paths, Some(cli.clone()), &managed),
            DspLibrarySelection {
                source: DspLibrarySource::CommandLine,
                path: cli,
            }
        );
        assert_eq!(
            resolve_dsp_library(&paths, None, &managed).source,
            DspLibrarySource::ExecutableSidecar
        );
        fs::remove_file(executable).unwrap();
        assert_eq!(
            resolve_dsp_library(&paths, None, &managed).source,
            DspLibrarySource::PersistentOverride
        );
        fs::remove_file(persistent).unwrap();
        assert_eq!(
            resolve_dsp_library(&paths, None, &managed).source,
            DspLibrarySource::Embedded
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_explicit_missing_library_is_selected_fail_closed() {
        let root = test_root("missing-cli");
        let paths = standard_paths(&root);
        let managed = root.join("managed.dll");
        let missing = root.join("missing.dll");

        let selection = resolve_dsp_library(&paths, Some(missing.clone()), &managed);

        assert_eq!(selection.source, DspLibrarySource::CommandLine);
        assert_eq!(selection.path, missing);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn relative_cli_paths_use_the_invocation_working_directory() {
        let cwd = Path::new("C:/invocation");
        let parsed = parse_ui_arguments(
            [
                OsString::from("--dsp-library"),
                OsString::from("custom/dsp.dll"),
            ],
            cwd,
        )
        .unwrap();
        assert_eq!(parsed.dsp_library, Some(cwd.join("custom/dsp.dll")));
        let inline = parse_ui_arguments([OsString::from("--dsp-library=other.dll")], cwd).unwrap();
        assert_eq!(inline.dsp_library, Some(cwd.join("other.dll")));
        assert!(parse_ui_arguments([OsString::from("--dsp-library")], cwd).is_err());
        assert!(parse_ui_arguments([OsString::from("--dsp-library=")], cwd).is_err());
        assert!(parse_ui_arguments(
            [
                OsString::from("--dsp-library=a.dll"),
                OsString::from("--dsp-library"),
                OsString::from("b.dll"),
            ],
            cwd,
        )
        .is_err());
    }

    #[test]
    fn managed_dsp_repairs_corrupt_content_without_touching_siblings() {
        let root = test_root("managed-repair");
        fs::create_dir_all(&root).unwrap();
        let target = root.join(DSP_LIBRARY_NAME);
        let sibling = root.join("keep.txt");
        fs::write(&target, b"corrupt").unwrap();
        fs::write(&sibling, b"keep").unwrap();

        write_embedded_dsp_if_needed(&target, b"official bytes").unwrap();
        write_embedded_dsp_if_needed(&target, b"official bytes").unwrap();

        assert_eq!(fs::read(target).unwrap(), b"official bytes");
        assert_eq!(fs::read(sibling).unwrap(), b"keep");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_explicit_library_stays_selected_and_latches_a_guard_error() {
        let root = test_root("missing-cli-guard");
        let paths = standard_paths(&root);
        let missing = root.join("missing.dll");

        let session = prepare_runtime(paths, Some(missing.clone())).unwrap();

        assert_eq!(session.selection.source, DspLibrarySource::CommandLine);
        assert_eq!(session.selection.path, missing);
        assert!(!session.selection_is_pinned());
        assert!(session
            .selection_guard_error()
            .is_some_and(|error| error.contains("pin selected DSP library")));
        assert!(session.managed_dsp_matches_embedded());
        session.cleanup();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn selected_custom_library_is_pinned_until_runtime_cleanup() {
        let root = test_root("selection-pin");
        let paths = standard_paths(&root);
        let sidecar = paths.executable_dir.join(DSP_LIBRARY_NAME);
        fs::write(&sidecar, b"custom").unwrap();

        let session = prepare_runtime(paths, None).unwrap();

        assert_eq!(
            session.selection.source,
            DspLibrarySource::ExecutableSidecar
        );
        assert!(session.selection_is_pinned());
        assert!(session.selection_guard_error().is_none());
        assert!(OpenOptions::new().write(true).open(&sidecar).is_err());
        assert!(fs::remove_file(&sidecar).is_err());
        session.cleanup();
        assert!(OpenOptions::new().write(true).open(&sidecar).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn portable_data_path_failure_never_falls_back_to_standard_paths() {
        let root = test_root("portable-unwritable");
        fs::create_dir_all(&root).unwrap();
        let executable = root.join("XMic.exe");
        fs::write(&executable, b"exe").unwrap();
        fs::write(root.join(PORTABLE_MARKER), []).unwrap();
        fs::write(root.join(DATA_DIRECTORY), b"not a directory").unwrap();
        let roaming = root.join("roaming");
        let local = root.join("local");
        let paths = discover_runtime_paths(executable, Some(&roaming), Some(&local)).unwrap();

        let error = match prepare_runtime(paths, None) {
            Ok(_) => panic!("portable preparation unexpectedly succeeded"),
            Err(error) => error,
        };

        assert!(error.contains("Portable mode requires a writable data directory"));
        assert!(!roaming.join(APP_IDENTIFIER).exists());
        assert!(!local.join(APP_IDENTIFIER).exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_cleanup_skips_a_runtime_directory_with_a_live_lock() {
        let root = test_root("active-cleanup");
        fs::create_dir_all(&root).unwrap();
        let active = root.join("run-active");
        let stale = root.join("run-stale");
        fs::create_dir(&active).unwrap();
        fs::create_dir(&stale).unwrap();
        fs::write(stale.join("artifact"), b"stale").unwrap();
        let lock = create_run_lock(&active).unwrap();

        cleanup_stale_runtime(&root);

        assert!(active.exists());
        assert!(!stale.exists());
        drop(lock);
        cleanup_stale_runtime(&root);
        assert!(!active.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_cleanup_work_is_bounded_per_startup() {
        let root = test_root("bounded-cleanup");
        fs::create_dir_all(&root).unwrap();
        for index in 0..(MAX_STALE_RUNTIME_ENTRIES + 5) {
            fs::write(root.join(format!("artifact-{index:03}")), b"stale").unwrap();
        }

        cleanup_stale_runtime(&root);

        assert_eq!(fs::read_dir(&root).unwrap().count(), 5);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn current_cleanup_removes_only_the_per_run_directory() {
        let root = test_root("cleanup");
        let paths = standard_paths(&root);
        fs::create_dir_all(&paths.runtime_root).unwrap();
        let run_dir = paths.runtime_root.join("run-test");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("artifact"), b"x").unwrap();
        fs::create_dir_all(&paths.persistent_override_dir).unwrap();
        fs::write(paths.config_dir.join("settings.json"), b"settings").unwrap();

        let session = RuntimeSession {
            paths: paths.clone(),
            run_dir: run_dir.clone(),
            helper_executable: run_dir.join(HELPER_EXECUTABLE_NAME),
            managed_dsp: run_dir.join(DSP_LIBRARY_NAME),
            selection: DspLibrarySelection {
                source: DspLibrarySource::Embedded,
                path: run_dir.join(DSP_LIBRARY_NAME),
            },
            selection_pin: Mutex::new(None),
            selection_guard_error: None,
            run_lock: Mutex::new(None),
            cleaned: AtomicBool::new(false),
        };
        session.cleanup();
        session.cleanup();

        assert!(!run_dir.exists());
        assert!(paths.runtime_root.exists());
        assert!(paths.config_dir.join("settings.json").exists());
        assert!(paths.persistent_override_dir.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn installer_resources_do_not_ship_a_second_dsp_library() {
        let bundle: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.bundle.conf.json")).unwrap();
        let resources = bundle["bundle"]["resources"].as_object().unwrap();
        assert!(resources
            .keys()
            .all(|resource| !resource.ends_with(DSP_LIBRARY_NAME)));
    }
}
