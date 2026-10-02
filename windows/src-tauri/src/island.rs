// Island window: placement on the chosen display, the two window sizes
// (full panel / invisible wake strip), click-through and the cursor poll.
//
// There is no notch on a PC, so the island is a black shape drawn at the top
// centre of the main display inside a borderless, transparent, always-on-top
// window that never takes focus.
//
// The user can drag it elsewhere (Settings::dock): onto the left or right edge,
// where it becomes a vertical strip, or anywhere else, where it stays a
// horizontal pill. The window is always the same 720×320 canvas; only where it
// sits, and where the island hangs inside it (the "anchor"), changes. The drag
// itself runs in the cursor poll below, which already reads the mouse every frame.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Monitor, PhysicalPosition, PhysicalSize, WebviewWindow};

use windows::Win32::Foundation::{HWND, POINT};
use windows::core::BOOL;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::System::Ole::RevokeDragDrop;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW,
};

/// Logical size of the full window — the largest island view, like the macOS panel.
pub const PANEL_W: f64 = 720.0;
pub const PANEL_H: f64 = 320.0;
/// Logical size of the invisible strip that wakes the island when it is hidden.
pub const STRIP_W: f64 = 240.0;
pub const STRIP_H: f64 = 6.0;

pub const WINDOW_LABEL: &str = "island";

/// Margin around the island that still counts as "on the island", in logical px.
/// Wider than the macOS 6 pt because a click must never be swallowed.
const HIT_MARGIN: f64 = 14.0;

/// How close to a screen edge a drop has to land to dock there, in logical px.
pub const SNAP: f64 = 56.0;
/// Dropped this close to the middle of the top edge, the island goes back to exactly
/// centred instead of a few pixels off.
pub const CENTER_SNAP: f64 = 24.0;

/// Where on the screen the island lives. See `Settings::dock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dock {
    Top,
    Left,
    Right,
    Free,
}

impl Dock {
    /// Anything unrecognised — a hand-edited or older settings.json — is the notch spot.
    pub fn parse(name: &str) -> Dock {
        match name {
            "left" => Dock::Left,
            "right" => Dock::Right,
            "free" => Dock::Free,
            _ => Dock::Top,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Dock::Top => "top",
            Dock::Left => "left",
            Dock::Right => "right",
            Dock::Free => "free",
        }
    }
}

/// A rectangle in logical px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Where the window goes (logical screen px) and where the island hangs inside it
/// (logical window px): top and free → the island's top-centre, left → its
/// left-centre, right → its right-centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    pub win: Rect,
    pub anchor_x: f64,
    pub anchor_y: f64,
}

/// What the front end needs to draw the island in the right spot.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Placement {
    pub dock: String,
    pub anchor_x: f64,
    pub anchor_y: f64,
}

impl Default for Placement {
    fn default() -> Self {
        Placement { dock: "top".into(), anchor_x: PANEL_W / 2.0, anchor_y: 0.0 }
    }
}

/// `v` limited to `lo..=hi`; a display too small for the window gives `lo`.
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    v.min(hi).max(lo)
}

