//! The little bit of Win32 the relay needs: who we are, who is on the other end of
//! the pipe, and which window the terminal we run in lives in.
//!
//! Named pipes live in a machine-wide namespace, so `\\.\pipe\coucou-<name>` can
//! be created by *any* account that gets there first. Two defences, both cheap:
//! the pipe name carries our SID, and once connected we check the server process
//! really belongs to us before sending anything.

use std::collections::HashMap;

use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, HWND, LPARAM, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowThreadProcessId,
    IsWindowVisible, GWL_EXSTYLE, GW_OWNER, WS_EX_TOOLWINDOW,
};

/// The SID of the account this process runs as, as `S-1-5-21-…`.
pub fn current_user_sid() -> Option<String> {
    unsafe { token_sid(GetCurrentProcess()) }
}

/// True when the process serving `handle` runs as the same user we do.
///
/// A failure to answer is treated as "not ours": refusing to talk to a pipe we
/// cannot vouch for costs one hook event, while trusting it could hand another
/// account on this machine the contents of every tool call.
pub fn pipe_server_is_same_user(handle: HANDLE) -> bool {
    let Some(mine) = current_user_sid() else { return false };
    unsafe {
        let mut pid = 0u32;
        if GetNamedPipeServerProcessId(handle, &mut pid).is_err() || pid == 0 {
            return false;
        }
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let theirs = token_sid(process);
        let _ = CloseHandle(process);
        theirs.as_deref() == Some(mine.as_str())
    }
}

/// The user SID behind a process handle. `process` is borrowed, never closed.
unsafe fn token_sid(process: HANDLE) -> Option<String> {
    let mut token = HANDLE::default();
    OpenProcessToken(process, TOKEN_QUERY, &mut token).ok()?;

    // First call sizes the buffer, second fills it.
    let mut needed = 0u32;
    let _ = GetTokenInformation(token, TokenUser, None, 0, &mut needed);
    if needed == 0 {
        let _ = CloseHandle(token);
        return None;
    }
    let mut buf = vec![0u8; needed as usize];
    let ok = GetTokenInformation(
        token,
        TokenUser,
        Some(buf.as_mut_ptr().cast()),
        needed,
        &mut needed,
    )
    .is_ok();
    let _ = CloseHandle(token);
    if !ok {
        return None;
    }

    let user = &*(buf.as_ptr() as *const TOKEN_USER);
    let mut text = PWSTR::null();
    ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
    let sid = text.to_string().ok();
    let _ = LocalFree(Some(HLOCAL(text.0 as *mut _)));
    sid
}

// ── Which window is our terminal? ─────────────────────────────────────────────

/// The window the session's terminal lives in, as far as the process tree can tell.
#[derive(Debug, Clone, PartialEq)]
pub struct Terminal {
    pub hwnd: isize,
    pub pid: u32,
    /// Lower-case image name, e.g. `windowsterminal.exe`, `code.exe`.
    pub exe: String,
}

/// Processes that belong to the desktop, not to any terminal. Climbing into one
/// means the session has no window of its own to jump to, and the shell's windows
/// (the taskbar, the desktop) must never be offered as one.
const DESKTOP_PROCESSES: &[&str] = &[
    "explorer.exe", "svchost.exe", "services.exe", "wininit.exe", "winlogon.exe",
    "csrss.exe", "smss.exe", "system", "system idle process", "sihost.exe",
    "runtimebroker.exe", "taskhostw.exe",
];

/// A parent chain is a few links long; this only keeps a recycled PID that points
/// back into the chain from looping forever.
const MAX_DEPTH: usize = 16;

/// First process, going up from `start`, that owns a window. `parents` maps a pid
/// to (parent pid, lower-case image name); `windows` maps a pid to its main window.
///
/// Pure on purpose: the process list and the window list are the parts that need
/// Windows, and this is the part that can be wrong.
pub fn nearest_window_owner(
    start: u32,
    parents: &HashMap<u32, (u32, String)>,
    windows: &HashMap<u32, isize>,
) -> Option<(u32, isize, String)> {
    let mut pid = start;
    for _ in 0..MAX_DEPTH {
        let (parent, exe) = parents.get(&pid)?;
        if DESKTOP_PROCESSES.contains(&exe.as_str()) {
            return None;
        }
        if let Some(&hwnd) = windows.get(&pid) {
            return Some((pid, hwnd, exe.clone()));
        }
        if *parent == 0 || *parent == pid {
            return None;
        }
        pid = *parent;
    }
    None
}

/// The terminal window this relay was started from: the window of the nearest
/// ancestor that has one. Windows Terminal, the VS Code / Antigravity window, a
/// console host, the Codex desktop app — whichever the session runs under.
/// None if there is no such ancestor; the caller simply leaves the field out.
pub fn find_terminal() -> Option<Terminal> {
    let parents = process_parents()?;
    let me = std::process::id();
    let start = parents.get(&me)?.0;
    let windows = top_level_windows();
    nearest_window_owner(start, &parents, &windows)
        .map(|(pid, hwnd, exe)| Terminal { hwnd, pid, exe })
}

