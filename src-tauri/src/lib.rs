use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Mutex,
    },
    thread,
};

#[cfg(target_os = "windows")]
use std::{os::windows::process::CommandExt, process::Command};

use serde::Serialize;
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, RunEvent, State, WebviewWindowBuilder, Wry,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_notification::NotificationExt;
use xmic_engine_protocol::{EngineOutput, EngineSettings, EngineState, EngineStatus};
use xmic_platform_api::EndpointDescriptor;
use xmic_platform_windows::start_system_lifecycle_monitor;

mod endpoint_cache;
mod engine;
mod engine_client;
mod engine_ipc;
mod engine_process;
mod lifecycle;
mod route_validation;
mod runtime;
mod settings_store;

use endpoint_cache::EndpointCache;
use engine_client::{EngineController, TrayState};
use lifecycle::{
    BackgroundLifecycle, LifecycleController, LifecyclePreferences, LifecyclePreferencesStore,
    LifecycleStatus,
};
use runtime::{
    discover_runtime_paths_from_environment, invocation_requests_dsp_override, parse_ui_arguments,
    prepare_runtime, AppMode, DspLibrarySource, RuntimeSession,
};

pub fn run_audio_engine_process() -> Result<(), String> {
    engine_process::run_from_environment()
}

const AUTOSTART_ARGUMENT: &str = "--autostart";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(target_os = "windows")]
const VB_CABLE_DOWNLOAD_URL: &str = "https://vb-audio.com/Cable/";
#[cfg(target_os = "windows")]
const PROJECT_URL: &str = "https://github.com/mengshouer/XMic";
#[cfg(target_os = "windows")]
const MPL_LICENSE_URL: &str = "https://www.mozilla.org/MPL/2.0/";
#[cfg(target_os = "windows")]
const WEBVIEW2_DOWNLOAD_URL: &str = "https://go.microsoft.com/fwlink/p/?LinkId=2124703";
const DSP_OVERRIDE_WARNING_EVENT: &str = "dsp-override-not-applied";
const TRAY_ID: &str = "main-tray";
const MENU_SHOW: &str = "show";
const MENU_TOGGLE_PROCESSING: &str = "toggle-processing";
const MENU_QUIT: &str = "quit";
/// Windows hides the window with `ShowWindow(SW_HIDE)` when it goes to the tray, and WebView2 does
/// not report that as `document.hidden` — the page keeps running, invisible, and would keep polling
/// forever. Rust performs both transitions, so it is the one that can say which happened.
const WINDOW_VISIBILITY_EVENT: &str = "window-visibility";
const LANGUAGE_ZH_CN: u8 = 0;
const LANGUAGE_EN: u8 = 1;

#[derive(Default)]
struct SecondaryInvocationState {
    show_window_pending: AtomicBool,
    override_warning_pending: AtomicBool,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
enum ExternalLinkTarget {
    VbCable,
    Project,
    MplLicense,
    MplSourceArchive,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplicationDiagnostics {
    mode: AppMode,
    data_directory: String,
    runtime_directory: String,
    webview_data_directory: String,
    dsp_source: DspLibrarySource,
    dsp_custom: bool,
    dsp_library_path: String,
    dsp_library_file_name: String,
    dsp_selection_pinned: bool,
    dsp_selection_guard_error: Option<String>,
    expected_dsp_abi: u32,
    actual_dsp_abi: Option<u32>,
    dsp_validation_error: Option<String>,
}

fn application_diagnostics(
    runtime: &RuntimeSession,
    validation: xmic_engine_protocol::DspLibraryValidation,
) -> ApplicationDiagnostics {
    ApplicationDiagnostics {
        mode: runtime.paths.mode,
        data_directory: runtime.paths.data_dir.to_string_lossy().into_owned(),
        runtime_directory: runtime.paths.runtime_root.to_string_lossy().into_owned(),
        webview_data_directory: runtime
            .paths
            .webview_data_dir
            .to_string_lossy()
            .into_owned(),
        dsp_source: runtime.selection.source,
        dsp_custom: runtime.selection.source.is_custom(),
        dsp_library_path: runtime.selection.path.to_string_lossy().into_owned(),
        dsp_library_file_name: runtime.selected_file_name(),
        dsp_selection_pinned: runtime.selection_is_pinned(),
        dsp_selection_guard_error: runtime.selection_guard_error(),
        expected_dsp_abi: validation.expected_abi,
        actual_dsp_abi: validation.actual_abi,
        dsp_validation_error: validation.error,
    }
}

#[derive(Clone, Copy)]
enum UiLanguage {
    ZhCn,
    En,
}

impl UiLanguage {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "zh-CN" => Some(Self::ZhCn),
            "en" => Some(Self::En),
            _ => None,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::ZhCn => LANGUAGE_ZH_CN,
            Self::En => LANGUAGE_EN,
        }
    }

