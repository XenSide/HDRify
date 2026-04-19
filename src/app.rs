use crate::config::GameEntry;
use crate::manager::{AppCmd, AppEvent, AppState};
use crate::{HDR_EXIT, HDR_HIDE, HDR_MENU, HDR_PICKER_DONE, HDR_SHOW, HDR_TOGGLE_VIS, HDR_UPDATE, WM_HDR};
use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    TrayIcon, TrayIconBuilder, TrayIconEvent,
};
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::Controls::SetWindowTheme,
        UI::WindowsAndMessaging::*,
    },
};

// Control IDs for the main settings window
const IDC_STATUS:    i32 = 1001;
const IDC_LOG:       i32 = 1002;
const IDC_RESTORE:   i32 = 1003;
const IDC_TOGGLE:    i32 = 1004;
const IDC_GAME_LIST: i32 = 1005;
const IDC_REMOVE:    i32 = 1006;
const IDC_ADD_PROC:  i32 = 1007;
const IDC_ADD_FILE:  i32 = 1008;

const BS_GROUPBOX_VAL: u32 = 7;

const DARK_BG_COLOR: COLORREF = COLORREF(0x001A1A1A);
const DARK_PANEL_COLOR: COLORREF = COLORREF(0x002A2A2A);

static mut DARK_BG_BRUSH: HBRUSH = HBRUSH(0 as _);
static mut DARK_PANEL_BRUSH: HBRUSH = HBRUSH(0 as _);

// Font weight constants
const FW_BOLD: i32 = 700;

// Shared queue: menu forwarder thread pushes MenuId here then posts HDR_MENU
type MenuQueue = Arc<Mutex<VecDeque<MenuId>>>;

// Per-window data stored via SetWindowLongPtrW(GWLP_USERDATA).
// Only ever accessed from the main thread (WndProc is always called on the
// thread that owns the window), so non-Send/Sync fields are fine here.
struct WinCtx {
    hinstance:   HINSTANCE,
    shared:      Arc<Mutex<AppState>>,
    event_tx:    mpsc::Sender<AppEvent>,
    menu_queue:  MenuQueue,
    // Tray items we need to update when state changes
    tray_status:  MenuItem,
    tray_restore: CheckMenuItem,
    tray_toggle:  MenuItem,
    _tray:        TrayIcon,  // keep alive
    // Menu IDs to recognise in HDR_MENU handler
    open_id:    MenuId,
    exit_id:    MenuId,
    restore_id: MenuId,
    toggle_id:  MenuId,
    // HWND of the currently open process picker (null when closed)
    picker_hwnd: HWND,
}