/// Where the 720×320 window (or the wake strip, when `collapsed`) goes for a dock.
/// `dx`/`dy` are `Settings::dock_x` / `dock_y`.
///
/// The window is kept inside the display, and the island's anchor absorbs
/// whatever the clamp moved: an island dropped near a corner stays exactly where
/// it was dropped while its window slides inwards.
pub fn window_geometry(mon: Rect, dock: Dock, dx: f64, dy: f64, collapsed: bool) -> Geometry {
    match dock {
        Dock::Top => {
            let (w, h) = if collapsed { (STRIP_W, STRIP_H) } else { (PANEL_W, PANEL_H) };
            // `dx` is where the island's centre sits along the edge; 0 — the default,
            // and what an older settings.json holds — is the middle of the display.
            let centre = if dx > 0.0 { dx } else { mon.w / 2.0 };
            let x = clamp(mon.x + centre - w / 2.0, mon.x, mon.x + mon.w - w);
            Geometry {
                win: Rect { x, y: mon.y, w, h },
                anchor_x: mon.x + centre - x,
                anchor_y: 0.0,
            }
        }
        Dock::Left | Dock::Right => {
            // Hidden, it is a vertical strip on the edge instead of a horizontal one.
            let (w, h) = if collapsed { (STRIP_H, STRIP_W) } else { (PANEL_W, PANEL_H) };
            let centre = mon.y + dy;
            let y = clamp(centre - h / 2.0, mon.y, mon.y + mon.h - h);
            let (x, anchor_x) = if dock == Dock::Left {
                (mon.x, 0.0)
            } else {
                (mon.x + mon.w - w, w)
            };
            Geometry { win: Rect { x, y, w, h }, anchor_x, anchor_y: centre - y }
        }
        // A free island never hides, so there is no collapsed form to size.
        Dock::Free => {
            let (w, h) = (PANEL_W, PANEL_H);
            let x = clamp(mon.x + dx - w / 2.0, mon.x, mon.x + mon.w - w);
            let y = clamp(mon.y + dy, mon.y, mon.y + mon.h - h);
            Geometry {
                win: Rect { x, y, w, h },
                anchor_x: mon.x + dx - x,
                anchor_y: mon.y + dy - y,
            }
        }
    }
}

/// Where a drop leaves the island. `island` is its rectangle on screen at the
/// moment of release; the result is the dock plus `dock_x` / `dock_y`.
///
/// Sides win over the top: a drop in a corner is a request for that edge.
pub fn classify_drop(mon: Rect, island: Rect) -> (Dock, f64, f64) {
    let to_left = island.x - mon.x;
    let to_right = (mon.x + mon.w) - (island.x + island.w);
    let to_top = island.y - mon.y;

    if to_left <= SNAP || to_right <= SNAP {
        let dock = if to_left <= to_right { Dock::Left } else { Dock::Right };
        let centre = island.y + island.h / 2.0 - mon.y;
        return (dock, 0.0, clamp(centre, 0.0, mon.h));
    }
    if to_top <= SNAP {
        // The top edge keeps where along it the island was dropped; near the middle
        // it snaps back to centred (0). Never 0 otherwise, since 0 means "centred".
        let centre = island.x + island.w / 2.0 - mon.x;
        let dx = if (centre - mon.w / 2.0).abs() <= CENTER_SNAP {
            0.0
        } else {
            clamp(centre, 1.0, mon.w)
        };
        return (Dock::Top, dx, 0.0);
    }
    let centre = island.x + island.w / 2.0 - mon.x;
    (Dock::Free, clamp(centre, 0.0, mon.w), clamp(island.y - mon.y, 0.0, mon.h))
}

/// A drag in flight: where inside the window the cursor grabbed it, physical px.
#[derive(Clone, Copy)]
struct DragState {
    dx: f64,
    dy: f64,
}

#[derive(Serialize, Clone)]
pub struct CursorPayload {
    pub x: f64,
    pub y: f64,
}

#[derive(Serialize, Clone)]
pub struct ScreenInfo {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// The island shape in window-logical coordinates, pushed by the front end.
/// The poll thread owns the click-through decision so it lands in the same 16 ms
/// tick as the cursor read — an IPC round trip here loses clicks.
#[derive(Clone, Copy, Default)]
pub struct IslandRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Wakes / parks the cursor poll thread so a hidden island costs literally nothing.
pub struct PollGate {
    active: Mutex<bool>,
    cv: Condvar,
    pub collapsed: AtomicBool,
    pub rect: Mutex<IslandRect>,
    /// Mirrors the window flag so we only call into Win32 when it changes.
    ignoring: AtomicBool,
    /// Set while the user is dragging the island; the poll thread then moves the window.
    drag: Mutex<Option<DragState>>,
    /// The last placement handed to the front end, for a page that loads later.
    pub placement: Mutex<Placement>,
}

impl PollGate {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(false),
            cv: Condvar::new(),
            collapsed: AtomicBool::new(true),
            rect: Mutex::new(IslandRect::default()),
            ignoring: AtomicBool::new(false),
            drag: Mutex::new(None),
            placement: Mutex::new(Placement::default()),
        }
    }

    pub fn set_rect(&self, rect: IslandRect) {
        *self.rect.lock().unwrap() = rect;
    }

    /// Forces the next poll tick to re-apply the flag (after a window resize).
    pub fn forget_ignore_state(&self) {
        self.ignoring.store(false, Ordering::Relaxed);
    }

    pub fn set_active(&self, on: bool) {
        let mut guard = self.active.lock().unwrap();
        *guard = on;
        self.cv.notify_all();
    }

    fn wait_until_active(&self) {
        let mut guard = self.active.lock().unwrap();
        while !*guard {
            guard = self.cv.wait(guard).unwrap();
        }
    }

    fn is_active(&self) -> bool {
        *self.active.lock().unwrap()
    }
}