    fn from_code(value: u8) -> Self {
        if value == LANGUAGE_EN {
            Self::En
        } else {
            Self::ZhCn
        }
    }
}

struct TrayLabels {
    show: &'static str,
    start_processing: &'static str,
    stop_processing: &'static str,
    configure_microphone: &'static str,
    quit: &'static str,
    off_tooltip: &'static str,
    processing_tooltip: &'static str,
    degraded_raw_tooltip: &'static str,
    degraded_silent_tooltip: &'static str,
    blocked_tooltip: &'static str,
    retrying_tooltip: &'static str,
    settings_reset_tooltip: &'static str,
    apply_settings: &'static str,
    configuration_tooltip: &'static str,
}

fn tray_labels(language: UiLanguage) -> TrayLabels {
    match language {
        UiLanguage::ZhCn => TrayLabels {
            show: "打开 XMic",
            start_processing: "启动处理",
            stop_processing: "停止处理",
            configure_microphone: "配置音频设备",
            quit: "退出 XMic",
            off_tooltip: "XMic - 已关闭 · 虚拟输出未激活",
            processing_tooltip: "XMic - 处理中 · 处理后音频",
            degraded_raw_tooltip: "XMic - 降级 · 原始麦克风",
            degraded_silent_tooltip: "XMic - 降级 · 静音",
            blocked_tooltip: "XMic - 已阻止 · 虚拟输出未激活",
            retrying_tooltip: "XMic - 正在恢复 · 虚拟输出未激活",
            settings_reset_tooltip: "XMic - 设置已重置 · 请应用当前默认值",
            apply_settings: "应用设置后启动",
            configuration_tooltip: "XMic - 已阻止 · 需要配置音频设备",
        },
        UiLanguage::En => TrayLabels {
            show: "Open XMic",
            start_processing: "Start processing",
            stop_processing: "Stop processing",
            configure_microphone: "Configure audio devices",
            quit: "Quit XMic",
            off_tooltip: "XMic - Off · virtual output inactive",
            processing_tooltip: "XMic - Processing · processed audio",
            degraded_raw_tooltip: "XMic - Degraded · raw microphone",
            degraded_silent_tooltip: "XMic - Degraded · silence",
            blocked_tooltip: "XMic - Blocked · virtual output inactive",
            retrying_tooltip: "XMic - Recovering · virtual output inactive",
            settings_reset_tooltip: "XMic - Settings reset · apply current defaults",
            apply_settings: "Apply settings before starting",
            configuration_tooltip: "XMic - Blocked · audio device setup required",
        },
    }
}

struct TrayMenuState {
    language: AtomicU8,
    /// What the tray was last drawn with. The UI polls about once a second and the tray is almost
    /// always identical between polls, so this is what stops every poll from rewriting the menu
    /// items and cloning the whole RGBA icon on the main thread.
    rendered: Mutex<Option<RenderedTray>>,
    show: MenuItem<Wry>,
    toggle_processing: MenuItem<Wry>,
    quit: MenuItem<Wry>,
}

/// Every value `refresh_tray` actually paints. The menu labels not listed here are derived from
/// `language`, so comparing the code covers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenderedTray {
    language: u8,
    label: &'static str,
    enabled: bool,
    tooltip: &'static str,
    processed: bool,
}

/// The green processed-output microphone and its grey status twin. At roughly 16 px a corner badge
/// would be a handful of muddy pixels, so every non-Processed output uses the same shape with colour
/// removed instead of shipping a second asset that could drift from it.
struct TrayIcons {
    processed: Image<'static>,
    non_processed: Image<'static>,
}

impl TrayIcons {
    fn from_window_icon(icon: &Image<'_>) -> Self {
        let processed = Image::new_owned(icon.rgba().to_vec(), icon.width(), icon.height());
        Self {
            non_processed: Image::new_owned(
                desaturate_rgba(icon.rgba()),
                icon.width(),
                icon.height(),
            ),
            processed,
        }
    }

