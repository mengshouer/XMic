use std::{
    ffi::c_void,
    sync::{
        atomic::{AtomicBool, AtomicIsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
        Arc, OnceLock,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{
            GetLastError, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            RemoteDesktop::{
                WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
                NOTIFY_FOR_THIS_SESSION,
            },
        },
        UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
            GetWindowLongPtrW, PostMessageW, PostQuitMessage, RegisterClassW, SetWindowLongPtrW,
            TranslateMessage, CREATESTRUCTW, GWLP_USERDATA, MSG, PBT_APMRESUMEAUTOMATIC,
            PBT_APMRESUMECRITICAL, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, WINDOW_EX_STYLE, WM_CLOSE,
            WM_DESTROY, WM_NCCREATE, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WNDCLASSW,
            WS_OVERLAPPED, WTS_CONSOLE_CONNECT, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_CONNECT,
            WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_LOGOFF, WTS_SESSION_UNLOCK,
        },
    },
};
use xmic_platform_api::{PlatformError, SystemLifecycleEvent};

use crate::{join_background_worker, operation_error, WORKER_START_TIMEOUT};

const EVENT_QUEUE_CAPACITY: usize = 32;
const WINDOW_CLASS_NAME: PCWSTR = w!("XMicSystemLifecycleMonitor");

pub struct SystemLifecycleMonitor {
    events: Receiver<SystemLifecycleEvent>,
    window_handle: Arc<AtomicIsize>,
    stop_requested: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<(), PlatformError>>>,
}

impl SystemLifecycleMonitor {
    pub fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<SystemLifecycleEvent>, PlatformError> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => Ok(Some(event)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(PlatformError::WorkerStopped),
        }
    }

    pub fn stop(mut self) -> Result<(), PlatformError> {
        self.request_stop();
        self.join_background_worker()
    }

    fn request_stop(&self) {
        request_background_stop(&self.window_handle, &self.stop_requested);
    }

    fn join_background_worker(&mut self) -> Result<(), PlatformError> {
        self.worker.take().map_or(Ok(()), |worker| {
            join_background_worker(worker, "system lifecycle monitor")
        })
    }
}

impl Drop for SystemLifecycleMonitor {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.join_background_worker();
    }
}

pub fn start_system_lifecycle_monitor() -> Result<SystemLifecycleMonitor, PlatformError> {
    let (event_tx, event_rx) = mpsc::sync_channel(EVENT_QUEUE_CAPACITY);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let window_handle = Arc::new(AtomicIsize::new(0));
    let stop_requested = Arc::new(AtomicBool::new(false));
    let worker_handle = Arc::clone(&window_handle);
    let worker_stop = Arc::clone(&stop_requested);
    let worker = thread::Builder::new()
        .name("xmic-system-lifecycle".to_owned())
        .spawn(move || {
            let result = run_monitor(event_tx, worker_handle, &worker_stop, &ready_tx);
            if let Err(error) = &result {
                let _ = ready_tx.send(Err(error.to_string()));
            }
            result
        })
        .map_err(|error| PlatformError::Operation(format!("start lifecycle monitor: {error}")))?;

    match ready_rx.recv_timeout(WORKER_START_TIMEOUT) {
        Ok(Ok(())) => Ok(SystemLifecycleMonitor {
            events: event_rx,
            window_handle,
            stop_requested,
            worker: Some(worker),
        }),
        Ok(Err(error)) => {
            stop_failed_start(&window_handle, &stop_requested, worker);
            Err(PlatformError::Operation(error))
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            stop_failed_start(&window_handle, &stop_requested, worker);
            Err(PlatformError::Operation(
                "system lifecycle monitor preparation timed out after 5 seconds".to_owned(),
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            stop_failed_start(&window_handle, &stop_requested, worker);
            Err(PlatformError::WorkerStopped)
        }
    }
}

fn request_window_close(window_handle: &AtomicIsize) {
    let raw_handle = window_handle.load(Ordering::Acquire);
    if raw_handle != 0 {
        let window = HWND(raw_handle as *mut c_void);
        let _ = unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
}

fn stop_failed_start(
    window_handle: &AtomicIsize,
    stop_requested: &AtomicBool,
    worker: JoinHandle<Result<(), PlatformError>>,
) {
    request_background_stop(window_handle, stop_requested);
    let _ = join_background_worker(worker, "failed system lifecycle monitor startup");
}

fn request_background_stop(window_handle: &AtomicIsize, stop_requested: &AtomicBool) {
    stop_requested.store(true, Ordering::Release);
    request_window_close(window_handle);
}

struct WindowContext {
    events: SyncSender<SystemLifecycleEvent>,
}

fn run_monitor(
    events: SyncSender<SystemLifecycleEvent>,
    window_handle: Arc<AtomicIsize>,
    stop_requested: &AtomicBool,
    ready: &SyncSender<Result<(), String>>,
) -> Result<(), PlatformError> {
    register_window_class()?;
    let module = unsafe { GetModuleHandleW(None) }
        .map_err(|error| operation_error("get lifecycle monitor module handle", error))?;
    let context = Box::new(WindowContext { events });
    let context_pointer = (&*context as *const WindowContext).cast::<c_void>();
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            WINDOW_CLASS_NAME,
            w!("XMic lifecycle monitor"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(HINSTANCE(module.0)),
            Some(context_pointer),
        )
    }
    .map_err(|error| operation_error("create lifecycle monitor window", error))?;
    window_handle.store(window.0 as isize, Ordering::Release);

    if let Err(error) = unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) } {
        window_handle.store(0, Ordering::Release);
        let _ = unsafe { DestroyWindow(window) };
        return Err(operation_error(
            "register Windows session lifecycle notifications",
            error,
        ));
    }

    if stop_requested.load(Ordering::Acquire) {
        let _ = unsafe { WTSUnRegisterSessionNotification(window) };
        let _ = unsafe { DestroyWindow(window) };
        window_handle.store(0, Ordering::Release);
        return Ok(());
    }

    let _ = ready.send(Ok(()));
    let result = run_message_loop();
    let _ = unsafe { WTSUnRegisterSessionNotification(window) };
    if unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(window)) }.as_bool() {
        let _ = unsafe { DestroyWindow(window) };
    }
    window_handle.store(0, Ordering::Release);
    drop(context);
    result
}