pub fn window(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window(WINDOW_LABEL)
}

fn cursor_physical() -> Option<(f64, f64)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some((p.x as f64, p.y as f64))
}

/// Lets dropped files reach the app again.
///
/// wry installs its drop target by walking the webview's child windows **once**,
/// when the webview is created. WebView2 creates `Chrome_RenderWidgetHostHWND`
/// later and registers its own target on it; being the innermost window, that one
/// wins, and since the page has no HTML5 drop handler it refuses everything — the
/// "no drop" cursor, with nothing reaching Tauri. Revoking it makes OLE fall
/// through to the target wry registered on the parent widget, which is the one
/// that feeds Tauri's drag events.
///
/// Cheap and idempotent, so it is simply re-run whenever a drag might be starting.
pub fn unblock_webview_drops(app: &AppHandle) {
    for label in [WINDOW_LABEL, "settings"] {
        let Some(win) = app.get_webview_window(label) else { continue };
        let Some(hwnd) = hwnd_of(&win) else { continue };
        unsafe {
            let _ = EnumChildWindows(Some(hwnd), Some(revoke_render_widget), LPARAM(0));
        }
    }
}

unsafe extern "system" fn revoke_render_widget(hwnd: HWND, _: LPARAM) -> BOOL {
    let mut name = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut name) };
    if len > 0 {
        let class = String::from_utf16_lossy(&name[..len as usize]);
        if class == "Chrome_RenderWidgetHostHWND" {
            let _ = unsafe { RevokeDragDrop(hwnd) };
        }
    }
    true.into()
}

/// True while the left mouse button is held — the only signal we get that a
/// drag might be in flight before it reaches the window.
fn left_button_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}

fn monitor_contains(m: &Monitor, x: f64, y: f64) -> bool {
    let p = m.position();
    let s = m.size();
    x >= p.x as f64
        && x < (p.x + s.width as i32) as f64
        && y >= p.y as f64
        && y < (p.y + s.height as i32) as f64
}

/// A stable name for a display, to remember which one the island was dropped on.
fn monitor_key(m: &Monitor) -> String {
    m.name().cloned().unwrap_or_else(|| {
        let p = m.position();
        format!("{},{}", p.x, p.y)
    })
}

/// The display the island lives on: the one under the cursor (if so asked), else the
/// one it was last dropped on while that is still plugged in, else the primary.
fn target_monitor(app: &AppHandle, pref: &str) -> Option<Monitor> {
    let monitors = app.available_monitors().ok()?;
    if pref == "cursor" {
        if let Some((cx, cy)) = cursor_physical() {
            if let Some(m) = monitors.iter().find(|m| monitor_contains(m, cx, cy)) {
                return Some(m.clone());
            }
        }
    }
    let remembered = app
        .try_state::<crate::Shared>()
        .map(|s| s.settings.lock().unwrap().dock_screen.clone())
        .unwrap_or_default();
    if !remembered.is_empty() {
        if let Some(m) = monitors.iter().find(|m| monitor_key(m) == remembered) {
            return Some(m.clone());
        }
    }
    app.primary_monitor()
        .ok()
        .flatten()
        .or_else(|| monitors.into_iter().next())
}