    fn select(&self, processed: bool) -> &Image<'static> {
        if processed {
            &self.processed
        } else {
            &self.non_processed
        }
    }
}

/// Rec. 709 luminance, alpha untouched, so the dark background stays dark and the coloured microphone
/// turns grey.
fn desaturate_rgba(rgba: &[u8]) -> Vec<u8> {
    let mut output = rgba.to_vec();
    for pixel in output.chunks_exact_mut(4) {
        let luminance = (0.2126 * f32::from(pixel[0])
            + 0.7152 * f32::from(pixel[1])
            + 0.0722 * f32::from(pixel[2]))
        .round()
        .clamp(0.0, 255.0) as u8;
        pixel[0] = luminance;
        pixel[1] = luminance;
        pixel[2] = luminance;
    }
    output
}

#[tauri::command]
fn get_bootstrap_status(
    app: AppHandle,
    controller: State<'_, EngineController>,
) -> Result<EngineStatus, String> {
    get_engine_status(app, controller)
}

#[tauri::command]
fn list_audio_endpoints(
    cache: State<'_, EndpointCache>,
) -> Result<Vec<EndpointDescriptor>, String> {
    cache
        .endpoints()
        .map(|endpoints| endpoints.as_ref().clone())
}

#[tauri::command]
fn get_engine_status(
    app: AppHandle,
    controller: State<'_, EngineController>,
) -> Result<EngineStatus, String> {
    let status = engine_status(&app, &controller)?;
    app.state::<LifecycleController>()
        .synchronize_configuration_issue(status.configuration_issue().cloned());
    schedule_tray_refresh(&app, TrayState::from_status(&status));
    Ok(status)
}

#[tauri::command]
fn get_lifecycle_status(
    app: AppHandle,
    lifecycle: State<'_, LifecycleController>,
) -> Result<LifecycleStatus, String> {
    current_lifecycle_status(&app, &lifecycle)
}

#[tauri::command]
fn set_ui_language(app: AppHandle, language: String) -> Result<(), String> {
    let language = UiLanguage::parse(&language)
        .ok_or_else(|| format!("unsupported interface language: {language}"))?;
    let menu = app
        .try_state::<TrayMenuState>()
        .ok_or_else(|| "tray menu is unavailable".to_owned())?;
    menu.language.store(language.code(), Ordering::Relaxed);
    schedule_tray_refresh(&app, app.state::<EngineController>().cached_tray_state());
    Ok(())
}

#[tauri::command]
fn open_external_link(target: ExternalLinkTarget) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let url = external_link_url(target);
        open_https_url(url)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = target;
        Err("opening external links is only supported on Windows".to_owned())
    }
}

#[tauri::command]
fn open_vb_cable_download() -> Result<(), String> {
    open_external_link(ExternalLinkTarget::VbCable)
}

#[cfg(target_os = "windows")]
fn external_link_url(target: ExternalLinkTarget) -> &'static str {
    match target {
        ExternalLinkTarget::VbCable => VB_CABLE_DOWNLOAD_URL,
        ExternalLinkTarget::Project => PROJECT_URL,
        ExternalLinkTarget::MplLicense => MPL_LICENSE_URL,
        ExternalLinkTarget::MplSourceArchive => concat!(
            "https://github.com/mengshouer/XMic/releases/download/v",
            env!("CARGO_PKG_VERSION"),
            "/XMic_",
            env!("CARGO_PKG_VERSION"),
            "_MPL-2.0-source.zip"
        ),
    }
}

#[cfg(target_os = "windows")]
fn open_https_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("external URL is not HTTPS".to_owned());
    }
    Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("open external page: {error}"))
}

#[tauri::command]
fn get_application_diagnostics(
    runtime: State<'_, RuntimeSession>,
    controller: State<'_, EngineController>,
) -> ApplicationDiagnostics {
    application_diagnostics(&runtime, controller.cached_status().dsp_validation)
}

#[tauri::command]
fn take_pending_dsp_override_warning(state: State<'_, SecondaryInvocationState>) -> bool {
    state.override_warning_pending.swap(false, Ordering::AcqRel)
}

