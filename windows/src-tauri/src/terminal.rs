// A session's terminal window: bring it to the front, and tell whether it is still
// there. The relay finds the window (coucou-hook's `find_terminal`) and sends its
// handle and process id with the session's events; the island hands them back here.
//
// A window handle can outlive its window and be handed to a different one later,
// so every call checks that the handle still belongs to the process it was found
// under. A stale handle then means "no", never "some other application".

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VK_MENU};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowThreadProcessId, IsIconic, IsWindow, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

fn handle(raw: i64) -> HWND {
    HWND(raw as isize as *mut _)
}

/// True when `raw` is still a window and still belongs to process `pid`.
pub fn alive(raw: i64, pid: u32) -> bool {
    if raw == 0 || pid == 0 {
        return false;
    }
    let h = handle(raw);
    unsafe {
        if !IsWindow(Some(h)).as_bool() {
            return false;
        }
        let mut owner = 0u32;
        GetWindowThreadProcessId(h, Some(&mut owner));
        owner == pid
    }
}

/// Brings the window to the front, restoring it first if it is minimised.
/// False if it is gone or is no longer the window we found.
pub fn focus(raw: i64, pid: u32) -> bool {
    if !alive(raw, pid) {
        return false;
    }
    let h = handle(raw);
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        if SetForegroundWindow(h).as_bool() {
            return true;
        }
        // Windows only lets a process change the foreground window if it has just
        // had input. A click on the island counts, but not always; a lone Alt press
        // is the usual way to make it count, and is invisible.
        keybd_event(VK_MENU.0 as u8, 0, KEYBD_EVENT_FLAGS(0), 0);
        keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
        SetForegroundWindow(h).as_bool()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_alive_without_a_handle_and_a_process() {
        assert!(!alive(0, 0));
        assert!(!alive(0, 123));
        assert!(!alive(0x1234, 0));
    }

    #[test]
    fn a_handle_that_is_not_a_window_is_never_focused() {
        // 0x7FFF_FFF0 is not a window on any machine this will run on.
        assert!(!alive(0x7FFF_FFF0, 4));
        assert!(!focus(0x7FFF_FFF0, 4));
    }
}