pub fn screen_info(app: &AppHandle, pref: &str) -> ScreenInfo {
    match target_monitor(app, pref) {
        Some(m) => {
            let scale = m.scale_factor();
            let p = m.position();
            let s = m.size();
            ScreenInfo {
                x: p.x as f64 / scale,
                y: p.y as f64 / scale,
                width: s.width as f64 / scale,
                height: s.height as f64 / scale,
                scale,
            }
        }
        None => ScreenInfo { x: 0.0, y: 0.0, width: 1920.0, height: 1080.0, scale: 1.0 },
    }
}

/// A display as a logical-px rectangle.
fn monitor_rect(m: &Monitor) -> Rect {
    let scale = m.scale_factor();
    let p = m.position();
    let s = m.size();
    Rect {
        x: p.x as f64 / scale,
        y: p.y as f64 / scale,
        w: s.width as f64 / scale,
        h: s.height as f64 / scale,
    }
}

/// The dock stored in the settings, with its coordinates.
fn stored_dock(app: &AppHandle) -> (Dock, f64, f64) {
    match app.try_state::<crate::Shared>() {
        Some(shared) => {
            let s = shared.settings.lock().unwrap();
            (Dock::parse(&s.dock), s.dock_x, s.dock_y)
        }
        None => (Dock::Top, 0.0, 0.0),
    }
}

/// Places and sizes the window for the stored dock. `collapsed` picks the wake
/// strip instead of the panel. Tells the front end where the island hangs.
pub fn apply_geometry(app: &AppHandle, pref: &str, collapsed: bool) {
    let Some(win) = window(app) else { return };
    let Some(m) = target_monitor(app, pref) else { return };

    let scale = m.scale_factor();
    let (dock, dx, dy) = stored_dock(app);
    let geo = window_geometry(monitor_rect(&m), dock, dx, dy, collapsed);

    let px = |v: f64| (v * scale).round() as i32;
    let pw = px(geo.win.w).max(1) as u32;
    let ph = px(geo.win.h).max(1) as u32;

    let _ = win.set_size(PhysicalSize::new(pw, ph));
    let _ = win.set_position(PhysicalPosition::new(px(geo.win.x), px(geo.win.y)));
    // Moving across displays can rescale the window: re-assert the physical size.
    let _ = win.set_size(PhysicalSize::new(pw, ph));
    let _ = win.set_always_on_top(true);

    let placement = Placement {
        dock: dock.as_str().to_string(),
        anchor_x: geo.anchor_x,
        anchor_y: geo.anchor_y,
    };
    if let Some(shared) = app.try_state::<crate::Shared>() {
        *shared.gate.placement.lock().unwrap() = placement.clone();
    }
    let _ = app.emit_to(WINDOW_LABEL, "placement", placement);
}

/// The front end saw the mouse go down on the island and then move: from here the
/// poll thread carries the window along with the cursor until the button is up.
/// False if there was nothing to grab.
pub fn begin_drag(app: &AppHandle, gate: &PollGate) -> bool {
    let Some(win) = window(app) else { return false };
    let (Some((cx, cy)), Ok(origin)) = (cursor_physical(), win.outer_position()) else {
        return false;
    };
    *gate.drag.lock().unwrap() = Some(DragState {
        dx: cx - origin.x as f64,
        dy: cy - origin.y as f64,
    });
    true
}

/// One poll tick of a drag: follow the cursor, or — once the button is up —
/// work out where the island now lives.
fn drive_drag(app: &AppHandle, gate: &PollGate) {
    let Some(drag) = *gate.drag.lock().unwrap() else { return };
    let Some(win) = window(app) else {
        *gate.drag.lock().unwrap() = None;
        return;
    };
    if left_button_down() {
        if let Some((cx, cy)) = cursor_physical() {
            let _ = win.set_position(PhysicalPosition::new(
                (cx - drag.dx).round() as i32,
                (cy - drag.dy).round() as i32,
            ));
        }
        return;
    }
    *gate.drag.lock().unwrap() = None;
    drop_island(app, &win, gate);
}