#[tauri::command]
fn update_lifecycle_preferences(
    app: AppHandle,
    lifecycle: State<'_, LifecycleController>,
    preferences: LifecyclePreferences,
) -> Result<LifecycleStatus, String> {
    let autolaunch = app.autolaunch();
    let previous_launch_at_login = autolaunch
        .is_enabled()
        .map_err(|error| format!("读取开机启动状态失败：{error}"))?;
    let launch_setting_changed = previous_launch_at_login != preferences.launch_at_login;

    if launch_setting_changed {
        set_launch_at_login(&autolaunch, preferences.launch_at_login)?;
    }

    if let Err(error) = lifecycle.set_startup_preferences(
        preferences.start_in_tray,
        preferences.start_engine_on_launch,
    ) {
        if launch_setting_changed {
            if let Err(rollback_error) = set_launch_at_login(&autolaunch, previous_launch_at_login)
            {
                return Err(format!(
                    "保存启动偏好失败：{error}；回滚开机启动也失败：{rollback_error}"
                ));
            }
        }
        return Err(error);
    }

    let status = current_lifecycle_status(&app, &lifecycle)?;
    app.emit("lifecycle-status", &status)
        .map_err(|error| error.to_string())?;
    schedule_tray_refresh(&app, app.state::<EngineController>().cached_tray_state());
    Ok(status)
}

fn set_launch_at_login(
    autolaunch: &tauri_plugin_autostart::AutoLaunchManager,
    enabled: bool,
) -> Result<(), String> {
    let result = if enabled {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };
    result.map_err(|error| format!("更新开机启动失败：{error}"))
}

#[tauri::command]
fn update_engine_settings(
    app: AppHandle,
    controller: State<'_, EngineController>,
    lifecycle: State<'_, LifecycleController>,
    settings: EngineSettings,
) -> Result<EngineStatus, String> {
    controller.update_settings(settings)?;
    lifecycle.clear_last_error();
    let status = emit_status(&app, &controller)?;
    lifecycle.synchronize_configuration_issue(status.configuration_issue().cloned());
    let _ = emit_lifecycle_status(&app);
    Ok(status)
}

#[tauri::command]
fn start_engine(
    app: AppHandle,
    controller: State<'_, EngineController>,
    lifecycle: State<'_, LifecycleController>,
) -> Result<EngineStatus, String> {
    if let Err(error) = lifecycle.start_manually(&controller) {
        emit_application_state(&app);
        return Err(error);
    }
    let status = emit_status(&app, &controller)?;
    let _ = emit_lifecycle_status(&app);
    Ok(status)
}

#[tauri::command]
fn stop_engine(
    app: AppHandle,
    controller: State<'_, EngineController>,
    lifecycle: State<'_, LifecycleController>,
) -> Result<EngineStatus, String> {
    lifecycle.stop_manually(&controller)?;
    let status = emit_status(&app, &controller)?;
    let _ = emit_lifecycle_status(&app);
    Ok(status)
}

fn engine_status(app: &AppHandle, controller: &EngineController) -> Result<EngineStatus, String> {
    let endpoints = app.state::<EndpointCache>().endpoints()?;
    Ok(controller.status(endpoints.as_ref().clone()))
}

fn emit_status(app: &AppHandle, controller: &EngineController) -> Result<EngineStatus, String> {
    let status = engine_status(app, controller)?;
    app.state::<LifecycleController>()
        .synchronize_configuration_issue(status.configuration_issue().cloned());
    app.emit("engine-status", &status)
        .map_err(|error| error.to_string())?;
    schedule_tray_refresh(app, TrayState::from_status(&status));
    Ok(status)
}

fn current_lifecycle_status(
    app: &AppHandle,
    lifecycle: &LifecycleController,
) -> Result<LifecycleStatus, String> {
    let launch_at_login = app
        .autolaunch()
        .is_enabled()
        .map_err(|error| format!("读取开机启动状态失败：{error}"))?;
    Ok(lifecycle.status(launch_at_login))
}

fn emit_lifecycle_status(app: &AppHandle) -> Result<LifecycleStatus, String> {
    let lifecycle = app.state::<LifecycleController>();
    let status = current_lifecycle_status(app, &lifecycle)?;
    app.emit("lifecycle-status", &status)
        .map_err(|error| error.to_string())?;
    schedule_tray_refresh(app, app.state::<EngineController>().cached_tray_state());
    Ok(status)
}

pub(crate) fn emit_application_state(app: &AppHandle) -> Option<EngineStatus> {
    let controller = app.state::<EngineController>();
    let status = emit_status(app, &controller).ok();
    let _ = emit_lifecycle_status(app);
    status
}