/// Every running process: pid → (parent pid, lower-case image name).
fn process_parents() -> Option<HashMap<u32, (u32, String)>> {
    let mut out = HashMap::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more && out.len() < 8192 {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let exe = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
            out.insert(entry.th32ProcessID, (entry.th32ParentProcessID, exe));
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    Some(out)
}

/// pid → its topmost real application window: visible, not owned by another
/// window, not a tool window, and with a title.
fn top_level_windows() -> HashMap<u32, isize> {
    let mut found: Vec<(u32, isize)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect_window), LPARAM(&mut found as *mut _ as isize));
    }
    // EnumWindows walks the z-order top-down, so the first window seen for a pid
    // is the one nearest the front.
    let mut by_pid = HashMap::new();
    for (pid, hwnd) in found {
        by_pid.entry(pid).or_insert(hwnd);
    }
    by_pid
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = &mut *(lparam.0 as *mut Vec<(u32, isize)>);
    let visible = IsWindowVisible(hwnd).as_bool();
    let owned = GetWindow(hwnd, GW_OWNER).is_ok();
    let tool = (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0) != 0;
    let titled = GetWindowTextLengthW(hwnd) > 0;
    if visible && !owned && !tool && titled {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0 {
            found.push((pid, hwnd.0 as isize));
        }
    }
    true.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(chain: &[(u32, u32, &str)]) -> HashMap<u32, (u32, String)> {
        chain.iter().map(|(p, pp, exe)| (*p, (*pp, exe.to_string()))).collect()
    }

    #[test]
    fn climbs_from_the_hook_to_the_terminal_that_has_a_window() {
        // coucou-hook ← powershell ← claude(node) ← pwsh ← WindowsTerminal
        let parents = tree(&[
            (10, 20, "coucou-hook.exe"),
            (20, 30, "powershell.exe"),
            (30, 40, "node.exe"),
            (40, 50, "pwsh.exe"),
            (50, 60, "windowsterminal.exe"),
            (60, 1, "explorer.exe"),
        ]);
        let windows = HashMap::from([(50u32, 0x1234isize), (60, 0x99)]);
        // Start from the hook's parent, as find_terminal does.
        assert_eq!(
            nearest_window_owner(20, &parents, &windows),
            Some((50, 0x1234, "windowsterminal.exe".to_string()))
        );
    }

    #[test]
    fn the_nearest_window_wins_over_a_farther_one() {
        // A shell with its own window inside an editor that also has one.
        let parents = tree(&[(20, 30, "bash.exe"), (30, 40, "code.exe"), (40, 1, "x.exe")]);
        let windows = HashMap::from([(30u32, 0xAAisize), (40, 0xBB)]);
        assert_eq!(nearest_window_owner(20, &parents, &windows).unwrap().1, 0xAA);
    }

    #[test]
    fn never_offers_the_desktop_as_a_terminal() {
        // A console app in a classic conhost: its window belongs to conhost, which is
        // not an ancestor, so the climb reaches explorer — which must not count.
        let parents = tree(&[(20, 30, "cmd.exe"), (30, 40, "explorer.exe"), (40, 1, "x.exe")]);
        let windows = HashMap::from([(30u32, 0x1isize), (40, 0x2)]);
        assert_eq!(nearest_window_owner(20, &parents, &windows), None);
    }

    #[test]
    fn a_tree_with_no_window_anywhere_is_none() {
        let parents = tree(&[(20, 30, "a.exe"), (30, 0, "b.exe")]);
        assert_eq!(nearest_window_owner(20, &parents, &HashMap::new()), None);
    }

    #[test]
    fn a_recycled_pid_that_loops_back_cannot_hang_the_climb() {
        let parents = tree(&[(20, 30, "a.exe"), (30, 20, "b.exe")]);
        assert_eq!(nearest_window_owner(20, &parents, &HashMap::new()), None);
    }

    #[test]
    fn a_pid_missing_from_the_snapshot_ends_the_climb() {
        // The parent exited between the snapshot and now.
        let parents = tree(&[(20, 99, "a.exe")]);
        assert_eq!(nearest_window_owner(20, &parents, &HashMap::new()), None);
    }

    /// Not run by default: asks the real machine, so it needs a real terminal around it.
    /// `cargo test -p coucou-hook this_machine -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn this_machine() {
        let t = find_terminal();
        println!("find_terminal() = {t:?}");
        if let Some(t) = &t {
            assert!(t.hwnd != 0 && t.pid != 0 && !t.exe.is_empty());
        }
    }
}