/// The button came up: dock (or leave free) according to where the island is, keep
/// it in the settings and move the window to match.
fn drop_island(app: &AppHandle, win: &WebviewWindow, gate: &PollGate) {
    let Some(shared) = app.try_state::<crate::Shared>() else { return };
    let pref = shared.settings.lock().unwrap().screen.clone();
    // The display the button came up over is the one the island now lives on. (It used
    // to be the stored one — the main display — so a drag onto a second screen was
    // classified against the first and pulled straight back.)
    let under_cursor = cursor_physical().and_then(|(cx, cy)| {
        app.available_monitors()
            .ok()?
            .into_iter()
            .find(|m| monitor_contains(m, cx, cy))
    });
    let Some(m) = under_cursor.or_else(|| target_monitor(app, &pref)) else { return };
    let Ok(origin) = win.outer_position() else { return };
    // The window may still carry the scale of the display it started on, so the
    // island is measured physically and only then turned into the target's logical px.
    let win_scale = win.scale_factor().unwrap_or(1.0);
    let mon_scale = m.scale_factor();
    let r = *gate.rect.lock().unwrap();
    let island = Rect {
        x: (origin.x as f64 + r.x * win_scale) / mon_scale,
        y: (origin.y as f64 + r.y * win_scale) / mon_scale,
        w: r.w * win_scale / mon_scale,
        h: r.h * win_scale / mon_scale,
    };
    let (dock, dx, dy) = classify_drop(monitor_rect(&m), island);
    let screen = monitor_key(&m);
    crate::log::line(format!("island dropped → {} ({dx:.0}, {dy:.0}) on {screen}", dock.as_str()));

    let updated = {
        let mut s = shared.settings.lock().unwrap();
        s.dock = dock.as_str().to_string();
        s.dock_x = dx;
        s.dock_y = dy;
        s.dock_screen = screen;
        let _ = crate::settings::save(&s);
        s.clone()
    };
    // The settings window keeps its own copy; without this it would save a stale one.
    let _ = app.emit("settings-changed", updated);

    let collapsed = gate.collapsed.load(Ordering::Relaxed);
    apply_geometry(app, &pref, collapsed);
}

fn hwnd_of(win: &WebviewWindow) -> Option<HWND> {
    let raw = win.hwnd().ok()?.0 as isize;
    if raw == 0 {
        return None;
    }
    Some(HWND(raw as *mut _))
}

/// WS_EX_NOACTIVATE keeps clicks from stealing focus; WS_EX_TOOLWINDOW keeps the
/// island out of Alt-Tab.
pub fn make_non_activating(win: &WebviewWindow) {
    let Some(hwnd) = hwnd_of(win) else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = ex | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
    }
}

/// Temporarily allow activation so a text field inside the island can be typed in.
pub fn set_activating(win: &WebviewWindow, activating: bool) {
    let Some(hwnd) = hwnd_of(win) else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = if activating {
            ex & !(WS_EX_NOACTIVATE.0 as isize)
        } else {
            ex | WS_EX_NOACTIVATE.0 as isize
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
    }
}

/// Position, size and scale of the monitor the island lives on. Any change here
/// means the island has to be placed again.
fn current_screen_key(app: &AppHandle) -> Option<(i32, i32, u32, u32, u64)> {
    let pref = app
        .try_state::<crate::Shared>()
        .map(|s| s.settings.lock().unwrap().screen.clone())
        .unwrap_or_else(|| "primary".into());
    let m = target_monitor(app, &pref)?;
    let p = m.position();
    let size = m.size();
    Some((p.x, p.y, size.width, size.height, m.scale_factor().to_bits()))
}