/// The one Off-only notification per application run. The temporary probe hears speech without
/// activating virtual output, so the notice must not describe the configured degraded-output policy.
/// It cannot arrive if the microphone is muted or unplugged because there is nothing to hear.
fn send_dsp_override_not_applied_notice(app: &AppHandle) {
    if app
        .try_state::<tauri_plugin_notification::Notification<Wry>>()
        .is_none()
    {
        return;
    }
    let language = app
        .try_state::<TrayMenuState>()
        .map(|menu| UiLanguage::from_code(menu.language.load(Ordering::Relaxed)))
        .unwrap_or(UiLanguage::ZhCn);
    let (title, body) = match language {
        UiLanguage::ZhCn => (
            "DSP 覆盖未应用",
            "XMic 已在运行。请完全退出 XMic 后，再使用 --dsp-library 重试。",
        ),
        UiLanguage::En => (
            "DSP override was not applied",
            "XMic is already running. Fully exit XMic, then try --dsp-library again.",
        ),
    };
    let _ = app.notification().builder().title(title).body(body).show();
}

pub(crate) fn send_notification_probe_notice(app: &AppHandle) -> bool {
    let controller = app.state::<EngineController>();
    if !controller.take_notification_probe_notice() {
        return false;
    }
    let language = app
        .try_state::<TrayMenuState>()
        .map(|menu| UiLanguage::from_code(menu.language.load(Ordering::Relaxed)))
        .unwrap_or(UiLanguage::ZhCn);
    let (title, body) = match language {
        UiLanguage::ZhCn => (
            "XMic 当前已关闭",
            "检测到你正在说话。XMic 的虚拟输出未激活；如需回声消除和降噪，请在通话前打开 XMic。",
        ),
        UiLanguage::En => (
            "XMic is Off",
            "XMic detected speech, but its virtual output is inactive. Turn on XMic before a call to use echo cancellation and noise suppression.",
        ),
    };
    let _ = app.notification().builder().title(title).body(body).show();
    true
}

fn setup_tray(app: &mut tauri::App) -> Result<(), tauri::Error> {
    let show = MenuItem::with_id(app, MENU_SHOW, "打开 XMic", true, None::<&str>)?;
    let toggle_processing =
        MenuItem::with_id(app, MENU_TOGGLE_PROCESSING, "启动处理", true, None::<&str>)?;
    let processing_separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "退出 XMic", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&show, &toggle_processing, &processing_separator, &quit],
    )?;

    app.manage(TrayMenuState {
        language: AtomicU8::new(LANGUAGE_ZH_CN),
        rendered: Mutex::new(None),
        show: show.clone(),
        toggle_processing: toggle_processing.clone(),
        quit: quit.clone(),
    });

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("XMic - 已停止")
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_SHOW => show_main_window(app),
            MENU_TOGGLE_PROCESSING => spawn_tray_engine_toggle(app.clone()),
            MENU_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        let icons = TrayIcons::from_window_icon(icon);
        builder = builder.icon(icons.non_processed.clone());
        app.manage(icons);
    }
    builder.build(app)?;
    Ok(())
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        emit_window_visibility(app, true);
    }
    if let Some(issue) = app
        .try_state::<LifecycleController>()
        .and_then(|lifecycle| lifecycle.configuration_issue())
    {
        let _ = app.emit("navigate-to-configuration", issue);
    }
}

fn emit_window_visibility(app: &AppHandle, visible: bool) {
    let _ = app.emit(WINDOW_VISIBILITY_EVENT, visible);
}

/// Asked once on mount, because the events alone cannot describe the state the window started in:
/// with `start_in_tray` it is never shown at all, and nothing would have emitted anything.
#[tauri::command]
fn is_main_window_visible(app: AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        // If the window cannot say, keep polling — that is what happened before this existed.
        .unwrap_or(true)
}

fn spawn_tray_engine_toggle(app: AppHandle) {
    let _ = thread::Builder::new()
        .name("xmic-tray-action".to_owned())
        .spawn(move || {
            let controller = app.state::<EngineController>();
            let lifecycle = app.state::<LifecycleController>();
            if controller.start_requested() {
                let _ = lifecycle.stop_manually(&controller);
            } else if controller.cached_tray_state().settings_reset
                || lifecycle.configuration_issue().is_some()
            {
                show_main_window(&app);
            } else {
                let _ = lifecycle.start_manually(&controller);
            }
            emit_application_state(&app);
        });
}

