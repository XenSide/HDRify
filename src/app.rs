use crate::config::GameEntry;
use crate::manager::{AppCmd, AppEvent, AppState};
use crate::wmi_monitor;
use egui::{Context, Ui};
use std::sync::{Arc, Mutex};
use std::sync::mpsc;
use tray_icon::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    TrayIcon, TrayIconBuilder,
};

// ── icon ─────────────────────────────────────────────────────────────────────
fn make_icon_rgba() -> Vec<u8> {
    let mut px = vec![0u8; 32 * 32 * 4];
    for chunk in px.chunks_mut(4) {
        chunk[0] = 24;
        chunk[1] = 100;
        chunk[2] = 230;
        chunk[3] = 255;
    }
    let mut set = |x: usize, y: usize| {
        let i = (y * 32 + x) * 4;
        px[i] = 255;
        px[i + 1] = 255;
        px[i + 2] = 255;
        px[i + 3] = 255;
    };
    for y in 8..24usize {
        set(9, y);
        set(10, y);
        set(21, y);
        set(22, y);
    }
    for x in 9..23usize {
        set(x, 15);
        set(x, 16);
    }
    px
}

// ── Win32 taskbar visibility toggle ──────────────────────────────────────────
/// Show or hide the window in the Windows taskbar by toggling WS_EX_TOOLWINDOW
/// and WS_EX_APPWINDOW.  Called on the main thread only.
fn set_taskbar_visible(visible: bool) {
    #[cfg(windows)]
    unsafe {
        use windows::core::{w, PCWSTR};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{
            FindWindowW, GetWindowLongW, SetWindowLongW, SetWindowPos,
            GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
            SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        };

        let hwnd: HWND = FindWindowW(PCWSTR::null(), w!("HDRify"))
            .unwrap_or(HWND(std::ptr::null_mut()));
        if hwnd.0.is_null() {
            return;
        }
        let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let new_style = if visible {
            (ex_style & !WS_EX_TOOLWINDOW.0) | WS_EX_APPWINDOW.0
        } else {
            (ex_style | WS_EX_TOOLWINDOW.0) & !WS_EX_APPWINDOW.0
        };
        SetWindowLongW(hwnd, GWL_EXSTYLE, new_style as i32);
        let _ = SetWindowPos(
            hwnd,
            HWND(std::ptr::null_mut()),
            0, 0, 0, 0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
}

// ── App ───────────────────────────────────────────────────────────────────────
pub struct App {
    shared: Arc<Mutex<AppState>>,

    // Tray items use Rc internally — NOT Send — they must stay on the main thread.
    tray: TrayIcon,
    tray_status_item: MenuItem,
    tray_restore_item: CheckMenuItem,
    tray_toggle_item: MenuItem,

    picker_open: bool,
    process_list: Vec<String>,
    picker_filter: String,

    /// Sender for the unified manager event channel.  Used by push_cmd so
    /// UI actions reach the manager without polling a shared Vec.
    event_tx: mpsc::Sender<AppEvent>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = crate::config::Config::load();

        // ── tray menu ─────────────────────────────────────────────────────
        let status_item = MenuItem::new("- Idle", false, None);
        let restore_item =
            CheckMenuItem::new("Restore HDR on exit", true, config.restore_on_exit, None);
        let toggle_item = MenuItem::new("Enable HDR now", true, None);
        let open_item = MenuItem::new("Open Settings…", true, None);
        let exit_item = MenuItem::new("Exit", true, None);

        let restore_id = restore_item.id().clone();
        let toggle_id = toggle_item.id().clone();
        let open_id = open_item.id().clone();
        let exit_id = exit_item.id().clone();

        let menu = Menu::new();
        menu.append_items(&[
            &status_item,
            &PredefinedMenuItem::separator(),
            &restore_item,
            &toggle_item,
            &PredefinedMenuItem::separator(),
            &open_item,
            &exit_item,
        ])
        .expect("menu append");

        let rgba = make_icon_rgba();
        let icon = tray_icon::Icon::from_rgba(rgba, 32, 32).expect("tray icon");
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("HDRify")
            .with_icon(icon)
            .build()
            .expect("tray build");

        // ── shared state + channels ───────────────────────────────────────
        let shared = Arc::new(Mutex::new(AppState::new(config)));

        // WMI monitor sends ProcessEvents on its own channel.
        let (wmi_tx, wmi_rx) = mpsc::channel();
        wmi_monitor::spawn_monitors(wmi_tx, Arc::clone(&shared));

        // Unified AppEvent channel used by the manager and forwarder threads.
        // app.rs keeps event_tx to send UI commands without polling.
        let (event_tx, event_rx) = mpsc::channel::<AppEvent>();

        crate::manager::spawn(
            cc.egui_ctx.clone(),
            event_rx,
            wmi_rx,
            Arc::clone(&shared),
            open_id,
            exit_id,
            restore_id,
            toggle_id,
            event_tx.clone(),
        );

        App {
            shared,
            tray,
            tray_status_item: status_item,
            tray_restore_item: restore_item,
            tray_toggle_item: toggle_item,
            picker_open: false,
            process_list: vec![],
            picker_filter: String::new(),
            event_tx,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Suppress egui's built-in ~15 s safety-net repaint timer.
        // The manager calls ctx.request_repaint() explicitly whenever something
        // actually needs to happen, so we never need the automatic fallback.
        ctx.request_repaint_after(std::time::Duration::from_secs(3600));

        // ── Consume taskbar-visibility flags set by the manager thread ─────
        // Must happen before any early-return so the Win32 call isn't skipped.
        {
            let mut s = self.shared.lock().unwrap();
            if s.needs_taskbar_show {
                s.needs_taskbar_show = false;
                set_taskbar_visible(true);
            }
            if s.needs_taskbar_hide {
                s.needs_taskbar_hide = false;
                set_taskbar_visible(false);
            }
        }

        // X button → park off-screen instead of quitting (app lives in tray).
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.park_offscreen(ctx);
            return;
        }

        // Refresh tray items whenever the manager flagged it.
        // update() is woken by ctx.request_repaint() from the manager thread,
        // so this fires even while the window is off-screen/minimized.
        {
            let mut s = self.shared.lock().unwrap();
            if s.needs_tray_refresh {
                s.needs_tray_refresh = false;
                let hdr_on = s.hdr_on();
                let status = if hdr_on {
                    format!(
                        "HDR active ({})",
                        if s.active_count > 0 {
                            format!("{} game(s)", s.active_count)
                        } else {
                            "manual".into()
                        }
                    )
                } else {
                    "- Idle".into()
                };
                let _ = self.tray_status_item.set_text(&status);
                let _ = self
                    .tray_toggle_item
                    .set_text(if hdr_on { "Disable HDR now" } else { "Enable HDR now" });
                let _ = self.tray.set_tooltip(Some(if hdr_on {
                    "HDRify — HDR active"
                } else {
                    "HDRify"
                }));
                self.tray_restore_item.set_checked(s.config.restore_on_exit);
            }
        }

        // Don't render while off-screen / hidden.
        if !self.shared.lock().unwrap().window_open {
            return;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_main(ui, ctx);
        });

        if self.picker_open {
            self.render_picker(ctx);
        }
    }
}