/// Emits `cursor` (window-logical coordinates) at ~60 Hz while the island is
/// visible. Parked on a condvar the rest of the time.
pub fn spawn_cursor_poll(app: AppHandle, gate: Arc<PollGate>) {
    std::thread::spawn(move || {
        let mut was_down = false;
        // Remembered across wakes so a display change while hidden is noticed the
        // moment the island comes back.
        let mut last_screen: Option<(i32, i32, u32, u32, u64)> = None;
        loop {
            gate.wait_until_active();
            let mut last = (f64::MIN, f64::MIN);
            let mut ticks: u32 = 0;
            while gate.is_active() {
                std::thread::sleep(Duration::from_millis(16));

                // A drag owns the tick: the window follows the cursor, and the
                // release is where the island finds out where it now lives.
                if gate.drag.lock().unwrap().is_some() {
                    drive_drag(&app, &gate);
                    continue;
                }

                // Monitors get plugged in, unplugged, rearranged and rescaled, and
                // an island pinned to coordinates that no longer exist is an island
                // nobody can reach. Checked about twice a second — the cursor poll
                // is already running, so this costs one monitor query.
                ticks = ticks.wrapping_add(1);
                if ticks % 30 == 0 {
                    let now = current_screen_key(&app);
                    if now.is_some() && now != last_screen {
                        let first = last_screen.is_none();
                        last_screen = now;
                        if !first {
                            crate::log::line("display layout changed — repositioning".to_string());
                            let _ = app.emit_to(WINDOW_LABEL, "screen-changed", ());
                        }
                    }
                }

                let Some(win) = window(&app) else { continue };
                let Ok(origin) = win.outer_position() else { continue };
                let scale = win.scale_factor().unwrap_or(1.0);
                let Some((cx, cy)) = cursor_physical() else { continue };
                let x = (cx - origin.x as f64) / scale;
                let y = (cy - origin.y as f64) / scale;
                let size = match win.inner_size() {
                    Ok(s) => (s.width as f64 / scale, s.height as f64 / scale),
                    Err(_) => (PANEL_W, PANEL_H),
                };
                if (x - last.0).abs() < 1.0 && (y - last.1).abs() < 1.0 {
                    continue;
                }
                last = (x, y);

                // Click-through: the window only takes the mouse over the island
                // shape. A small entry margin means the flag is already off by the
                // time a moving cursor reaches a button.
                let r = *gate.rect.lock().unwrap();
                let on_island = r.w > 0.0
                    && x >= r.x - HIT_MARGIN
                    && x <= r.x + r.w + HIT_MARGIN
                    && y >= r.y - HIT_MARGIN
                    && y <= r.y + r.h + HIT_MARGIN;

                // A file being dragged has to be able to find us. WS_EX_TRANSPARENT
                // — what click-through is on Windows — hides the window from
                // WindowFromPoint, so OLE finds no drop target and shows the "no
                // drop" cursor. macOS has no such problem: AppKit delivers drags to
                // registered destinations whatever ignoresMouseEvents says. So while
                // a button is held anywhere over the panel, the whole panel takes
                // the mouse, which also makes the drop zone as forgiving as the Mac's.
                // A press may be the start of a drag: make sure the drop target is
                // ours before the file arrives.
                let down = left_button_down();
                if down && !was_down {
                    let handle = app.clone();
                    let _ = app.run_on_main_thread(move || unblock_webview_drops(&handle));
                }
                was_down = down;

                let dragging = down
                    && x >= 0.0
                    && x <= size.0
                    && y >= 0.0
                    && y <= size.1;

                let accept = on_island || dragging;
                if gate.ignoring.load(Ordering::Relaxed) == accept {
                    gate.ignoring.store(!accept, Ordering::Relaxed);
                    let _ = win.set_ignore_cursor_events(!accept);
                }

                let _ = win.emit("cursor", CursorPayload { x, y });
            }
        }
    });
}