// Per-window data for the process picker popup
struct PickerCtx {
    main_hwnd:    HWND,
    event_tx:     mpsc::Sender<AppEvent>,
    all_processes: Vec<String>,
    filter_edit:  HWND,
    proc_list:    HWND,
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn apply_gui_font(hwnd: HWND) {
    let font = GetStockObject(DEFAULT_GUI_FONT);
    SendMessageW(hwnd, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
}

/// Create a child control and apply the system GUI font.
unsafe fn make_ctrl(
    parent: HWND, class: PCWSTR, text: PCWSTR,
    extra: u32, x: i32, y: i32, w: i32, h: i32,
    id: i32, hi: HINSTANCE,
) -> HWND {
    let style = WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | extra);
    let ctrl = CreateWindowExW(
        WINDOW_EX_STYLE(0), class, text, style,
        x, y, w, h,
        parent, HMENU(id as isize as *mut _), hi, None,
    ).unwrap_or(HWND(std::ptr::null_mut()));
    if !ctrl.0.is_null() {
        apply_gui_font(ctrl);
        // Disable visual styles for GroupBoxes and CheckBoxes so they respect WM_CTLCOLORSTATIC
        if extra == BS_GROUPBOX_VAL || extra == BS_AUTOCHECKBOX_VAL {
            let _ = SetWindowTheme(ctrl, w!(" "), w!(" "));
        }
        // Make status label bold and larger
        if id == IDC_STATUS {
            let mut lf: LOGFONTW = std::mem::zeroed();
            lf.lfHeight = -14; // Larger font
            lf.lfWeight = FW_BOLD; // Bold
            lf.lfFaceName = [b'M' as u16, b'S' as u16, b' ' as u16, b'S' as u16, b'h' as u16, b'e' as u16, b'l' as u16, b'l' as u16, b' ' as u16, b'D' as u16, b'l' as u16, b'g' as u16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            let font = CreateFontIndirectW(&lf);
            if !font.0.is_null() {
                SendMessageW(ctrl, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            }
        } else {
            // Check if this is a section header by examining the text
            let text_str = String::from_utf16_lossy(std::slice::from_raw_parts(text.as_ptr(), {
                let mut len = 0;
                while *text.as_ptr().offset(len) != 0 { len += 1; }
                len as usize
            }));
            if text_str == "HDR Control" || text_str == "Game Management" || text_str == "Activity Log" {
                // Make section headers bold
                let mut lf: LOGFONTW = std::mem::zeroed();
                lf.lfHeight = -12;
                lf.lfWeight = FW_BOLD;
                lf.lfFaceName = [b'M' as u16, b'S' as u16, b' ' as u16, b'S' as u16, b'h' as u16, b'e' as u16, b'l' as u16, b'l' as u16, b' ' as u16, b'D' as u16, b'l' as u16, b'g' as u16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
                let font = CreateFontIndirectW(&lf);
                if !font.0.is_null() {
                    SendMessageW(ctrl, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
                }
            }
        }
    }
    ctrl
}

/// Get the text of the currently selected item in a listbox. Returns None if
/// nothing is selected or the index is LB_ERR.
// Raw control-style constants (plain i32 in windows 0.58, so no .0 accessor)
const BST_CHECKED_VAL:   usize = 1;
const BST_UNCHECKED_VAL: usize = 0;
const BS_AUTOCHECKBOX_VAL: u32 = 3;
const BS_PUSHBUTTON_VAL:   u32 = 0;
const LBS_NOSEL_VAL:             u32 = 0x4000;
const LBS_NOTIFY_VAL:            u32 = 0x0001;
const LBS_NOINTEGRALHEIGHT_VAL:  u32 = 0x0100;
const ES_AUTOHSCROLL_VAL:        u32 = 0x0080;

/// Unwrapping wrapper for GetDlgItem — returns HWND::default() if not found.
unsafe fn dlg(parent: HWND, id: i32) -> HWND {
    GetDlgItem(parent, id).unwrap_or_default()
}

unsafe fn get_list_sel_text(list: HWND) -> Option<String> {
    let idx = SendMessageW(list, LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
    if idx == LB_ERR as isize { return None; }
    let len = SendMessageW(list, LB_GETTEXTLEN, WPARAM(idx as usize), LPARAM(0)).0;
    if len <= 0 { return None; }
    let mut buf = vec![0u16; (len + 1) as usize];
    SendMessageW(list, LB_GETTEXT, WPARAM(idx as usize), LPARAM(buf.as_mut_ptr() as isize));
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

// ── Tray icon ────────────────────────────────────────────────────────────

fn tray_rgba() -> Vec<u8> {
    let mut px = vec![0u8; 32 * 32 * 4];
    for c in px.chunks_mut(4) { c[0] = 24; c[1] = 100; c[2] = 230; c[3] = 255; }
    let mut set = |x: usize, y: usize| {
        let i = (y * 32 + x) * 4;
        px[i] = 255; px[i+1] = 255; px[i+2] = 255; px[i+3] = 255;
    };
    for y in 8..24usize { set(9,y); set(10,y); set(21,y); set(22,y); }
    for x in 9..23usize { set(x,15); set(x,16); }
    px
}

type TrayParts = (TrayIcon, MenuItem, CheckMenuItem, MenuItem, MenuId, MenuId, MenuId, MenuId);

fn build_tray(restore_on_exit: bool) -> TrayParts {
    let status_item  = MenuItem::new("- Idle", false, None);
    let restore_item = CheckMenuItem::new("Restore HDR on exit", true, restore_on_exit, None);
    let toggle_item  = MenuItem::new("Enable HDR now", true, None);
    let open_item    = MenuItem::new("Open Settings\u{2026}", true, None);
    let exit_item    = MenuItem::new("Exit", true, None);

    let open_id    = open_item.id().clone();
    let exit_id    = exit_item.id().clone();
    let restore_id = restore_item.id().clone();
    let toggle_id  = toggle_item.id().clone();

    let menu = Menu::new();
    menu.append_items(&[
        &status_item, &PredefinedMenuItem::separator(),
        &restore_item, &toggle_item, &PredefinedMenuItem::separator(),
        &open_item, &exit_item,
    ]).expect("menu append");

    let icon = tray_icon::Icon::from_rgba(tray_rgba(), 32, 32).expect("tray icon");
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("HDRify")
        .with_icon(icon)
        .build()
        .expect("tray build");

    (tray, status_item, restore_item, toggle_item, open_id, exit_id, restore_id, toggle_id)
}

// ── Window class registration ─────────────────────────────────────────────

unsafe fn register_classes(hi: HINSTANCE) {
    let cursor = LoadCursorW(HINSTANCE(std::ptr::null_mut()), IDC_ARROW).unwrap_or_default();
    DARK_BG_BRUSH = CreateSolidBrush(DARK_BG_COLOR);
    DARK_PANEL_BRUSH = CreateSolidBrush(DARK_PANEL_COLOR);

    let wc = WNDCLASSEXW {
        cbSize:        std::mem::size_of::<WNDCLASSEXW>() as u32,
        style:         CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc:   Some(wnd_proc),
        hInstance:     hi,
        hCursor:       cursor,
        hbrBackground: DARK_BG_BRUSH,
        lpszClassName: w!("HdrifyWnd"),
        ..Default::default()
    };
    RegisterClassExW(&wc);

    let wc2 = WNDCLASSEXW {
        cbSize:        std::mem::size_of::<WNDCLASSEXW>() as u32,
        style:         CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc:   Some(picker_proc),
        hInstance:     hi,
        hCursor:       cursor,
        hbrBackground: DARK_BG_BRUSH,
        lpszClassName: w!("HdrifyPicker"),
        ..Default::default()
    };
    RegisterClassExW(&wc2);
}

// ── Controls ──────────────────────────────────────────────────────────────

unsafe fn create_controls(hwnd: HWND, hi: HINSTANCE) {
    // Status label (text set dynamically; coloured green when HDR is active)
    make_ctrl(hwnd, w!("STATIC"), w!("Idle"), 0, 20, 20, 460, 24, IDC_STATUS, hi);

    // Activity log group
    make_ctrl(hwnd, w!("BUTTON"), w!("Activity Log"), BS_GROUPBOX_VAL, 20, 55, 460, 155, 0, hi);
    make_ctrl(hwnd, w!("LISTBOX"), w!("") ,
        LBS_NOSEL_VAL | LBS_NOINTEGRALHEIGHT_VAL | WS_VSCROLL.0 | WS_BORDER.0,
        30, 75, 440, 110, IDC_LOG, hi);

    // HDR Control group
    make_ctrl(hwnd, w!("BUTTON"), w!("HDR Control"), BS_GROUPBOX_VAL, 20, 230, 460, 120, 0, hi);
    make_ctrl(hwnd, w!("BUTTON"), w!("Restore HDR when games exit"),
        BS_AUTOCHECKBOX_VAL, 30, 255, 320, 22, IDC_RESTORE, hi);
    make_ctrl(hwnd, w!("BUTTON"), w!("Enable HDR Now"),
        BS_PUSHBUTTON_VAL, 30, 290, 200, 32, IDC_TOGGLE, hi);

    // Game Management group
    make_ctrl(hwnd, w!("BUTTON"), w!("Game Management"), BS_GROUPBOX_VAL, 20, 370, 460, 145, 0, hi);
    make_ctrl(hwnd, w!("LISTBOX"), w!("") ,
        LBS_NOTIFY_VAL | LBS_NOINTEGRALHEIGHT_VAL | WS_VSCROLL.0 | WS_BORDER.0,
        30, 395, 440, 100, IDC_GAME_LIST, hi);

    // Action buttons — perfectly symmetrical alignment with GroupBoxes
    // Left button (120px) aligns with the left edge of the GroupBox (x=20)
    make_ctrl(hwnd, w!("BUTTON"), w!("Remove Selected"),
        BS_PUSHBUTTON_VAL, 20, 525, 120, 32, IDC_REMOVE, hi);
    // Center button (200px) sits perfectly in the middle with a 10px gap
    make_ctrl(hwnd, w!("BUTTON"), w!("Add from Running Processes..."),
        BS_PUSHBUTTON_VAL, 150, 525, 200, 32, IDC_ADD_PROC, hi);
    // Right button (120px) aligns with the right edge of the GroupBox (x=480)
    make_ctrl(hwnd, w!("BUTTON"), w!("Add from File..."),
        BS_PUSHBUTTON_VAL, 360, 525, 120, 32, IDC_ADD_FILE, hi);
}

// ── State → UI sync ───────────────────────────────────────────────────────

/// Read shared AppState and push every value to the Win32 controls and tray.
/// Must be called from the main thread (WndProc context).
unsafe fn update_controls(hwnd: HWND, ctx: &mut WinCtx) {
    // Snapshot under lock; drop before any Win32 calls to avoid priority inversion.
    let (hdr_on, active, messages, games, restore) = {
        let s = ctx.shared.lock().unwrap();
        (s.hdr_on(), s.active_count, s.messages.clone(), s.config.games.clone(), s.config.restore_on_exit)
    };

    // Status label
    let status = if hdr_on {
        if active > 0 { format!("HDR active \u{2014} {} game(s) running", active) }
        else { "HDR active \u{2014} enabled manually".into() }
    } else { "Idle".into() };
    let sw = to_wide(&status);
    let _ = SetWindowTextW(dlg(hwnd, IDC_STATUS), PCWSTR(sw.as_ptr()));
    InvalidateRect(dlg(hwnd, IDC_STATUS), None, true); // repaint for colour change

    // Toggle button text
    let tw = to_wide(if hdr_on { "Disable HDR now" } else { "Enable HDR now" });
    let _ = SetWindowTextW(dlg(hwnd, IDC_TOGGLE), PCWSTR(tw.as_ptr()));

    // Restore checkbox
    let chk = if restore { BST_CHECKED_VAL } else { BST_UNCHECKED_VAL };
    SendMessageW(dlg(hwnd, IDC_RESTORE), BM_SETCHECK, WPARAM(chk), LPARAM(0));

    // Log listbox: only repopulate if message count changed (avoids flicker)
    let log = dlg(hwnd, IDC_LOG);
    if SendMessageW(log, LB_GETCOUNT, WPARAM(0), LPARAM(0)).0 as usize != messages.len() {
        SendMessageW(log, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
        for m in &messages {
            let w = to_wide(m);
            SendMessageW(log, LB_ADDSTRING, WPARAM(0), LPARAM(w.as_ptr() as isize));
        }
        if !messages.is_empty() {
            SendMessageW(log, LB_SETTOPINDEX, WPARAM(messages.len() - 1), LPARAM(0));
        }
    }

    // Game list (always repopulate; list is small)
    let gl = dlg(hwnd, IDC_GAME_LIST);
    SendMessageW(gl, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    for g in &games {
        let t = to_wide(&format!("{} ({})", g.display_name, g.exe_name));
        SendMessageW(gl, LB_ADDSTRING, WPARAM(0), LPARAM(t.as_ptr() as isize));
    }

    // Tray items
    let tray_label = if hdr_on {
        if active > 0 { format!("HDR active ({} game(s))", active) } else { "HDR active (manual)".into() }
    } else { "- Idle".into() };
    let _ = ctx.tray_status.set_text(&tray_label);
    let _ = ctx.tray_toggle.set_text(if hdr_on { "Disable HDR now" } else { "Enable HDR now" });
    ctx.tray_restore.set_checked(restore);
}

// ── Main window procedure ─────────────────────────────────────────────────

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Helper: borrow the per-window context. Returns None before WM_CREATE sets it.
    macro_rules! ctx {
        () => {{
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WinCtx;
            if p.is_null() { return DefWindowProcW(hwnd, msg, wp, lp); }
            &mut *p
        }};
    }

    match msg {
        WM_CREATE => {
            let cs = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            let ctx = &mut *(cs.lpCreateParams as *mut WinCtx);
            create_controls(hwnd, ctx.hinstance);
            update_controls(hwnd, ctx);
            LRESULT(0)
        }

        WM_COMMAND => {
            let ctx = ctx!();
            let id   = (wp.0 & 0xFFFF) as i32;
            let note = ((wp.0 >> 16) & 0xFFFF) as u32;
            match (id, note) {
                (IDC_TOGGLE, BN_CLICKED) => {
                    ctx.event_tx.send(AppEvent::Cmd(AppCmd::ToggleHdr)).ok();
                }
                (IDC_RESTORE, BN_CLICKED) => {
                    // BS_AUTOCHECKBOX already toggled state; read new value
                    let v = SendMessageW(dlg(hwnd, IDC_RESTORE), BM_GETCHECK, WPARAM(0), LPARAM(0));
                    ctx.event_tx.send(AppEvent::Cmd(AppCmd::SetRestoreOnExit(v.0 == 1))).ok();
                }
                (IDC_REMOVE, BN_CLICKED) => {
                    let idx = SendMessageW(dlg(hwnd, IDC_GAME_LIST), LB_GETCURSEL, WPARAM(0), LPARAM(0)).0;
                    if idx != LB_ERR as isize {
                        ctx.event_tx.send(AppEvent::Cmd(AppCmd::RemoveGame(idx as usize))).ok();
                    }
                }
                (IDC_ADD_PROC, BN_CLICKED) => {
                    if ctx.picker_hwnd.0.is_null() || !IsWindow(ctx.picker_hwnd).as_bool() {
                        ctx.picker_hwnd = open_picker(hwnd, ctx.event_tx.clone(), ctx.hinstance);
                    } else {
                        SetForegroundWindow(ctx.picker_hwnd);
                    }
                }
                (IDC_ADD_FILE, BN_CLICKED) => {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Executable", &["exe"])
                        .set_title("Select Game Executable").pick_file()
                    {
                        let exe = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let display = p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| exe.clone());
                        ctx.event_tx.send(AppEvent::Cmd(AppCmd::AddGame(GameEntry { display_name: display, exe_name: exe }))).ok();
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }

        WM_HDR => {
            if wp.0 == HDR_TOGGLE_VIS {
                if IsWindowVisible(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                } else {
                    let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
                    let _ = SetForegroundWindow(hwnd);
                    update_controls(hwnd, ctx!());
                }
                return LRESULT(0);
            }
            let ctx = ctx!();
            match wp.0 {
                x if x == HDR_UPDATE => { update_controls(hwnd, ctx); }
                x if x == HDR_SHOW   => { let _ = ShowWindow(hwnd, SW_SHOWNORMAL); let _ = SetForegroundWindow(hwnd); update_controls(hwnd, ctx); }
                x if x == HDR_HIDE   => { let _ = ShowWindow(hwnd, SW_HIDE); }
                x if x == HDR_PICKER_DONE => { ctx.picker_hwnd = HWND(std::ptr::null_mut()); update_controls(hwnd, ctx); }
                x if x == HDR_EXIT => {
                    let (restore, saved) = {
                        let mut s = ctx.shared.lock().unwrap();
                        let saved = if s.config.restore_on_exit { s.saved_states.take() } else { None };
                        (s.config.restore_on_exit, saved)
                    };
                    if restore { if let Some(states) = saved { crate::hdr::restore(&states); } }
                    let _ = DestroyWindow(hwnd);
                }
                x if x == HDR_MENU => {
                    let ids: Vec<MenuId> = { let mut q = ctx.menu_queue.lock().unwrap(); q.drain(..).collect() };
                    for id in ids {
                        if id == ctx.open_id {
                            ShowWindow(hwnd, SW_SHOWNORMAL); SetForegroundWindow(hwnd); update_controls(hwnd, ctx);
                        } else if id == ctx.exit_id {
                            SendMessageW(hwnd, WM_HDR, WPARAM(HDR_EXIT), LPARAM(0));
                        } else if id == ctx.restore_id {
                            { let mut s = ctx.shared.lock().unwrap(); s.config.restore_on_exit = !s.config.restore_on_exit; s.config.save(); }
                            update_controls(hwnd, ctx);
                        } else if id == ctx.toggle_id {
                            ctx.event_tx.send(AppEvent::Cmd(AppCmd::ToggleHdr)).ok();
                        }
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }

        // Colour static controls and groupbox text with white and transparent backgrounds
        WM_CTLCOLORSTATIC => {
            let hdc = HDC(wp.0 as *mut _);
            SetTextColor(hdc, COLORREF(0x00FFFFFF)); // Pure white
            SetBkColor(hdc, DARK_BG_COLOR);
            return LRESULT(DARK_BG_BRUSH.0 as isize);
        }

        WM_CTLCOLORBTN => {
            let hdc = HDC(wp.0 as *mut _);
            SetTextColor(hdc, COLORREF(0x00FFFFFF)); // Pure white for groupbox and checkbox text
            SetBkColor(hdc, DARK_BG_COLOR);
            return LRESULT(DARK_BG_BRUSH.0 as isize);
        }

        WM_CTLCOLORLISTBOX => {
            let hdc = HDC(wp.0 as *mut _);
            SetTextColor(hdc, COLORREF(0x00FFFFFF)); // Pure white
            SetBkColor(hdc, DARK_PANEL_COLOR);
            return LRESULT(DARK_PANEL_BRUSH.0 as isize);
        }

        // X button hides instead of destroying (app lives in tray)
        WM_CLOSE => { ShowWindow(hwnd, SW_HIDE); LRESULT(0) }

        WM_NCDESTROY => {
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WinCtx;
            if !p.is_null() { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0); drop(Box::from_raw(p)); }
            PostQuitMessage(0);
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wp, lp)
    }
}

// ── Process picker popup ──────────────────────────────────────────────────

unsafe fn open_picker(parent: HWND, event_tx: mpsc::Sender<AppEvent>, hi: HINSTANCE) -> HWND {
    let ctx = Box::new(PickerCtx {
        main_hwnd: parent,
        event_tx,
        all_processes: vec![],
        filter_edit: HWND(std::ptr::null_mut()),
        proc_list:   HWND(std::ptr::null_mut()),
    });
    CreateWindowExW(
        WS_EX_TOOLWINDOW,
        w!("HdrifyPicker"), w!("Add Game"),
        WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0 | WS_VISIBLE.0),
        CW_USEDEFAULT, CW_USEDEFAULT, 380, 460,
        parent, HMENU(std::ptr::null_mut()), hi,
        Some(Box::into_raw(ctx) as *const _),
    ).unwrap_or(HWND(std::ptr::null_mut()))
}

unsafe fn refresh_picker(ctx: &mut PickerCtx, filter: &str) {
    use sysinfo::System;
    ctx.all_processes.clear();
    let mut sys = System::new();
    sys.refresh_processes();
    let mut names: Vec<String> = sys.processes().values().map(|p| p.name().to_string()).collect();
    names.sort_unstable(); names.dedup();
    ctx.all_processes = names;
    populate_picker_list(ctx.proc_list, &ctx.all_processes, filter);
}

unsafe fn populate_picker_list(list: HWND, procs: &[String], filter: &str) {
    SendMessageW(list, LB_RESETCONTENT, WPARAM(0), LPARAM(0));
    let f = filter.to_lowercase();
    for name in procs {
        if f.is_empty() || name.to_lowercase().contains(&f) {
            let w = to_wide(name);
            SendMessageW(list, LB_ADDSTRING, WPARAM(0), LPARAM(w.as_ptr() as isize));
        }
    }
}

unsafe extern "system" fn picker_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    macro_rules! ctx {
        () => {{
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PickerCtx;
            if p.is_null() { return DefWindowProcW(hwnd, msg, wp, lp); }
            &mut *p
        }};
    }
    match msg {
        WM_CREATE => {
            let cs = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            let ctx = &mut *(cs.lpCreateParams as *mut PickerCtx);
            // Filter label + edit
            make_ctrl(hwnd, w!("STATIC"),  w!("Filter:"), 0, 10, 10, 40, 20, 0, HINSTANCE(std::ptr::null_mut()));
            let fe = make_ctrl(hwnd, w!("EDIT"), w!(""),
                ES_AUTOHSCROLL_VAL | WS_BORDER.0,
                55, 8, 200, 22, 2001, HINSTANCE(std::ptr::null_mut()));
            make_ctrl(hwnd, w!("BUTTON"), w!("Refresh"), BS_PUSHBUTTON_VAL, 262, 8, 55, 22, 2002, HINSTANCE(std::ptr::null_mut()));
            make_ctrl(hwnd, w!("BUTTON"), w!("Close"),   BS_PUSHBUTTON_VAL, 322, 8, 46, 22, 2003, HINSTANCE(std::ptr::null_mut()));
            // Process list
            let pl = make_ctrl(hwnd, w!("LISTBOX"), w!(""),
                LBS_NOTIFY_VAL | LBS_NOINTEGRALHEIGHT_VAL | WS_VSCROLL.0 | WS_BORDER.0,
                10, 38, 352, 378, 2004, HINSTANCE(std::ptr::null_mut()));
            ctx.filter_edit = fe;
            ctx.proc_list   = pl;
            refresh_picker(ctx, "");
            LRESULT(0)
        }

        WM_COMMAND => {
            let ctx = ctx!();
            let id   = (wp.0 & 0xFFFF) as i32;
            let note = ((wp.0 >> 16) & 0xFFFF) as u32;

            // Get current filter text
            let filter_len = GetWindowTextLengthW(ctx.filter_edit);
            let mut fbuf = vec![0u16; (filter_len + 1) as usize];
            GetWindowTextW(ctx.filter_edit, &mut fbuf);
            let filter = String::from_utf16_lossy(&fbuf[..filter_len as usize]);

            match (id, note) {
                (2001, EN_CHANGE) => { populate_picker_list(ctx.proc_list, &ctx.all_processes, &filter); }
                (2002, BN_CLICKED) => { refresh_picker(ctx, &filter); }
                (2003, BN_CLICKED) => { let _ = DestroyWindow(hwnd); }
                (2004, v) if v == LBN_DBLCLK || (v == 0 && (wp.0 >> 16) == 0) => {
                    if let Some(exe) = get_list_sel_text(ctx.proc_list) {
                        let display = exe.strip_suffix(".exe").unwrap_or(&exe).to_string();
                        ctx.event_tx.send(AppEvent::Cmd(AppCmd::AddGame(GameEntry { display_name: display, exe_name: exe }))).ok();
                        let _ = DestroyWindow(hwnd);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }

        WM_NCDESTROY => {
            let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PickerCtx;
            if !p.is_null() {
                let main = (*p).main_hwnd;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(p));
                // Notify main window so it clears picker_hwnd and refreshes the game list
                let _ = PostMessageW(main, WM_HDR, WPARAM(HDR_PICKER_DONE), LPARAM(0));
            }
            LRESULT(0)
        }

        WM_CLOSE => { let _ = DestroyWindow(hwnd); LRESULT(0) }
        _ => DefWindowProcW(hwnd, msg, wp, lp)
    }
}

// ── Entry point ───────────────────────────────────────────────────────────

pub fn run() {
    let hm = unsafe { GetModuleHandleW(PCWSTR::null()).unwrap() };
    let hinstance: HINSTANCE = hm.into();
    let config    = crate::config::Config::load();
    let restore   = config.restore_on_exit;
    let shared    = Arc::new(Mutex::new(AppState::new(config)));

    let (event_tx, event_rx) = mpsc::channel::<AppEvent>();
    let menu_queue: MenuQueue = Arc::new(Mutex::new(VecDeque::new()));

    let (tray, tray_status, tray_restore, tray_toggle, open_id, exit_id, restore_id, toggle_id) =
        build_tray(restore);

    unsafe { register_classes(hinstance); }

    let win_ctx = Box::new(WinCtx {
        hinstance,
        shared:      Arc::clone(&shared),
        event_tx:    event_tx.clone(),
        menu_queue:  Arc::clone(&menu_queue),
        tray_status, tray_restore, tray_toggle, _tray: tray,
        open_id: open_id.clone(), exit_id: exit_id.clone(),
        restore_id:  restore_id.clone(), toggle_id: toggle_id.clone(),
        picker_hwnd: HWND(std::ptr::null_mut()),
    });

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW, w!("HdrifyWnd"), w!("HDRify"),
            WINDOW_STYLE(WS_CAPTION.0 | WS_SYSMENU.0),
            CW_USEDEFAULT, CW_USEDEFAULT, 515, 615,
            HWND(std::ptr::null_mut()), HMENU(std::ptr::null_mut()),
            hinstance, Some(Box::into_raw(win_ctx) as *const _),
        ).expect("CreateWindowExW failed")
    };

    let hwnd_val = hwnd.0 as isize;

    // Manager thread: processes AppEvent::Cmd and AppEvent::Process
    crate::manager::spawn(hwnd_val, event_rx, Arc::clone(&shared));

    // WMI process monitors
    let (wmi_tx, wmi_rx) = mpsc::channel();
    crate::wmi_monitor::spawn_monitors(wmi_tx, Arc::clone(&shared));

    // WMI forwarder: wraps ProcessEvents into AppEvents
    { let tx = event_tx.clone();
      std::thread::Builder::new().name("wmi-fwd".into()).spawn(move || {
          while let Ok(ev) = wmi_rx.recv() { let _ = tx.send(AppEvent::Process(ev)); }
      }).expect("wmi-fwd"); }

    // Tray left-click: toggle window visibility
    { std::thread::Builder::new().name("tray-fwd".into()).spawn(move || {
          loop { match TrayIconEvent::receiver().recv() {
              Ok(TrayIconEvent::Click { button: tray_icon::MouseButton::Left, .. }) => unsafe {
                  let _ = PostMessageW(HWND(hwnd_val as *mut _), WM_HDR, WPARAM(HDR_TOGGLE_VIS), LPARAM(0));
              },
              Ok(_) => {}, Err(_) => break,
          }}
      }).expect("tray-fwd"); }

    // Menu forwarder: pushes MenuId to queue then wakes WndProc
    { let mq = Arc::clone(&menu_queue);
      std::thread::Builder::new().name("menu-fwd".into()).spawn(move || {
          while let Ok(ev) = MenuEvent::receiver().recv() {
              mq.lock().unwrap().push_back(ev.id);
              unsafe { let _ = PostMessageW(HWND(hwnd_val as *mut _), WM_HDR, WPARAM(HDR_MENU), LPARAM(0)); }
          }
      }).expect("menu-fwd"); }

    // Standard Win32 message loop — blocks on GetMessageW, wakes only on real events.
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, HWND(std::ptr::null_mut()), 0, 0).as_bool() {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
