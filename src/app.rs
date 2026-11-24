//! Settings window, tray icon, and the decisions about when HDR goes on and off.
//! Everything in here runs on the UI thread.

use crate::config::{Config, GameEntry};
use crate::{hdr, log, watch};
use std::cell::{Cell, RefCell};
use std::mem::size_of;
use windows::core::{w, HSTRING, PCSTR, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Controls::Dialogs::*;
use windows::Win32::UI::Controls::{CheckDlgButton, SetWindowTheme, BST_CHECKED, BST_UNCHECKED, EM_SETCUEBANNER};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, SetFocus, VK_DELETE};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

// Private window messages.
const WM_TRAY: u32 = WM_APP + 1; // mouse events on the tray icon
/// From the watch thread: `wParam` is a `watch::` event, `lParam` a `Box<String>`.
pub const WM_WATCH: u32 = WM_APP + 2;
const WM_SHOW: u32 = WM_APP + 3; // a second launch asks the running copy to show its window

// Control and menu ids. 1 and 2 are IDOK and IDCANCEL, which Enter and Esc send.
const ID_OK: i32 = 1;
const ID_CANCEL: i32 = 2;
const ID_STATUS: i32 = 100;
const ID_TOGGLE: i32 = 101;
const ID_RESTORE: i32 = 102;
const ID_GAMES: i32 = 103;
const ID_ADD_RUNNING: i32 = 104;
const ID_BROWSE: i32 = 105;
const ID_REMOVE: i32 = 106;
const ID_LOG: i32 = 107;
const ID_OPEN: i32 = 108;
const ID_EXIT: i32 = 109;
const ID_FILTER: i32 = 110;
const ID_PROCESSES: i32 = 111;
const ID_LABEL: i32 = -1; // static text nobody looks up

const TIMER_RESTORE: usize = 1;
const TIMER_TRAY: usize = 2;
const TIMER_WATCH: usize = 3;

/// Wait this long after the last game exits before putting HDR back. A game that restarts
/// itself (through its launcher or Steam) then costs one mode switch, not three.
const RESTORE_DELAY_MS: u32 = 5000;
const LOG_LINES: usize = 200;

// Colours are 0x00BBGGRR.
const BG: COLORREF = COLORREF(0x00202020);
const FIELD: COLORREF = COLORREF(0x002B2B2B);
const TEXT: COLORREF = COLORREF(0x00FFFFFF);
const TEXT_DIM: COLORREF = COLORREF(0x00B4B4B4);
const GREEN: COLORREF = COLORREF(0x0080E08C);
const AMBER: COLORREF = COLORREF(0x0050B4F0);

// ── Layout ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
enum Font {
    Normal,
    Bold,
    Big,
}

/// One child control. `rect` is x, y, width, height in 96-dpi pixels.
struct Ctl {
    id: i32,
    class: PCWSTR,
    text: PCWSTR,
    style: u32,
    rect: [i32; 4],
    font: Font,
}

/// Centres static text vertically. Lives in a windows-crate module nothing else here needs.
const SS_CENTERIMAGE: u32 = 0x200;
const BUTTON: u32 = WS_TABSTOP.0 | BS_PUSHBUTTON as u32;
const LIST: u32 = WS_TABSTOP.0 | WS_VSCROLL.0 | WS_BORDER.0 | (LBS_NOINTEGRALHEIGHT | LBS_NOTIFY) as u32;

const MAIN_STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPED.0 | WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0);
const MAIN_SIZE: (i32, i32) = (480, 462);
#[rustfmt::skip]
const MAIN: &[Ctl] = &[
    Ctl { id: ID_STATUS, class: w!("STATIC"), text: w!(""), style: SS_CENTERIMAGE, rect: [16, 14, 300, 32], font: Font::Big },
    Ctl { id: ID_TOGGLE, class: w!("BUTTON"), text: w!("Turn HDR on"), style: BUTTON, rect: [344, 15, 120, 30], font: Font::Normal },
    Ctl { id: ID_RESTORE, class: w!("BUTTON"), text: w!("Put HDR back the way it was when the last game exits"), style: WS_TABSTOP.0 | BS_CHECKBOX as u32, rect: [16, 56, 448, 22], font: Font::Normal },
    Ctl { id: ID_LABEL, class: w!("STATIC"), text: w!("Games"), style: 0, rect: [16, 94, 448, 18], font: Font::Bold },
    Ctl { id: ID_GAMES, class: w!("LISTBOX"), text: w!(""), style: LIST | LBS_WANTKEYBOARDINPUT as u32, rect: [16, 116, 448, 112], font: Font::Normal },
    Ctl { id: ID_ADD_RUNNING, class: w!("BUTTON"), text: w!("Add a running game\u{2026}"), style: BUTTON, rect: [16, 236, 160, 30], font: Font::Normal },
    Ctl { id: ID_BROWSE, class: w!("BUTTON"), text: w!("Browse\u{2026}"), style: BUTTON, rect: [184, 236, 96, 30], font: Font::Normal },
    Ctl { id: ID_REMOVE, class: w!("BUTTON"), text: w!("Remove"), style: BUTTON, rect: [368, 236, 96, 30], font: Font::Normal },
    Ctl { id: ID_LABEL, class: w!("STATIC"), text: w!("Activity"), style: 0, rect: [16, 284, 448, 18], font: Font::Bold },
    Ctl { id: ID_LOG, class: w!("LISTBOX"), text: w!(""), style: LIST | LBS_NOSEL as u32, rect: [16, 306, 448, 140], font: Font::Normal },
];