fn schedule_tray_refresh(app: &AppHandle, tray: TrayState) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || refresh_tray(&handle, tray));
}

/// Draws the tray from a state the caller already has. Taking it as a parameter is what keeps the
/// main thread out of IPC: this runs on the main thread, and asking the helper here would freeze the
/// window for the connect timeout whenever the helper stops answering.
fn refresh_tray(app: &AppHandle, tray: TrayState) {
    let Some(menu) = app.try_state::<TrayMenuState>() else {
        return;
    };
    let lifecycle = app.state::<LifecycleController>();
    let configuration_required = lifecycle.configuration_issue().is_some();
    let available = lifecycle.is_session_available();
    let language = menu.language.load(Ordering::Relaxed);
    let labels = tray_labels(UiLanguage::from_code(language));
    let (label, enabled) = if tray.desired_enabled {
        (labels.stop_processing, true)
    } else if tray.settings_reset {
        (labels.apply_settings, true)
    } else if configuration_required {
        (labels.configure_microphone, available)
    } else {
        (labels.start_processing, available)
    };
    // Render from the public output contract rather than the configured degraded policy: Off and
    // Blocked both have inactive virtual output even if the policy is Raw.
    let tooltip = if tray.settings_reset {
        labels.settings_reset_tooltip
    } else if tray.retrying {
        labels.retrying_tooltip
    } else if configuration_required {
        labels.configuration_tooltip
    } else if tray.state == EngineState::Blocked {
        labels.blocked_tooltip
    } else {
        match tray.output {
            EngineOutput::Inactive => labels.off_tooltip,
            EngineOutput::Processed => labels.processing_tooltip,
            EngineOutput::Raw => labels.degraded_raw_tooltip,
            EngineOutput::Silence => labels.degraded_silent_tooltip,
        }
    };
    let rendered = RenderedTray {
        language,
        label,
        enabled,
        tooltip,
        processed: tray.processed,
    };
    {
        let mut last = menu
            .rendered
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *last == Some(rendered) {
            return;
        }
        *last = Some(rendered);
    }

    let _ = menu.show.set_text(labels.show);
    let _ = menu.toggle_processing.set_text(label);
    let _ = menu.toggle_processing.set_enabled(enabled);
    let _ = menu.quit.set_text(labels.quit);
    if let Some(tray_icon) = app.tray_by_id(TRAY_ID) {
        let _ = tray_icon.set_tooltip(Some(tooltip));
        if let Some(icons) = app.try_state::<TrayIcons>() {
            let _ = tray_icon.set_icon(Some(icons.select(rendered.processed).clone()));
        }
    }
}

fn create_main_window(app: &mut tauri::App, runtime: &RuntimeSession) -> Result<(), tauri::Error> {
    let config = app
        .config()
        .app
        .windows
        .first()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("main window configuration".into()))?;
    let mut builder = WebviewWindowBuilder::from_config(app.handle(), &config)?;
    if runtime.paths.mode == AppMode::Portable {
        if let Err(error) = fs::create_dir_all(&runtime.paths.webview_data_dir) {
            show_startup_error(
                &format!(
                    "Portable mode requires a writable WebView data directory at {}.\n\n{error}",
                    runtime.paths.webview_data_dir.display()
                ),
                false,
            );
            return Err(error.into());
        }
        builder = builder.data_directory(runtime.paths.webview_data_dir.clone());
    }
    if let Err(error) = builder.build() {
        show_startup_error(
            &format!(
                "XMic could not create its application window. Standalone and Portable builds require the system Microsoft Edge WebView2 Evergreen Runtime.\n\n{error}"
            ),
            true,
        );
        return Err(error);
    }
    Ok(())
}

