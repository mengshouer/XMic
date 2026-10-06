use windows::{
    core::PCWSTR,
    Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_ICONERROR, MB_OK, MB_YESNO},
};

/// Shows a native error dialog, returning true only if the user answers Yes.
pub fn show_error_dialog(title: &str, message: &str, ask_confirmation: bool) -> bool {
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let style = MB_ICONERROR | if ask_confirmation { MB_YESNO } else { MB_OK };
    // Both terminated buffers remain alive until the modal dialog returns.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR::from_raw(message.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            style,
        ) == IDYES
    }
}
