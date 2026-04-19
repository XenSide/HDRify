use crate::config::{Config, GameEntry};
use crate::hdr::{self, DisplayState};
use crate::wmi_monitor::ProcessEvent;
use egui::Context;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use tray_icon::menu::{MenuEvent, MenuId};
use tray_icon::TrayIconEvent;

// ── Events flowing into the manager ──────────────────────────────────────────
pub enum AppEvent {
    Process(ProcessEvent),
    TrayLeft,
    Menu(MenuId),
    Cmd(AppCmd),
}

// ── Commands the UI thread sends via AppEvent::Cmd ────────────────────────────
pub enum AppCmd {
    AddGame(GameEntry),
    RemoveGame(usize),
    SetRestoreOnExit(bool),
    ToggleHdr,
    #[allow(dead_code)]
    Shutdown,
}

// ── State shared between the manager thread and the UI ───────────────────────
pub struct AppState {
    pub config: Config,
    pub messages: Vec<String>,
    pub active_count: usize,
    pub hdr_manually_on: bool,
    /// User explicitly pressed "Disable HDR" while games were running.
    /// Overrides active_count so the UI and toggle stay consistent.
    /// Cleared automatically when all games exit or when the user re-enables.
    pub hdr_override_off: bool,
    pub saved_states: Option<Vec<DisplayState>>,
    pub window_open: bool,
    /// Set by the manager when tray text/tooltip needs refreshing.
    /// Cleared by App::update() after it applies the changes.
    pub needs_tray_refresh: bool,
    /// Signal App::update() to add the window to the taskbar (show).
    pub needs_taskbar_show: bool,
    /// Signal App::update() to remove the window from the taskbar (hide/park).
    pub needs_taskbar_hide: bool,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let active_count = count_running_games(&config);
        let (saved_states, init_msg) = if active_count > 0 {
            (
                Some(hdr::enable_all()),
                Some("HDR enabled (watched game already running).".to_string()),
            )
        } else {
            (None, None)
        };
        AppState {
            config,
            messages: init_msg.into_iter().collect(),
            active_count,
            hdr_manually_on: false,
            hdr_override_off: false,
            saved_states,
            window_open: false,
            needs_tray_refresh: active_count > 0,
            needs_taskbar_show: false,
            needs_taskbar_hide: false,
        }
    }

    pub fn hdr_on(&self) -> bool {
        !self.hdr_override_off && (self.active_count > 0 || self.hdr_manually_on)
    }

    pub fn push_msg(&mut self, msg: String) {
        self.messages.push(msg);
        if self.messages.len() > 50 {
            self.messages.remove(0);
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────
/// Spawns the manager thread plus three lightweight forwarder threads.
///
/// Each forwarder blocks on its respective event source and forwards events to
/// the unified `event_rx` channel — no polling anywhere.
///
/// * `wmi_rx`   — receives `ProcessEvent` from the WMI monitor threads
/// * `event_rx` — unified inbound channel for the manager
/// * `event_tx` — sender side of that channel (used by the forwarders)
pub fn spawn(
    ctx: Context,
    event_rx: mpsc::Receiver<AppEvent>,
    wmi_rx: mpsc::Receiver<ProcessEvent>,
    shared: Arc<Mutex<AppState>>,
    open_id: MenuId,
    exit_id: MenuId,
    restore_id: MenuId,
    toggle_id: MenuId,
    event_tx: mpsc::Sender<AppEvent>,
) {
    // ── WMI forwarder ─────────────────────────────────────────────────────
    // Blocks on Receiver<ProcessEvent> and wraps each event in AppEvent::Process.
    {
        let tx = event_tx.clone();
        std::thread::Builder::new()
            .name("wmi-fwd".into())
            .spawn(move || {
                while let Ok(ev) = wmi_rx.recv() {
                    let _ = tx.send(AppEvent::Process(ev));
                }
            })
            .expect("wmi-fwd thread");
    }

    // ── Tray-icon left-click forwarder ────────────────────────────────────
    // Blocks on the static TrayIconEvent receiver — wakes only on real events.
    {
        let tx = event_tx.clone();
        std::thread::Builder::new()
            .name("tray-fwd".into())
            .spawn(move || {
                loop {
                    match TrayIconEvent::receiver().recv() {
                        Ok(TrayIconEvent::Click {
                            button: tray_icon::MouseButton::Left,
                            ..
                        }) => {
                            let _ = tx.send(AppEvent::TrayLeft);
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
            })
            .expect("tray-fwd thread");
    }

    // ── Menu-event forwarder ──────────────────────────────────────────────
    // Blocks on the static MenuEvent receiver — wakes only on real events.
    {
        let tx = event_tx;
        std::thread::Builder::new()
            .name("menu-fwd".into())
            .spawn(move || {
                while let Ok(ev) = MenuEvent::receiver().recv() {
                    let _ = tx.send(AppEvent::Menu(ev.id));
                }
            })
            .expect("menu-fwd thread");
    }

    // ── Manager ───────────────────────────────────────────────────────────
    // Purely event-driven: recv() blocks until something arrives.
    std::thread::Builder::new()
        .name("manager".into())
        .spawn(move || {
            loop {
                let ev = match event_rx.recv() {
                    Ok(ev) => ev,
                    Err(_) => break, // all senders dropped → exit
                };

                let mut need_show = false;
                let mut need_hide = false;
                let mut need_repaint = false;

                {
                    let mut state = shared.lock().unwrap();

                    match ev {
                        AppEvent::Process(pev) => {
                            handle_wmi_event(pev, &mut state);
                            need_repaint = true;
                        }

                        AppEvent::TrayLeft => {
                            if state.window_open {
                                state.window_open = false;
                                need_hide = true;
                            } else {
                                need_show = true;
                            }
                        }

                        AppEvent::Menu(id) => {
                            if id == open_id {
                                need_show = true;
                            } else if id == exit_id {
                                if state.config.restore_on_exit {
                                    if let Some(s) = state.saved_states.take() {
                                        hdr::restore(&s);
                                    }
                                }
                                std::process::exit(0);
                            } else if id == restore_id {
                                state.config.restore_on_exit = !state.config.restore_on_exit;
                                state.config.save();
                                state.needs_tray_refresh = true;
                                need_repaint = true;
                            } else if id == toggle_id {
                                do_toggle_hdr(&mut state);
                                need_repaint = true;
                            }
                        }

                        AppEvent::Cmd(cmd) => {
                            match cmd {
                                AppCmd::AddGame(entry) => {
                                    if !state.config.games.iter().any(|g| {
                                        g.exe_name.eq_ignore_ascii_case(&entry.exe_name)
                                    }) {
                                        state.config.games.push(entry);
                                        state.config.save();
                                    }
                                }
                                AppCmd::RemoveGame(i) => {
                                    if i < state.config.games.len() {
                                        state.config.games.remove(i);
                                        state.config.save();
                                    }
                                }
                                AppCmd::SetRestoreOnExit(v) => {
                                    state.config.restore_on_exit = v;
                                    state.config.save();
                                    state.needs_tray_refresh = true;
                                }
                                AppCmd::ToggleHdr => {
                                    do_toggle_hdr(&mut state);
                                }
                                AppCmd::Shutdown => {
                                    if state.config.restore_on_exit {
                                        if let Some(s) = state.saved_states.take() {
                                            hdr::restore(&s);
                                        }
                                    }
                                    std::process::exit(0);
                                }
                            }
                            need_repaint = true;
                        }
                    }

                    if need_show {
                        state.window_open = true;
                        state.needs_tray_refresh = true;
                        state.needs_taskbar_show = true;
                    }
                    if need_hide {
                        state.needs_taskbar_hide = true;
                    }
                    if need_repaint {
                        state.needs_tray_refresh = true;
                    }
                } // lock released

                if need_show {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                        egui::pos2(200.0, 200.0),
                    ));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    ctx.request_repaint();
                } else if need_hide {
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                        crate::OFFSCREEN_X,
                        crate::OFFSCREEN_Y,
                    )));
                    ctx.request_repaint();
                } else if need_repaint {
                    ctx.request_repaint();
                }
            }
        })
        .expect("manager thread");
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn do_toggle_hdr(state: &mut AppState) {
    if state.hdr_on() {
        let displays = hdr::query_displays();
        for d in &displays {
            if d.hdr_supported {
                hdr::set_hdr(d.adapter_id, d.target_id, false);
            }
        }
        state.hdr_manually_on = false;
        state.hdr_override_off = state.active_count > 0;
        state.push_msg("[-] HDR disabled manually.".into());
    } else {
        hdr::enable_all();
        state.hdr_manually_on = true;
        state.hdr_override_off = false;
        state.push_msg("[+] HDR enabled manually.".into());
    }
    state.needs_tray_refresh = true;
}

fn handle_wmi_event(ev: ProcessEvent, state: &mut AppState) {
    match ev {
        ProcessEvent::WmiError(msg) => {
            state.push_msg(format!("[!] {msg}"));
        }
        ProcessEvent::Started(name, pre_enable_states) => {
            if !is_watched(state, &name) {
                return;
            }
            state.active_count += 1;
            if state.active_count == 1 && !state.hdr_manually_on && !state.hdr_override_off {
                if let Some(saved) = pre_enable_states {
                    // WMI thread already enabled HDR; record the pre-enable states.
                    state.saved_states = Some(saved);
                } else {
                    state.saved_states = Some(hdr::enable_all());
                }
                state.push_msg(format!("[+] HDR enabled -- {name} started."));
            } else if state.hdr_override_off {
                state.push_msg(format!("[~] Tracking {name} (HDR manually off)."));
            } else {
                state.push_msg(format!("[+] {name} started (HDR already on)."));
            }
            state.needs_tray_refresh = true;
        }
        ProcessEvent::Stopped(name) => {
            if !is_watched(state, &name) {
                return;
            }
            state.active_count = state.active_count.saturating_sub(1);
            if state.active_count == 0 {
                state.hdr_override_off = false;
                if !state.hdr_manually_on {
                    if state.config.restore_on_exit {
                        if let Some(saved) = state.saved_states.take() {
                            hdr::restore(&saved);
                        }
                        state.push_msg(format!("[-] HDR restored -- {name} exited."));
                    } else {
                        state.saved_states = None;
                        state.push_msg(format!("[-] {name} exited (HDR left on)."));
                    }
                }
            } else {
                state.push_msg(format!(
                    "[-] {name} exited ({} game(s) still running).",
                    state.active_count
                ));
            }
            state.needs_tray_refresh = true;
        }
    }
}

fn is_watched(state: &AppState, exe: &str) -> bool {
    state
        .config
        .games
        .iter()
        .any(|g| g.exe_name.eq_ignore_ascii_case(exe))
}

fn count_running_games(config: &Config) -> usize {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_processes();
    config
        .games
        .iter()
        .filter(|g| {
            sys.processes()
                .values()
                .any(|p| p.name().eq_ignore_ascii_case(&g.exe_name))
        })
        .count()
}