// ── rendering ─────────────────────────────────────────────────────────────────
impl App {
    fn render_main(&mut self, ui: &mut Ui, ctx: &Context) {
        // Snapshot shared state — hold lock only while reading.
        let (config, messages, active_count, hdr_on) = {
            let s = self.shared.lock().unwrap();
            (
                s.config.clone(),
                s.messages.clone(),
                s.active_count,
                s.hdr_on(), // respects hdr_override_off
            )
        };

        ui.heading("HDRify");
        ui.separator();

        if hdr_on {
            ui.colored_label(
                egui::Color32::from_rgb(80, 200, 80),
                format!(
                    "HDR active — {}",
                    if active_count > 0 {
                        format!("{} watched game(s) running", active_count)
                    } else {
                        "enabled manually".into()
                    }
                ),
            );
            ui.add_space(4.0);
        }

        if !messages.is_empty() {
            egui::CollapsingHeader::new("Log")
                .default_open(false)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(80.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for m in &messages {
                                ui.weak(m);
                            }
                        });
                });
            ui.add_space(4.0);
        }

        ui.separator();

        // Restore on exit checkbox
        let mut restore = config.restore_on_exit;
        if ui
            .checkbox(&mut restore, "Restore HDR on game exit")
            .changed()
        {
            self.push_cmd(AppCmd::SetRestoreOnExit(restore));
        }

        ui.add_space(4.0);
        let label = if hdr_on { "Disable HDR now" } else { "Enable HDR now" };
        if ui.button(label).clicked() {
            self.push_cmd(AppCmd::ToggleHdr);
        }

        ui.add_space(8.0);
        ui.label("Watched executables:");
        ui.add_space(2.0);

        let mut to_remove: Option<usize> = None;
        egui::ScrollArea::vertical()
            .id_source("game_list")
            .max_height(180.0)
            .show(ui, |ui| {
                if config.games.is_empty() {
                    ui.weak("None — add games below.");
                }
                for (i, game) in config.games.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(&game.display_name);
                        ui.weak(format!("({})", game.exe_name));
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.small_button("x").clicked() {
                                    to_remove = Some(i);
                                }
                            },
                        );
                    });
                }
            });

        if let Some(i) = to_remove {
            self.push_cmd(AppCmd::RemoveGame(i));
        }

        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Add from running processes…").clicked() {
                self.refresh_process_list();
                self.picker_filter.clear();
                self.picker_open = true;
            }
            if ui.button("Add from file…").clicked() {
                self.add_from_file();
            }
        });

        let _ = ctx;
    }

    fn render_picker(&mut self, ctx: &Context) {
        let filter = &mut self.picker_filter;
        let process_list = &self.process_list;
        let mut close = false;
        let mut to_add: Option<String> = None;
        let mut do_refresh = false;

        egui::Window::new("Select Running Process")
            .resizable(true)
            .default_size([380.0, 460.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Filter:");
                    ui.text_edit_singleline(filter);
                    if ui.button("Refresh").clicked() {
                        do_refresh = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
                ui.separator();

                let filter_lower = filter.to_lowercase();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for name in process_list.iter() {
                        if filter_lower.is_empty()
                            || name.to_lowercase().contains(&filter_lower)
                        {
                            if ui.selectable_label(false, name).clicked() {
                                to_add = Some(name.clone());
                                close = true;
                            }
                        }
                    }
                });
            });

        if do_refresh {
            self.refresh_process_list();
        }
        if close {
            self.picker_open = false;
        }
        if let Some(exe) = to_add {
            let display = exe.strip_suffix(".exe").unwrap_or(&exe).to_string();
            self.push_cmd(AppCmd::AddGame(GameEntry {
                display_name: display,
                exe_name: exe,
            }));
        }
    }

    fn push_cmd(&self, cmd: AppCmd) {
        let _ = self.event_tx.send(AppEvent::Cmd(cmd));
    }

    /// Park the window off-screen and remove it from the taskbar.
    /// Used for the X button and tray "hide" action.
    fn park_offscreen(&self, ctx: &Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
            crate::OFFSCREEN_X,
            crate::OFFSCREEN_Y,
        )));
        set_taskbar_visible(false);
        self.shared.lock().unwrap().window_open = false;
    }

    fn refresh_process_list(&mut self) {
        use sysinfo::System;
        let mut sys = System::new();
        sys.refresh_processes();
        let mut names: Vec<String> = sys
            .processes()
            .values()
            .map(|p| p.name().to_string())
            .collect();
        names.sort_unstable();
        names.dedup();
        self.process_list = names;
    }

    fn add_from_file(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Executable", &["exe"])
            .set_title("Select Game Executable")
            .pick_file()
        {
            let exe_name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let display = path
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| exe_name.clone());
            self.push_cmd(AppCmd::AddGame(GameEntry {
                display_name: display,
                exe_name,
            }));
        }
    }
}
