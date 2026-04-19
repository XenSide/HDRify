use crate::config::{Config, GameEntry};
use crate::hdr::{self, DisplayState};
use crate::wmi_monitor::ProcessEvent;
use crate::{HDR_UPDATE, WM_HDR};
use std::sync::{mpsc, Arc, Mutex};
use windows::Win32::{Foundation::{HWND, LPARAM, WPARAM}, UI::WindowsAndMessaging::PostMessageW};

// ── Events ────────────────────────────────────────────────────────────────

pub enum AppEvent {
    Process(ProcessEvent),
    Cmd(AppCmd),
}

pub enum AppCmd {
    AddGame(GameEntry),
    RemoveGame(usize),
    SetRestoreOnExit(bool),
    ToggleHdr,
}

// ── Shared state ──────────────────────────────────────────────────────────

pub struct AppState {
    pub config:         Config,
    pub messages:       Vec<String>,
    pub active_count:   usize,
    pub hdr_manually_on: bool,
    pub hdr_override_off: bool,
    pub saved_states:   Option<Vec<DisplayState>>,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let active_count = count_running_games(&config);
        let (saved_states, init_msg) = if active_count > 0 {
            (Some(hdr::enable_all()), Some("HDR enabled (watched game already running).".to_string()))
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
        }
    }

    pub fn hdr_on(&self) -> bool {
        !self.hdr_override_off && (self.active_count > 0 || self.hdr_manually_on)
    }

    pub fn push_msg(&mut self, msg: String) {
        self.messages.push(msg);
        if self.messages.len() > 50 { self.messages.remove(0); }
    }
}

// ── Manager thread ────────────────────────────────────────────────────────

/// Spawns the manager thread. It receives AppEvents, updates shared state,
/// and posts WM_HDR/HDR_UPDATE to the main window whenever something changes.
/// All tray/menu events are handled directly in WndProc; this thread only
/// deals with process events and UI commands.
pub fn spawn(hwnd: isize, event_rx: mpsc::Receiver<AppEvent>, shared: Arc<Mutex<AppState>>) {
    std::thread::Builder::new()
        .name("manager".into())
        .spawn(move || {
            loop {
                let ev = match event_rx.recv() {
                    Ok(ev) => ev,
                    Err(_) => break,
                };
                {
                    let mut state = shared.lock().unwrap();
                    match ev {
                        AppEvent::Process(pev) => handle_process_event(pev, &mut state),
                        AppEvent::Cmd(cmd) => match cmd {
                            AppCmd::AddGame(entry) => {
                                if !state.config.games.iter().any(|g| g.exe_name.eq_ignore_ascii_case(&entry.exe_name)) {
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
                            }
                            AppCmd::ToggleHdr => do_toggle_hdr(&mut state),
                        },
                    }
                } // lock released before PostMessageW
                unsafe {
                    let _ = PostMessageW(HWND(hwnd as *mut _), WM_HDR, WPARAM(HDR_UPDATE), LPARAM(0));
                }
            }
        })
        .expect("manager thread");
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn do_toggle_hdr(state: &mut AppState) {
    if state.hdr_on() {
        for d in &hdr::query_displays() {
            if d.hdr_supported { hdr::set_hdr(d.adapter_id, d.target_id, false); }
        }
        state.hdr_manually_on  = false;
        state.hdr_override_off = state.active_count > 0;
        state.push_msg("[-] HDR disabled manually.".into());
    } else {
        hdr::enable_all();
        state.hdr_manually_on  = true;
        state.hdr_override_off = false;
        state.push_msg("[+] HDR enabled manually.".into());
    }
}

fn handle_process_event(ev: ProcessEvent, state: &mut AppState) {
    match ev {
        ProcessEvent::WmiError(msg) => { state.push_msg(format!("[!] {msg}")); }
        ProcessEvent::Started(name, pre_enable_states) => {
            if !is_watched(state, &name) { return; }
            state.active_count += 1;
            if state.active_count == 1 && !state.hdr_manually_on && !state.hdr_override_off {
                state.saved_states = pre_enable_states.or_else(|| Some(hdr::enable_all()));
                state.push_msg(format!("[+] HDR enabled — {name} started."));
            } else if state.hdr_override_off {
                state.push_msg(format!("[~] Tracking {name} (HDR manually off)."));
            } else {
                state.push_msg(format!("[+] {name} started (HDR already on)."));
            }
        }
        ProcessEvent::Stopped(name) => {
            if !is_watched(state, &name) { return; }
            state.active_count = state.active_count.saturating_sub(1);
            if state.active_count == 0 {
                state.hdr_override_off = false;
                if !state.hdr_manually_on {
                    if state.config.restore_on_exit {
                        if let Some(saved) = state.saved_states.take() { hdr::restore(&saved); }
                        state.push_msg(format!("[-] HDR restored — {name} exited."));
                    } else {
                        state.saved_states = None;
                        state.push_msg(format!("[-] {name} exited (HDR left on)."));
                    }
                }
            } else {
                state.push_msg(format!("[-] {name} exited ({} game(s) still running).", state.active_count));
            }
        }
    }
}

fn is_watched(state: &AppState, exe: &str) -> bool {
    state.config.games.iter().any(|g| g.exe_name.eq_ignore_ascii_case(exe))
}

fn count_running_games(config: &Config) -> usize {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_processes();
    config.games.iter().filter(|g| {
        sys.processes().values().any(|p| p.name().eq_ignore_ascii_case(&g.exe_name))
    }).count()
}