const PICKER_STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0);
const PICKER_SIZE: (i32, i32) = (360, 440);
#[rustfmt::skip]
const PICKER: &[Ctl] = &[
    Ctl { id: ID_FILTER, class: w!("EDIT"), text: w!(""), style: WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL as u32, rect: [16, 16, 328, 26], font: Font::Normal },
    Ctl { id: ID_PROCESSES, class: w!("LISTBOX"), text: w!(""), style: LIST, rect: [16, 50, 328, 336], font: Font::Normal },
    Ctl { id: ID_OK, class: w!("BUTTON"), text: w!("Add"), style: WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32, rect: [148, 396, 94, 30], font: Font::Normal },
    Ctl { id: ID_CANCEL, class: w!("BUTTON"), text: w!("Cancel"), style: BUTTON, rect: [250, 396, 94, 30], font: Font::Normal },
];

// ── State ─────────────────────────────────────────────────────────────────

/// Shared by both windows through GWLP_USERDATA. Only ever touched on the UI thread, and
/// window procedures re-enter (a SetWindowText in one handler runs WM_CTLCOLOR in another),
/// so every field is a Cell or a RefCell that is borrowed for one statement at a time.
struct App {
    hwnd: Cell<HWND>,
    picker: Cell<HWND>,
    config: RefCell<Config>,
    /// The HDR displays HDRify has switched on for the running games, in the state they
    /// had before. `Some` from the first one until the restore runs, so it also means
    /// "HDR is currently ours to put back".
    saved: RefCell<Option<Vec<hdr::Display>>>,
    hdr_on: Cell<bool>,
    /// The last restore could not reach every display in `saved` (one was off, or
    /// refused); it is tried again whenever the displays change.
    restore_owed: Cell<bool>,
    /// The trace session has been up since the last start; decides whether to restart it.
    watching: Cell<bool>,
    /// Exe names offered by the picker, before filtering.
    processes: RefCell<Vec<String>>,
    fonts: Cell<[HFONT; 3]>,
    /// Tray icons: [HDR off, HDR on].
    icons: [HICON; 2],
    bg: HBRUSH,
    field: HBRUSH,
    /// Broadcast by Explorer when the taskbar (re)appears.
    taskbar_created: u32,
}

impl App {
    unsafe fn item(&self, id: i32) -> HWND {
        GetDlgItem(self.hwnd.get(), id).unwrap_or_default()
    }

    /// Enables or disables a control. Disabling the control that has the focus would
    /// leave the keyboard with nowhere to go, so the focus moves to the games list first.
    unsafe fn enable(&self, id: i32, on: bool) {
        let control = self.item(id);
        if !on && GetFocus() == control {
            let _ = SetFocus(self.item(ID_GAMES));
        }
        let _ = EnableWindow(control, on);
    }

    unsafe fn log(&self, msg: &str) {
        let line = HSTRING::from(log::write(msg));
        let list = self.item(ID_LOG);
        SendMessageW(list, LB_ADDSTRING, WPARAM(0), LPARAM(line.as_ptr() as isize));
        let mut count = SendMessageW(list, LB_GETCOUNT, WPARAM(0), LPARAM(0)).0;
        if count as usize > LOG_LINES {
            SendMessageW(list, LB_DELETESTRING, WPARAM(0), LPARAM(0));
            count -= 1;
        }
        // Scroll to the newest line. The index must exist or the list box ignores it.
        SendMessageW(list, LB_SETTOPINDEX, WPARAM(count.max(1) as usize - 1), LPARAM(0));
    }

    fn game_names(&self) -> Vec<String> {
        self.config.borrow().games.iter().map(|g| g.exe_name.clone()).collect()
    }

    // ── HDR ───────────────────────────────────────────────────────────────

