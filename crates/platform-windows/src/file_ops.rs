use std::{io, os::windows::ffi::OsStrExt, path::Path};

use windows::{
    core::PCWSTR,
    Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH},
};

pub fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    // Both terminated buffers remain alive for the synchronous Win32 call.
    unsafe {
        MoveFileExW(
            PCWSTR::from_raw(source.as_ptr()),
            PCWSTR::from_raw(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(io::Error::other)
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains a null character",
        ));
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn atomic_replace_handles_unicode_and_preserves_destination_on_failure() {
        static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "xmic-file-replace-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&directory).unwrap();
        let source = directory.join("source-\u{97f3}.tmp");
        let destination = directory.join("destination-\u{58f0}.dll");
        fs::write(&source, b"replacement").unwrap();
        fs::write(&destination, b"original").unwrap();

        atomic_replace(&source, &destination).unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"replacement");
        assert!(atomic_replace(&source, &destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"replacement");
        fs::write(&source, b"new file").unwrap();
        let new_destination = directory.join("new.dll");
        atomic_replace(&source, &new_destination).unwrap();
        assert_eq!(fs::read(&new_destination).unwrap(), b"new file");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn atomic_replace_rejects_embedded_null_paths() {
        let error =
            atomic_replace(Path::new("source\0suffix"), Path::new("destination")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