fn show_startup_error(message: &str, offer_webview_download: bool) {
    #[cfg(target_os = "windows")]
    {
        let body = if offer_webview_download {
            format!("{message}\n\nOpen the official WebView2 download page?")
        } else {
            message.to_owned()
        };
        let confirmed = xmic_platform_windows::show_error_dialog(
            "XMic startup error",
            &body,
            offer_webview_download,
        );
        if offer_webview_download && confirmed {
            let _ = open_https_url(WEBVIEW2_DOWNLOAD_URL);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = offer_webview_download;
        eprintln!("{message}");
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeSelfTestReport {
    diagnostics: ApplicationDiagnostics,
    embedded_library_matches: bool,
    current_runtime_cleaned: bool,
}

pub fn run_runtime_self_test(report_path: &Path) -> Result<(), String> {
    let paths = discover_runtime_paths_from_environment()?;
    let runtime = prepare_runtime(paths, None)?;
    let embedded_library_matches = runtime.managed_dsp_matches_embedded();
    let controller = EngineController::new(
        runtime.helper_executable.clone(),
        runtime.selection.path.clone(),
        runtime.selection_guard_error(),
        runtime.paths.config_dir.clone(),
        false,
    )?;
    let validation = controller.query_dsp_validation_for_self_test();
    let diagnostics = application_diagnostics(&runtime, validation.clone());
    controller.shutdown();
    drop(controller);
    runtime.cleanup();
    let current_runtime_cleaned = !runtime.run_dir.exists();
    let report = RuntimeSelfTestReport {
        diagnostics,
        embedded_library_matches,
        current_runtime_cleaned,
    };
    let json = serde_json::to_vec_pretty(&report)
        .map_err(|error| format!("serialize runtime self-test report: {error}"))?;
    fs::write(report_path, json).map_err(|error| {
        format!(
            "write runtime self-test report {}: {error}",
            report_path.display()
        )
    })?;
    if !embedded_library_matches {
        return Err("managed WebRTC DSP does not match the bytes embedded in XMic".to_owned());
    }
    if !validation.is_valid() {
        return Err(format!(
            "selected DSP validation failed: {}",
            validation
                .error
                .unwrap_or_else(|| "unknown error".to_owned())
        ));
    }
    if !report.diagnostics.dsp_selection_pinned {
        return Err("selected DSP library was not pinned for this application run".to_owned());
    }
    if !current_runtime_cleaned {
        return Err(format!(
            "current runtime directory was not cleaned: {}",
            runtime.run_dir.display()
        ));
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> Result<(), String> {
    let runtime_paths = discover_runtime_paths_from_environment().inspect_err(|error| {
        show_startup_error(error, false);
    })?;
    let invocation_cwd = std::env::current_dir().map_err(|error| {
        let error = format!("resolve invocation working directory: {error}");
        show_startup_error(&error, false);
        error
    })?;
    let ui_arguments = parse_ui_arguments(std::env::args_os().skip(1), &invocation_cwd)
        .inspect_err(|error| {
            show_startup_error(error, false);
        })?;

    let builder = tauri::Builder::default()
        .manage(SecondaryInvocationState::default())
        // This remains the first plugin so secondary copies exit before setup creates or cleans any
        // runtime/data artifact. The fixed app identifier keeps all copies in one Windows-session scope.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            let override_requested = invocation_requests_dsp_override(&args);
            if override_requested {
                app.state::<SecondaryInvocationState>()
                    .override_warning_pending
                    .store(true, Ordering::Release);
                let _ = app.emit(DSP_OVERRIDE_WARNING_EVENT, ());
                let notice_app = app.clone();
                let _ = app
                    .run_on_main_thread(move || send_dsp_override_not_applied_notice(&notice_app));
            }
            if override_requested || !args.iter().any(|argument| argument == AUTOSTART_ARGUMENT) {
                if app.get_webview_window("main").is_none() {
                    app.state::<SecondaryInvocationState>()
                        .show_window_pending
                        .store(true, Ordering::Release);
                }
                show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(
            tauri_plugin_autostart::Builder::new()
                .arg(AUTOSTART_ARGUMENT)
                .build(),
        )
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                    emit_window_visibility(window.app_handle(), false);
                }
            }
        })
        .setup(move |app| {
            let runtime =
                match prepare_runtime(runtime_paths.clone(), ui_arguments.dsp_library.clone()) {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        show_startup_error(&error, false);
                        return Err(std::io::Error::other(error).into());
                    }
                };
            if let Err(error) = create_main_window(app, &runtime) {
                runtime.cleanup();
                return Err(error.into());
            }
            let config_dir = runtime.paths.config_dir.clone();
            let lifecycle = LifecycleController::new(LifecyclePreferencesStore::new(
                config_dir.join("app-preferences.json"),
            ));
            let controller = EngineController::new(
                runtime.helper_executable.clone(),
                runtime.selection.path.clone(),
                runtime.selection_guard_error(),
                config_dir.clone(),
                lifecycle.start_engine_on_launch(),
            )
            .map_err(std::io::Error::other)?;
            app.manage(runtime);
            app.manage(controller);
            app.manage(lifecycle);
            // Constructing it registers the endpoint notification client the cache invalidates
            // against, so it costs a permanent thread and a short wait here rather than on the first
            // status poll. It enumerates nothing until something asks.
            app.manage(EndpointCache::new());
            setup_tray(app)?;
            // Publishes initial Windows-session availability and grants only the temporary Off-time
            // capture probe permission to open a microphone.
            {
                let lifecycle = app.state::<LifecycleController>();
                let controller = app.state::<EngineController>();
                lifecycle.refresh_engine_availability(&controller);
            }

            match start_system_lifecycle_monitor() {
                Ok(monitor) => {
                    let background = BackgroundLifecycle::start(app.handle().clone(), monitor)
                        .map_err(std::io::Error::other)?;
                    app.manage(background);
                }
                Err(error) => {
                    let lifecycle = app.state::<LifecycleController>();
                    let controller = app.state::<EngineController>();
                    lifecycle.monitor_failed(&controller, error.to_string());
                }
            }

            let secondary_show_requested = app
                .state::<SecondaryInvocationState>()
                .show_window_pending
                .swap(false, Ordering::AcqRel);
            if secondary_show_requested || !app.state::<LifecycleController>().start_in_tray() {
                show_main_window(app.handle());
            }
            refresh_tray(
                app.handle(),
                app.state::<EngineController>().cached_tray_state(),
            );
            Ok(())
        });

    let app = builder
        .invoke_handler(tauri::generate_handler![
            get_bootstrap_status,
            get_engine_status,
            get_lifecycle_status,
            is_main_window_visible,
            set_ui_language,
            open_vb_cable_download,
            open_external_link,
            get_application_diagnostics,
            take_pending_dsp_override_warning,
            list_audio_endpoints,
            update_lifecycle_preferences,
            update_engine_settings,
            start_engine,
            stop_engine
        ])
        .build(tauri::generate_context!())
        .map_err(|error| format!("build Tauri application: {error}"))?;

    app.run(|app, event| {
        if matches!(event, RunEvent::ExitRequested { .. }) {
            if let Some(background) = app.try_state::<BackgroundLifecycle>() {
                background.shutdown();
            }
            let lifecycle = app.state::<LifecycleController>();
            let controller = app.state::<EngineController>();
            lifecycle.shutdown(&controller);
            controller.shutdown();
            if let Some(runtime) = app.try_state::<RuntimeSession>() {
                runtime.cleanup();
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secondary_dsp_override_invocations_are_detected_in_both_cli_forms() {
        assert!(invocation_requests_dsp_override(&[
            "XMic.exe".to_owned(),
            "--dsp-library".to_owned(),
            "custom.dll".to_owned(),
        ]));
        assert!(invocation_requests_dsp_override(&[
            "XMic.exe".to_owned(),
            "--dsp-library=custom.dll".to_owned(),
        ]));
        assert!(!invocation_requests_dsp_override(&[
            "XMic.exe".to_owned(),
            "--autostart".to_owned(),
        ]));
    }

    #[test]
    fn configured_main_window_is_deferred_until_its_data_path_is_known() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(config["identifier"], runtime::APP_IDENTIFIER);
        assert_eq!(config["app"]["windows"][0]["title"], "XMic");
        assert_eq!(config["app"]["windows"][0]["create"], false);
    }

    #[test]
    fn the_non_processed_icon_keeps_the_shape_and_drops_the_colour() {
        // Two pixels of the real icon: the green microphone and the transparent corner.
        let green = [0x2f, 0xb5, 0x7e, 0xff];
        let transparent = [0x00, 0x00, 0x00, 0x00];
        let mut rgba = green.to_vec();
        rgba.extend_from_slice(&transparent);

        let desaturated = desaturate_rgba(&rgba);

        assert_eq!(desaturated.len(), rgba.len());
        assert_eq!(desaturated[0], desaturated[1]);
        assert_eq!(desaturated[1], desaturated[2]);
        // Alpha decides the shape, so it has to survive untouched.
        assert_eq!(desaturated[3], 0xff);
        assert_eq!(desaturated[7], 0x00);
    }
}