    /// Reads the real display state and shows it in the window and the tray.
    unsafe fn refresh(&self) {
        let displays = hdr::query();
        let on = displays.iter().any(|d| d.enabled);
        let capable = displays.iter().any(|d| d.supported);
        self.hdr_on.set(on);

        let mut status = String::from(match (capable, on) {
            (false, _) => "No HDR display",
            (true, true) => "HDR on",
            (true, false) => "HDR off",
        });
        match watch::running() {
            _ if !watch::is_tracing() => status += " \u{b7} not watching for games",
            0 => {}
            1 => status += " \u{b7} 1 game running",
            n => status += &format!(" \u{b7} {n} games running"),
        }
        let _ = SetWindowTextW(self.item(ID_STATUS), &HSTRING::from(&status));
        let _ = SetWindowTextW(self.item(ID_TOGGLE), if on { w!("Turn HDR off") } else { w!("Turn HDR on") });
        self.enable(ID_TOGGLE, capable);
        self.tray(&format!("HDRify \u{2014} {status}"));
    }

    /// Brings HDR in line with whether a watched game is running. Call after anything
    /// that can change the set of running games.
    unsafe fn sync_hdr(&self) {
        let hwnd = self.hwnd.get();
        if watch::running() > 0 {
            let _ = KillTimer(hwnd, TIMER_RESTORE);
            // Every HDR display gets one try: all of them when the first game starts, and
            // any that is connected or switched on later (the TV, say) when WM_DISPLAYCHANGE
            // brings us back here. With none there is nothing to switch or to put back.
            let known = self.saved.borrow().clone().unwrap_or_default();
            let (new, switch) = hdr::wanted(&known, &hdr::query(), self.restore_owed.replace(false));
            // Recorded before switching, so the WM_DISPLAYCHANGE the switch causes finds
            // these displays already known.
            if !new.is_empty() {
                self.saved.borrow_mut().get_or_insert_with(Vec::new).extend(new);
            }
            if !switch.is_empty() {
                let changed = hdr::set_all(&switch, true);
                self.log(&if changed > 0 {
                    format!("HDR turned on ({changed} display{})", if changed == 1 { "" } else { "s" })
                } else if switch.iter().all(|d| d.enabled) {
                    "HDR was already on".into()
                } else {
                    "[!] Could not turn HDR on: the display did not accept it".into()
                });
            }
        } else if self.saved.borrow().is_some() {
            SetTimer(hwnd, TIMER_RESTORE, RESTORE_DELAY_MS, None);
        }
        self.refresh();
    }

    /// The last game has been gone for RESTORE_DELAY_MS.
    unsafe fn restore(&self) {
        let _ = KillTimer(self.hwnd.get(), TIMER_RESTORE);
        if watch::running() > 0 {
            return;
        }
        let saved = self.saved.borrow_mut().take();
        if let Some(saved) = saved {
            if !self.config.borrow().restore_on_exit {
                self.restore_owed.set(false);
                self.log("Left HDR as it is (restore is switched off)");
            } else {
                let (changed, owed) = hdr::restore(&saved);
                let was_owed = self.restore_owed.replace(!owed.is_empty());
                if changed > 0 {
                    self.log("HDR put back the way it was before the game");
                } else if owed.is_empty() && !was_owed {
                    self.log("HDR is already the way it was before the game");
                }
                if !owed.is_empty() {
                    // Kept, so sync_hdr starts the timer again at the next display change,
                    // which is when a display that was off comes back. Said once.
                    if !was_owed {
                        let n = owed.len();
                        self.log(&format!(
                            "[!] Could not put HDR back on {n} display{} (off, or it refused); trying again when the displays change",
                            if n == 1 { "" } else { "s" }
                        ));
                    }
                    *self.saved.borrow_mut() = Some(owed);
                }
            }
        }
        self.refresh();
    }

    unsafe fn toggle_hdr(&self) {
        let displays = hdr::query();
        let on = !displays.iter().any(|d| d.enabled);
        let state = if on { "on" } else { "off" };
        if hdr::set_all(&displays, on) > 0 {
            self.log(&format!("HDR turned {state} by hand"));
        } else {
            self.log(&format!("[!] Could not turn HDR {state}"));
        }
        self.refresh();
    }

    // ── Games ─────────────────────────────────────────────────────────────

    unsafe fn add_game(&self, exe: &str) {
        let exe = exe.trim();
        if exe.is_empty() {
            return;
        }
        let index = {
            let mut config = self.config.borrow_mut();
            let found = config.games.iter().position(|g| g.exe_name.eq_ignore_ascii_case(exe));
            found.unwrap_or_else(|| {
                config.games.push(GameEntry { exe_name: exe.to_string() });
                config.games.len() - 1
            })
        };
        self.games_changed(Some(index));
    }

