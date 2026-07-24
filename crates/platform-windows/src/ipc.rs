use std::{
    fs::{File, OpenOptions},
    os::windows::io::{FromRawHandle, RawHandle},
    thread,
    time::{Duration, Instant},
};

use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_PIPE_CONNECTED, WAIT_FAILED},
        Storage::FileSystem::PIPE_ACCESS_DUPLEX,
        System::{
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW, PIPE_READMODE_BYTE,
                PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
            },
            Threading::{OpenProcess, WaitForSingleObject, INFINITE, PROCESS_SYNCHRONIZE},
        },
    },
};
use xmic_platform_api::PlatformError;

const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

pub struct LocalIpcServer {
    path: String,
}

impl LocalIpcServer {
    pub fn bind(name: &str) -> Result<Self, PlatformError> {
        validate_pipe_name(name)?;
        Ok(Self {
            path: pipe_path(name),
        })
    }

    pub fn accept(&self) -> Result<File, PlatformError> {
        let wide = wide_null(&self.path);
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                0,
                None,
            )
        };
        if handle.is_invalid() {
            return Err(last_operation_error("create local IPC named pipe"));
        }
        let connected = unsafe { ConnectNamedPipe(handle, None) };
        if connected.is_err() && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
            unsafe {
                drop(File::from_raw_handle(handle.0 as RawHandle));
            }
            return Err(last_operation_error("accept local IPC named pipe client"));
        }
        Ok(unsafe { File::from_raw_handle(handle.0 as RawHandle) })
    }
}

pub fn connect_local_ipc(name: &str, timeout: Duration) -> Result<File, PlatformError> {
    validate_pipe_name(name)?;
    let path = pipe_path(name);
    let wide = wide_null(&path);
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout_ms = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX);
        if unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), timeout_ms) }.as_bool() {
            match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(stream) => return Ok(stream),
                Err(error) if Instant::now() >= deadline => {
                    return Err(PlatformError::Operation(format!(
                        "open local IPC named pipe: {error}"
                    )));
                }
                Err(_) => {}
            }
        } else if Instant::now() >= deadline {
            return Err(last_operation_error("wait for local IPC named pipe"));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub fn wait_for_process_exit(process_id: u32) -> Result<(), PlatformError> {
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, process_id) }
        .map_err(|error| PlatformError::Operation(format!("open parent process: {error}")))?;
    let wait_result = unsafe { WaitForSingleObject(process, INFINITE) };
    let close_result = unsafe { CloseHandle(process) };
    close_result.map_err(|error| {
        PlatformError::Operation(format!("close parent process handle: {error}"))
    })?;
    if wait_result == WAIT_FAILED {
        Err(last_operation_error("wait for parent process exit"))
    } else {
        Ok(())
    }
}

fn validate_pipe_name(name: &str) -> Result<(), PlatformError> {
    if !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(())
    } else {
        Err(PlatformError::Operation(
            "local IPC pipe name is invalid".to_owned(),
        ))
    }
}

fn pipe_path(name: &str) -> String {
    format!(r"\\.\pipe\{name}")
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn last_operation_error(context: &str) -> PlatformError {
    PlatformError::Operation(format!("{context}: {}", std::io::Error::last_os_error()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn named_pipe_round_trip_is_local_and_duplex() {
        let name = format!("xmic-ipc-test-{}", std::process::id());
        let server = LocalIpcServer::bind(&name).unwrap();
        let worker = std::thread::spawn(move || {
            let mut stream = server.accept().unwrap();
            let mut request = [0u8; 4];
            stream.read_exact(&mut request).unwrap();
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong").unwrap();
        });

        let mut client = connect_local_ipc(&name, Duration::from_secs(2)).unwrap();
        client.write_all(b"ping").unwrap();
        let mut response = [0u8; 4];
        client.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"pong");
        worker.join().unwrap();
    }
}