pub fn set_ignore_cursor(app: &AppHandle, ignore: bool) {
    if let Some(win) = window(app) {
        let _ = win.set_ignore_cursor_events(ignore);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MON: Rect = Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 };
    /// A second display to the right of the first, not at the origin.
    const MON2: Rect = Rect { x: 1920.0, y: 0.0, w: 1920.0, h: 1080.0 };

    fn at(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn top_is_centred_on_the_display_and_flush_to_the_top() {
        let g = window_geometry(MON, Dock::Top, 0.0, 0.0, false);
        assert_eq!(g.win, at(600.0, 0.0, PANEL_W, PANEL_H));
        assert_eq!((g.anchor_x, g.anchor_y), (PANEL_W / 2.0, 0.0));
    }

    #[test]
    fn a_collapsed_top_window_is_just_the_wake_strip() {
        let g = window_geometry(MON, Dock::Top, 0.0, 0.0, true);
        assert_eq!(g.win, at(840.0, 0.0, STRIP_W, STRIP_H));
    }

    #[test]
    fn a_side_dock_sits_flush_on_its_edge_around_the_dropped_height() {
        let l = window_geometry(MON, Dock::Left, 0.0, 500.0, false);
        assert_eq!((l.win.x, l.win.w), (0.0, PANEL_W));
        assert_eq!(l.win.y, 500.0 - PANEL_H / 2.0);
        // The strip's centre is exactly where it was dropped, in window terms.
        assert_eq!((l.anchor_x, l.anchor_y), (0.0, PANEL_H / 2.0));

        let r = window_geometry(MON, Dock::Right, 0.0, 500.0, false);
        assert_eq!(r.win.x, 1920.0 - PANEL_W, "flush to the right edge");
        assert_eq!(r.anchor_x, PANEL_W, "the island hangs from the window's right side");
    }

    #[test]
    fn a_side_window_never_leaves_the_display_and_the_anchor_takes_the_slack() {
        let top = window_geometry(MON, Dock::Left, 0.0, 10.0, false);
        assert_eq!(top.win.y, 0.0);
        assert_eq!(top.anchor_y, 10.0, "still 10 px from the top of the screen");

        let bottom = window_geometry(MON, Dock::Right, 0.0, 1070.0, false);
        assert_eq!(bottom.win.y, 1080.0 - PANEL_H);
        assert_eq!(bottom.win.y + bottom.anchor_y, 1070.0, "still where it was dropped");
    }

    #[test]
    fn a_collapsed_side_dock_is_a_vertical_strip_on_the_edge() {
        let l = window_geometry(MON, Dock::Left, 0.0, 400.0, true);
        assert_eq!(l.win, at(0.0, 400.0 - STRIP_W / 2.0, STRIP_H, STRIP_W));
        let r = window_geometry(MON, Dock::Right, 0.0, 400.0, true);
        assert_eq!(r.win.x, 1920.0 - STRIP_H);
    }

    #[test]
    fn free_stays_where_it_was_dropped_with_its_window_inside_the_display() {
        let g = window_geometry(MON, Dock::Free, 960.0, 400.0, false);
        assert_eq!(g.win, at(600.0, 400.0, PANEL_W, PANEL_H));
        assert_eq!((g.anchor_x, g.anchor_y), (360.0, 0.0));

        // Near the right edge the window slides in; the island does not.
        let edge = window_geometry(MON, Dock::Free, 1800.0, 400.0, false);
        assert_eq!(edge.win.x, 1920.0 - PANEL_W);
        assert_eq!(edge.win.x + edge.anchor_x, 1800.0);

        // Near the bottom the window slides up; the island stays put.
        let low = window_geometry(MON, Dock::Free, 960.0, 1060.0, false);
        assert_eq!(low.win.y, 1080.0 - PANEL_H);
        assert_eq!(low.win.y + low.anchor_y, 1060.0);
    }

    #[test]
    fn free_never_collapses() {
        assert_eq!(
            window_geometry(MON, Dock::Free, 960.0, 400.0, true),
            window_geometry(MON, Dock::Free, 960.0, 400.0, false),
        );
    }

    #[test]
    fn a_display_smaller_than_the_window_does_not_panic_or_go_negative() {
        let tiny = at(0.0, 0.0, 500.0, 300.0);
        for dock in [Dock::Top, Dock::Left, Dock::Right, Dock::Free] {
            let g = window_geometry(tiny, dock, 250.0, 150.0, false);
            assert!(g.win.w > 0.0 && g.win.h > 0.0);
        }
    }

    #[test]
    fn dropping_near_a_side_docks_there_and_remembers_the_height() {
        // The compact horizontal pill (288×32), left edge 20 px away, at y=600.
        let (dock, _, y) = classify_drop(MON, at(20.0, 584.0, 288.0, 32.0));
        assert_eq!(dock, Dock::Left);
        assert_eq!(y, 600.0, "the strip's centre, not the pill's top");

        let (dock, _, _) = classify_drop(MON, at(1920.0 - 288.0 - 10.0, 300.0, 288.0, 32.0));
        assert_eq!(dock, Dock::Right);
    }

    #[test]
    fn a_vertical_strip_dragged_a_little_stays_docked() {
        // 32 wide × 288 tall on the left edge, nudged 30 px inwards and 100 px down.
        let (dock, _, y) = classify_drop(MON, at(30.0, 300.0, 32.0, 288.0));
        assert_eq!(dock, Dock::Left);
        assert_eq!(y, 444.0);
    }

    #[test]
    fn sides_beat_the_top_in_a_corner() {
        let (dock, _, _) = classify_drop(MON, at(10.0, 10.0, 288.0, 32.0));
        assert_eq!(dock, Dock::Left);
    }

    #[test]
    fn dropping_near_the_top_keeps_where_along_the_edge_it_was_dropped() {
        // Island centre at x=844 (left of the middle, 960), 30 px from the top.
        let (dock, dx, dy) = classify_drop(MON, at(700.0, 30.0, 288.0, 32.0));
        assert_eq!((dock, dy), (Dock::Top, 0.0));
        assert_eq!(dx, 844.0);

        // Close to the middle it goes back to exactly centred.
        let (_, dx, _) = classify_drop(MON, at(960.0 - 144.0 + 10.0, 10.0, 288.0, 32.0));
        assert_eq!(dx, 0.0);

        // Just outside the side snap, so still the top: never 0 by accident, 0 means "centred".
        let (dock, dx, _) = classify_drop(MON, at(60.0, 10.0, 288.0, 32.0));
        assert_eq!((dock, dx), (Dock::Top, 204.0));
    }

    #[test]
    fn a_top_island_stays_where_it_was_dropped_with_its_window_inside_the_display() {
        let g = window_geometry(MON, Dock::Top, 400.0, 0.0, false);
        assert_eq!(g.win, at(40.0, 0.0, PANEL_W, PANEL_H));
        assert_eq!((g.anchor_x, g.anchor_y), (360.0, 0.0));

        // Near the left edge the window slides in and the island does not.
        let edge = window_geometry(MON, Dock::Top, 150.0, 0.0, false);
        assert_eq!(edge.win.x, 0.0);
        assert_eq!(edge.win.x + edge.anchor_x, 150.0);

        // Hidden, the wake strip sits under the same spot.
        let strip = window_geometry(MON, Dock::Top, 400.0, 0.0, true);
        assert_eq!(strip.win.x + strip.anchor_x, 400.0);

        // On a second display it is measured from that display's corner.
        let g2 = window_geometry(MON2, Dock::Top, 400.0, 0.0, false);
        assert_eq!(g2.win.x, 1920.0 + 40.0);
    }

    #[test]
    fn dropping_in_the_middle_leaves_it_free_at_the_drop_point() {
        let (dock, x, y) = classify_drop(MON, at(800.0, 500.0, 288.0, 32.0));
        assert_eq!(dock, Dock::Free);
        assert_eq!((x, y), (944.0, 500.0), "centre x, top y");
    }

    #[test]
    fn a_display_not_at_the_origin_is_measured_from_its_own_corner() {
        // Same drop as above, but on the second display: 20 px from *its* left edge.
        let (dock, _, y) = classify_drop(MON2, at(1940.0, 584.0, 288.0, 32.0));
        assert_eq!((dock, y), (Dock::Left, 600.0));

        let g = window_geometry(MON2, Dock::Left, 0.0, 600.0, false);
        assert_eq!(g.win.x, 1920.0, "flush to the second display's left edge");

        let (dock, x, _) = classify_drop(MON2, at(2720.0, 500.0, 288.0, 32.0));
        assert_eq!((dock, x), (Dock::Free, 944.0), "relative to the display, not the desktop");
    }

    #[test]
    fn dock_names_round_trip_and_anything_else_is_the_notch() {
        for dock in [Dock::Top, Dock::Left, Dock::Right, Dock::Free] {
            assert_eq!(Dock::parse(dock.as_str()), dock);
        }
        assert_eq!(Dock::parse(""), Dock::Top);
        assert_eq!(Dock::parse("bottom"), Dock::Top);
    }
}