    unsafe fn remove_selected_game(&self) {
        let index = SendMessageW(self.item(ID_GAMES), LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        if index < 0 || index as usize >= self.config.borrow().games.len() {
            return;
        }
        self.config.borrow_mut().games.remove(index as usize);
        let left = self.config.borrow().games.len();
        self.games_changed(left.checked_sub(1).map(|last| last.min(index as usize)));
    }

    /// The watch list was edited: save it, tell the watcher, redraw the list.
    unsafe fn games_changed(&self, select: Option<usize>) {
        self.save();
        for name in watch::set_watch(self.game_names()) {
            self.log(&format!("{name} is already running"));
        }
        self.fill_games(select);
        self.sync_hdr();
    }

    unsafe fn save(&self) {
        let result = self.config.borrow().save(&crate::config::dir());
        if let Err(e) = result {
            self.log(&format!("[!] Could not save settings: {e}"));
        }
    }

    unsafe fn fill_games(&self, select: Option<usize>) {
        let list = self.item(ID_GAMES);
        SendMessageW(list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for name in self.game_names() {
            SendMessageW(list, LB_ADDSTRING, WPARAM(0), LPARAM(HSTRING::from(name).as_ptr() as isize));
        }
        if let Some(index) = select {
            SendMessageW(list, LB_SETCURSEL, WPARAM(index), LPARAM(0));
        }
        self.enable(ID_REMOVE, select.is_some());
    }

    // ── Tray ──────────────────────────────────────────────────────────────

    unsafe fn tray_data(&self, tip: &str) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd.get(),
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.icons[self.hdr_on.get() as usize],
            ..Default::default()
        };
        for (to, from) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *to = from;
        }
        data
    }

    /// Updates the tray icon, adding it if it isn't there: at the first call, and after
    /// Explorer restarted. Whether it is there is never remembered, because adding an icon
    /// that exists fails just like adding one while there is no taskbar. That last case is
    /// normal at logon (HDRify often starts before Explorer), so a timer retries.
    unsafe fn tray(&self, tip: &str) {
        let data = self.tray_data(tip);
        let ok = Shell_NotifyIconW(NIM_MODIFY, &data).as_bool() || Shell_NotifyIconW(NIM_ADD, &data).as_bool();
        if ok {
            let _ = KillTimer(self.hwnd.get(), TIMER_TRAY);
        } else {
            SetTimer(self.hwnd.get(), TIMER_TRAY, 2000, None);
        }
    }

    unsafe fn tray_menu(&self) {
        let hwnd = self.hwnd.get();
        let Ok(menu) = CreatePopupMenu() else { return };
        let toggle = if self.hdr_on.get() { w!("Turn HDR off") } else { w!("Turn HDR on") };
        let restore = if self.config.borrow().restore_on_exit { MF_CHECKED } else { MF_UNCHECKED };
        let _ = AppendMenuW(menu, MF_STRING, ID_TOGGLE as usize, toggle);
        let _ = AppendMenuW(menu, MF_STRING | restore, ID_RESTORE as usize, w!("Put HDR back when games exit"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN as usize, w!("Settings"));
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT as usize, w!("Exit"));
        let _ = SetMenuDefaultItem(menu, ID_OPEN as u32, 0);
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // Without this the menu stays open when the user clicks somewhere else.
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, at.x, at.y, 0, hwnd, None);
        let _ = DestroyMenu(menu);
        if chosen.0 != 0 {
            self.command(chosen.0, BN_CLICKED);
        } else {
            // Dismissed: keyboard focus goes back to the tray, where it came from.
            let _ = Shell_NotifyIconW(NIM_SETFOCUS, &self.tray_data(""));
        }
    }

    // ── Window ────────────────────────────────────────────────────────────

    unsafe fn show(&self) {
        let hwnd = self.hwnd.get();
        let _ = ShowWindow(hwnd, if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW });
        let _ = SetForegroundWindow(hwnd);
        if GetFocus() == HWND::default() {
            let _ = SetFocus(self.item(ID_GAMES)); // somewhere for the keyboard to start
        }
        self.refresh();
    }

    /// Back to the tray. The picker goes too: Windows would leave it on screen alone.
    unsafe fn hide(&self) {
        let _ = DestroyWindow(self.picker.get());
        let _ = ShowWindow(self.hwnd.get(), SW_HIDE);
    }

    unsafe fn command(&self, id: i32, code: u32) {
        let hwnd = self.hwnd.get();
        match (id, code) {
            (ID_TOGGLE, BN_CLICKED) => self.toggle_hdr(),
            (ID_RESTORE, BN_CLICKED) => {
                let on = !self.config.borrow().restore_on_exit;
                self.config.borrow_mut().restore_on_exit = on;
                let _ = CheckDlgButton(hwnd, ID_RESTORE, if on { BST_CHECKED } else { BST_UNCHECKED });
                self.save();
            }
            (ID_ADD_RUNNING, BN_CLICKED) => self.open_picker(),
            (ID_BROWSE, BN_CLICKED) => {
                if let Some(exe) = browse_for_exe(hwnd) {
                    self.add_game(&exe);
                }
            }
            (ID_REMOVE, BN_CLICKED) => self.remove_selected_game(),
            (ID_GAMES, LBN_SELCHANGE) => {
                self.enable(ID_REMOVE, true);
            }
            (ID_OPEN, BN_CLICKED) => self.show(),
            (ID_EXIT, BN_CLICKED) => {
                self.shut_down();
                let _ = DestroyWindow(hwnd);
            }
            (ID_CANCEL, BN_CLICKED) => self.hide(),
            _ => {}
        }
    }

    /// Exit, logoff or shutdown: give HDR back now, there is no later.
    unsafe fn shut_down(&self) {
        // Forget the games first. Putting HDR back sends WM_DISPLAYCHANGE, and with a game
        // still counted as running that would switch HDR straight back on.
        watch::set_watch(vec![]);
        let saved = self.saved.borrow_mut().take();
        if let Some(saved) = saved {
            if self.config.borrow().restore_on_exit {
                hdr::restore(&saved);
            }
        }
        let _ = Shell_NotifyIconW(NIM_DELETE, &self.tray_data(""));
        watch::stop();
        log::write("HDRify closed");
    }

    /// Text and background colours for every child control of both windows.
    unsafe fn colors(&self, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        let dc = HDC(wp.0 as *mut _);
        let id = GetDlgCtrlID(HWND(lp.0 as *mut _));
        let field = msg == WM_CTLCOLORLISTBOX || msg == WM_CTLCOLOREDIT;
        SetTextColor(
            dc,
            match id {
                ID_STATUS if !watch::is_tracing() => AMBER,
                ID_STATUS if self.hdr_on.get() => GREEN,
                ID_STATUS | ID_LOG => TEXT_DIM,
                _ => TEXT,
            },
        );
        SetBkColor(dc, if field { FIELD } else { BG });
        LRESULT(if field { self.field } else { self.bg }.0 as isize)
    }

    /// Fonts for the monitor `hwnd` is on, in the user's system UI typeface.
    unsafe fn make_fonts(&self, hwnd: HWND) {
        let mut metrics = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
        let _ = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            metrics.cbSize,
            Some(&mut metrics as *mut _ as *mut _),
            0,
            GetDpiForWindow(hwnd),
        );
        let mut face = metrics.lfMessageFont;
        let normal = CreateFontIndirectW(&face);
        face.lfWeight = 600;
        let bold = CreateFontIndirectW(&face);
        face.lfHeight = face.lfHeight * 3 / 2;
        let big = CreateFontIndirectW(&face);
        for old in self.fonts.replace([normal, bold, big]) {
            let _ = DeleteObject(old);
        }
    }

    /// Creates the controls in `controls` as children of `hwnd`.
    unsafe fn create_controls(&self, hwnd: HWND, controls: &[Ctl]) {
        let dark: BOOL = true.into();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
        for c in controls {
            let style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | c.style);
            let id = HMENU(c.id as isize as *mut _);
            if let Ok(ctl) =
                CreateWindowExW(WINDOW_EX_STYLE(0), c.class, c.text, style, 0, 0, 0, 0, hwnd, id, None, None)
            {
                let theme = if c.id == ID_FILTER { w!("DarkMode_CFD") } else { w!("DarkMode_Explorer") };
                let _ = SetWindowTheme(ctl, theme, None);
            }
        }
    }

    /// Positions and sizes `controls` and the window itself for the window's current dpi.
    unsafe fn layout(&self, hwnd: HWND, controls: &[Ctl], client: (i32, i32), style: WINDOW_STYLE) {
        let dpi = GetDpiForWindow(hwnd);
        let px = |v: i32| v * dpi as i32 / 96;
        let fonts = self.fonts.get();
        let mut next = GetWindow(hwnd, GW_CHILD).unwrap_or_default();
        for c in controls {
            let [x, y, w, h] = c.rect;
            let _ = SetWindowPos(next, None, px(x), px(y), px(w), px(h), SWP_NOZORDER | SWP_NOACTIVATE);
            SendMessageW(next, WM_SETFONT, WPARAM(fonts[c.font as usize].0 as usize), LPARAM(1));
            next = GetWindow(next, GW_HWNDNEXT).unwrap_or_default();
        }
        let mut frame = RECT { left: 0, top: 0, right: px(client.0), bottom: px(client.1) };
        let _ = AdjustWindowRectExForDpi(&mut frame, style, false, WINDOW_EX_STYLE(0), dpi);
        let (w, h) = (frame.right - frame.left, frame.bottom - frame.top);
        let _ = SetWindowPos(hwnd, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
    }

    // ── Process picker ────────────────────────────────────────────────────

    unsafe fn open_picker(&self) {
        if IsWindow(self.picker.get()).as_bool() {
            let _ = SetForegroundWindow(self.picker.get());
            return;
        }
        let mut at = RECT::default();
        let _ = GetWindowRect(self.hwnd.get(), &mut at);
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("HdrifyPicker"),
            w!("Add a running game"),
            PICKER_STYLE | WS_VISIBLE,
            at.left + 48,
            at.top + 48,
            0,
            0,
            self.hwnd.get(),
            None,
            None,
            Some(self as *const App as *const _),
        );
    }

    /// Reads the running processes into the picker, minus games already on the list.
    unsafe fn load_processes(&self) {
        let watched = self.game_names();
        let mut names: Vec<String> = watch::processes()
            .into_iter()
            .map(|(_, name)| name)
            .filter(|n| n.to_ascii_lowercase().ends_with(".exe") && !watched.iter().any(|w| w.eq_ignore_ascii_case(n)))
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        *self.processes.borrow_mut() = names;
        self.filter_processes();
    }

    unsafe fn filter_processes(&self) {
        let picker = self.picker.get();
        let filter = window_text(GetDlgItem(picker, ID_FILTER).unwrap_or_default()).to_lowercase();
        let list = GetDlgItem(picker, ID_PROCESSES).unwrap_or_default();
        SendMessageW(list, WM_SETREDRAW, WPARAM(0), LPARAM(0));
        SendMessageW(list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for name in self.processes.borrow().iter().filter(|n| n.to_lowercase().contains(&filter)) {
            SendMessageW(list, LB_ADDSTRING, WPARAM(0), LPARAM(HSTRING::from(name).as_ptr() as isize));
        }
        SendMessageW(list, WM_SETREDRAW, WPARAM(1), LPARAM(0));
        let _ = InvalidateRect(list, None, true);
    }

    /// Adds the picked process (or the only one left by the filter) and closes the picker.
    unsafe fn pick_process(&self) {
        let picker = self.picker.get();
        let list = GetDlgItem(picker, ID_PROCESSES).unwrap_or_default();
        let mut index = SendMessageW(list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
        if index < 0 && SendMessageW(list, LB_GETCOUNT, WPARAM(0), LPARAM(0)).0 == 1 {
            index = 0;
        }
        let length = SendMessageW(list, LB_GETTEXTLEN, WPARAM(index as usize), LPARAM(0)).0;
        if index < 0 || length <= 0 {
            return;
        }
        let mut text = vec![0u16; length as usize + 1];
        SendMessageW(list, LB_GETTEXT, WPARAM(index as usize), LPARAM(text.as_mut_ptr() as isize));
        let _ = DestroyWindow(picker);
        self.add_game(&String::from_utf16_lossy(&text[..length as usize]));
    }
}

// ── Window procedures ─────────────────────────────────────────────────────

/// The `App` a window was created with, once WM_NCCREATE has stored it.
unsafe fn app_of(hwnd: HWND, msg: u32, lp: LPARAM) -> Option<&'static App> {
    if msg == WM_NCCREATE {
        let create = &*(lp.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const App).as_ref()
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(app) = app_of(hwnd, msg, lp) else {
        return DefWindowProcW(hwnd, msg, wp, lp);
    };
    match msg {
        WM_CREATE => {
            app.hwnd.set(hwnd);
            app.create_controls(hwnd, MAIN);
            app.make_fonts(hwnd);
            app.layout(hwnd, MAIN, MAIN_SIZE, MAIN_STYLE);
            if app.config.borrow().restore_on_exit {
                let _ = CheckDlgButton(hwnd, ID_RESTORE, BST_CHECKED);
            }
            app.fill_games(None);
            app.refresh();
        }
        WM_COMMAND => app.command((wp.0 & 0xFFFF) as i32, (wp.0 >> 16) as u32 & 0xFFFF),
        WM_TRAY => match lp.0 as u32 {
            // Always show, never toggle: a double-click sends two of these.
            WM_LBUTTONUP => app.show(),
            WM_RBUTTONUP => app.tray_menu(),
            _ => {}
        },
        WM_WATCH => {
            let text = *Box::from_raw(lp.0 as *mut String);
            match wp.0 {
                watch::STARTED => app.log(&format!("{text} started")),
                watch::STOPPED => app.log(&format!("{text} exited")),
                watch::READY => {
                    app.watching.set(true);
                    for name in watch::set_watch(app.game_names()) {
                        app.log(&format!("{name} is already running"));
                    }
                }
                _ => {
                    app.log(&format!("[!] {text}"));
                    // A session that was up and got ended by something else is started
                    // again. One that never came up (no administrator rights) is not.
                    if app.watching.replace(false) {
                        // Sooner than the restore delay, so a running game is found
                        // again before HDR would be put back.
                        SetTimer(hwnd, TIMER_WATCH, 1000, None);
                    }
                }
            }
            app.sync_hdr();
        }
        WM_SHOW => app.show(),
        WM_TIMER if wp.0 == TIMER_RESTORE => app.restore(),
        WM_TIMER if wp.0 == TIMER_TRAY => app.refresh(),
        WM_TIMER if wp.0 == TIMER_WATCH => {
            let _ = KillTimer(hwnd, TIMER_WATCH);
            app.log("Watching for games again");
            watch::spawn(hwnd);
        }
        // HDR was switched by us, by Win+Alt+B or in Settings, or a monitor came or went.
        WM_DISPLAYCHANGE => app.sync_hdr(),
        WM_DPICHANGED => {
            // shortcut: an open picker keeps its old size, it is closed instead; re-lay it out if that annoys.
            let _ = DestroyWindow(app.picker.get());
            let to = &*(lp.0 as *const RECT);
            let _ = SetWindowPos(hwnd, None, to.left, to.top, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            app.make_fonts(hwnd);
            app.layout(hwnd, MAIN, MAIN_SIZE, MAIN_STYLE);
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX => return app.colors(msg, wp, lp),
        // Delete in the games list removes the selected game.
        WM_VKEYTOITEM if (wp.0 & 0xFFFF) as u16 == VK_DELETE.0 => {
            app.remove_selected_game();
            return LRESULT(-2);
        }
        WM_VKEYTOITEM => return LRESULT(-1),
        // The close button only hides the window; Exit is in the tray menu.
        WM_CLOSE => app.hide(),
        // Logoff and shutdown, and also an installer asking the app to close: exit either way.
        WM_ENDSESSION if wp.0 != 0 => {
            app.shut_down();
            let _ = DestroyWindow(hwnd);
        }
        WM_DESTROY => PostQuitMessage(0),
        _ if msg == app.taskbar_created => app.refresh(),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

unsafe extern "system" fn picker_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(app) = app_of(hwnd, msg, lp) else {
        return DefWindowProcW(hwnd, msg, wp, lp);
    };
    match msg {
        WM_CREATE => {
            app.picker.set(hwnd);
            app.create_controls(hwnd, PICKER);
            app.layout(hwnd, PICKER, PICKER_SIZE, PICKER_STYLE);
            let filter = GetDlgItem(hwnd, ID_FILTER).unwrap_or_default();
            SendMessageW(filter, EM_SETCUEBANNER, WPARAM(1), LPARAM(w!("Type to filter").as_ptr() as isize));
        }
        // Reload on every activation: the usual flow is open this, start the game, come back.
        WM_ACTIVATE if wp.0 & 0xFFFF != WA_INACTIVE as usize => {
            app.load_processes();
            let _ = SetFocus(GetDlgItem(hwnd, ID_FILTER).unwrap_or_default());
        }
        WM_COMMAND => match ((wp.0 & 0xFFFF) as i32, (wp.0 >> 16) as u32 & 0xFFFF) {
            (ID_FILTER, EN_CHANGE) => app.filter_processes(),
            (ID_PROCESSES, LBN_DBLCLK) | (ID_OK, BN_CLICKED) => app.pick_process(),
            (ID_CANCEL, BN_CLICKED) => {
                let _ = DestroyWindow(hwnd);
            }
            _ => {}
        },
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX | WM_CTLCOLOREDIT => return app.colors(msg, wp, lp),
        // shortcut: on a monitor with another scale the picker closes instead of resizing; re-lay it out if that annoys.
        WM_DPICHANGED => {
            let _ = DestroyWindow(hwnd);
        }
        WM_DESTROY => app.picker.set(HWND::default()),
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

// ── Helpers ───────────────────────────────────────────────────────────────

unsafe fn window_text(hwnd: HWND) -> String {
    let mut text = [0u16; 260];
    let length = GetWindowTextW(hwnd, &mut text);
    String::from_utf16_lossy(&text[..length as usize])
}

/// File dialog for a game's exe. Returns the file name only: that is what gets matched.
unsafe fn browse_for_exe(owner: HWND) -> Option<String> {
    let mut path = [0u16; 1024];
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: w!("Programs (*.exe)\0*.exe\0"),
        lpstrFile: PWSTR(path.as_mut_ptr()),
        nMaxFile: path.len() as u32,
        lpstrTitle: w!("Choose the game's exe"),
        Flags: OFN_FILEMUSTEXIST | OFN_HIDEREADONLY | OFN_NOCHANGEDIR | OFN_DONTADDTORECENT,
        ..Default::default()
    };
    if !GetOpenFileNameW(&mut dialog).as_bool() {
        return None;
    }
    let path = String::from_utf16_lossy(path.split(|&c| c == 0).next()?);
    path.rsplit(['\\', '/']).next().map(str::to_string)
}

/// The tray and window icon: a rounded 32x32 tile with a white H, drawn here so the
/// project carries no image files. Blue while HDR is on, grey while it is off.
unsafe fn make_icon(tile: [u8; 3]) -> HICON {
    let mut bgra = [0u8; 32 * 32 * 4];
    for (i, px) in bgra.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((i % 32) as f32 + 0.5, (i / 32) as f32 + 0.5);
        // Distance past the rounded edge (corner radius 7), turned into one pixel of anti-aliasing.
        let (dx, dy) = ((x - 16.0).abs() - 9.0, (y - 16.0).abs() - 9.0);
        let outside = dx.max(0.0).hypot(dy.max(0.0)) - 7.0;
        let alpha = (0.5 - outside).clamp(0.0, 1.0);
        let (col, row) = (i % 32, i / 32);
        let stem = (9..=11).contains(&col) || (20..=22).contains(&col);
        let glyph = (8..24).contains(&row) && (stem || ((15..=16).contains(&row) && (9..=22).contains(&col)));
        let [r, g, b] = if glyph { [255, 255, 255] } else { tile };
        px.copy_from_slice(&[b, g, r, (alpha * 255.0) as u8]);
    }
    let mask = [0u8; 32 * 32 / 8];
    CreateIcon(None, 32, 32, 1, 32, mask.as_ptr(), bgra.as_ptr()).unwrap_or_default()
}

/// Makes Windows draw this app's menus, scrollbars and "DarkMode_*" themed controls dark.
/// SetPreferredAppMode and FlushMenuThemes are undocumented uxtheme exports (ordinals 135
/// and 136, Windows 10 1903+) that Explorer and most dark Win32 apps rely on; there is no
/// documented way to get a dark popup menu. If they ever go away the UI is merely lighter.
unsafe fn force_dark_mode() {
    let Ok(uxtheme) = GetModuleHandleW(w!("uxtheme.dll")) else {
        return;
    };
    if let Some(set_preferred_app_mode) = GetProcAddress(uxtheme, PCSTR(135 as *const u8)) {
        const FORCE_DARK: i32 = 2;
        type Exported = unsafe extern "system" fn() -> isize;
        type SetPreferredAppMode = unsafe extern "system" fn(i32) -> i32;
        std::mem::transmute::<Exported, SetPreferredAppMode>(set_preferred_app_mode)(FORCE_DARK);
    }
    if let Some(flush_menu_themes) = GetProcAddress(uxtheme, PCSTR(136 as *const u8)) {
        flush_menu_themes();
    }
}

// ── Entry point ───────────────────────────────────────────────────────────

pub fn run() {
    unsafe {
        // One copy only: a second launch shows the first one's window and leaves.
        // Failing with "access denied" means an elevated copy owns it and this one isn't.
        let instance = CreateMutexW(None, false, w!("Local\\HDRify"));
        if instance.is_err() || GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(first) = FindWindowW(w!("HdrifyWnd"), None) {
                let _ = AllowSetForegroundWindow(ASFW_ANY);
                let _ = PostMessageW(first, WM_SHOW, WPARAM(0), LPARAM(0));
            }
            return;
        }

        force_dark_mode();
        let (config, config_problem) = Config::load(&crate::config::dir());
        let app: &'static App = Box::leak(Box::new(App {
            hwnd: Cell::default(),
            picker: Cell::default(),
            config: RefCell::new(config),
            saved: RefCell::new(None),
            hdr_on: Cell::new(false),
            restore_owed: Cell::new(false),
            watching: Cell::new(false),
            processes: RefCell::default(),
            fonts: Cell::default(),
            icons: [make_icon([112, 118, 128]), make_icon([24, 100, 230])],
            bg: CreateSolidBrush(BG),
            field: CreateSolidBrush(FIELD),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
        }));

        let instance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let procs: [(PCWSTR, WNDPROC); 2] =
            [(w!("HdrifyWnd"), Some(wnd_proc)), (w!("HdrifyPicker"), Some(picker_proc))];
        for (name, proc) in procs {
            RegisterClassExW(&WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: proc,
                hInstance: instance,
                hIcon: app.icons[1],
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: app.bg,
                lpszClassName: name,
                ..Default::default()
            });
        }
        let created = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("HdrifyWnd"),
            w!("HDRify"),
            MAIN_STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            None,
            None,
            instance,
            Some(app as *const App as *const _),
        );
        let Ok(hwnd) = created else {
            log::write("[!] HDRify could not create its window");
            return;
        };

        // Explorer is not elevated and the release build is; let its "the taskbar is back"
        // broadcast through whatever the message filter's default is.
        let _ = ChangeWindowMessageFilterEx(hwnd, app.taskbar_created, MSGFLT_ALLOW, None);

        app.log(&format!("HDRify {} started", env!("CARGO_PKG_VERSION")));
        if let Some(problem) = config_problem {
            app.log(&format!("[!] {problem}"));
        }
        watch::set_watch(app.game_names());
        watch::spawn(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            // IsDialogMessage gives both windows Tab, arrow keys, Enter and Esc.
            if !IsDialogMessageW(GetAncestor(msg.hwnd, GA_ROOT), &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