fn register_window_class() -> Result<(), PlatformError> {
    static REGISTRATION: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTRATION
        .get_or_init(|| {
            let module = unsafe { GetModuleHandleW(None) }.map_err(|error| error.to_string())?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: HINSTANCE(module.0),
                lpszClassName: WINDOW_CLASS_NAME,
                ..Default::default()
            };
            let atom = unsafe { RegisterClassW(&class) };
            if atom != 0 || unsafe { GetLastError() } == ERROR_CLASS_ALREADY_EXISTS {
                Ok(())
            } else {
                Err(windows::core::Error::from_win32().to_string())
            }
        })
        .clone()
        .map_err(|error| {
            PlatformError::Operation(format!("register lifecycle monitor window class: {error}"))
        })
}

fn run_message_loop() -> Result<(), PlatformError> {
    let mut message = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if result.0 == -1 {
            return Err(operation_error(
                "read lifecycle monitor window message",
                windows::core::Error::from_win32(),
            ));
        }
        if !result.as_bool() {
            return Ok(());
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }

    let context = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const WindowContext;
    if !context.is_null() {
        if let Some(event) = classify_message(message, wparam.0) {
            let _ = unsafe { &*context }.events.try_send(event);
        }
    }

    match message {
        WM_CLOSE => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

fn classify_message(message: u32, event: usize) -> Option<SystemLifecycleEvent> {
    match (message, event as u32) {
        (WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK) => Some(SystemLifecycleEvent::SessionLocked),
        (WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK) => Some(SystemLifecycleEvent::SessionUnlocked),
        (WM_WTSSESSION_CHANGE, WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT) => {
            Some(SystemLifecycleEvent::SessionDisconnected)
        }
        (WM_WTSSESSION_CHANGE, WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT) => {
            Some(SystemLifecycleEvent::SessionConnected)
        }
        (WM_WTSSESSION_CHANGE, WTS_SESSION_LOGOFF) => Some(SystemLifecycleEvent::SessionLoggedOff),
        (WM_POWERBROADCAST, PBT_APMSUSPEND) => Some(SystemLifecycleEvent::Suspending),
        (
            WM_POWERBROADCAST,
            PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND | PBT_APMRESUMECRITICAL,
        ) => Some(SystemLifecycleEvent::Resumed),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_session_and_power_messages() {
        assert_eq!(
            classify_message(WM_WTSSESSION_CHANGE, WTS_SESSION_LOCK as usize),
            Some(SystemLifecycleEvent::SessionLocked)
        );
        assert_eq!(
            classify_message(WM_WTSSESSION_CHANGE, WTS_REMOTE_DISCONNECT as usize),
            Some(SystemLifecycleEvent::SessionDisconnected)
        );
        assert_eq!(
            classify_message(WM_POWERBROADCAST, PBT_APMSUSPEND as usize),
            Some(SystemLifecycleEvent::Suspending)
        );
        assert_eq!(classify_message(WM_CLOSE, 0), None);
    }

    #[test]
    fn failed_start_records_stop_even_before_a_window_exists() {
        let handle = AtomicIsize::new(0);
        let stop_requested = AtomicBool::new(false);

        request_background_stop(&handle, &stop_requested);

        assert!(stop_requested.load(Ordering::Acquire));
    }

    #[test]
    fn monitor_starts_and_stops_cleanly() {
        start_system_lifecycle_monitor().unwrap().stop().unwrap();
    }
}
