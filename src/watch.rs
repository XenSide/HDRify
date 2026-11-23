//! Which watched games are running, fed by process start/stop events from the kernel's
//! ETW provider. Events are consumed on one background thread; only a watched game starting
//! or exiting wakes the UI thread, with a [`WM_WATCH`] message.
//!
//! This reads ETW directly, not WMI's Win32_ProcessStartTrace. WMI sits on the same events
//! but hands them over 1.1-1.6 s after the process started; this session, with its 50 ms
//! flush timer, takes about 50 ms. Both need administrator rights.

use crate::app::WM_WATCH;
use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard};
use windows::core::{w, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_INVALID_PARAMETER, ERROR_SUCCESS, HWND,
    LPARAM, WPARAM,
};
use windows::Win32::System::Diagnostics::Etw::*;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

// `wParam` of WM_WATCH. `lParam` is a `Box<String>`: the game's exe name, or the error text.
pub const STOPPED: usize = 0;
pub const STARTED: usize = 1;
/// Events are flowing; games that were already running can now be picked up safely.
pub const READY: usize = 2;
/// Tracing could not start, or stopped.
pub const FAILED: usize = 3;

struct Tracker {
    /// Exe names to watch, as the user entered them.
    watch: Vec<String>,
    /// Watched processes that are alive: (pid, exe name from the watch list).
    running: Vec<(u32, String)>,
}

impl Tracker {
    /// A process started. `image` is its exe name or full path. Returns the watch-list
    /// name if it is a watched game that isn't tracked yet.
    fn started(&mut self, pid: u32, image: &str) -> Option<String> {
        let exe = image.rsplit(['\\', '/']).next()?;
        let name = self.watch.iter().find(|w| w.eq_ignore_ascii_case(exe))?.clone();
        if self.running.iter().any(|(p, _)| *p == pid) {
            return None;
        }
        self.running.push((pid, name.clone()));
        Some(name)
    }

    /// A process exited. Returns its name if it was a tracked game.
    fn stopped(&mut self, pid: u32) -> Option<String> {
        let i = self.running.iter().position(|(p, _)| *p == pid)?;
        Some(self.running.swap_remove(i).1)
    }

    /// Replaces the watch list. A game taken off the list stops counting as running.
    fn set_watch(&mut self, names: Vec<String>) {
        self.running.retain(|(_, name)| names.iter().any(|w| w.eq_ignore_ascii_case(name)));
        self.watch = names;
    }
}

static TRACKER: Mutex<Tracker> = Mutex::new(Tracker { watch: Vec::new(), running: Vec::new() });
static TRACING: AtomicBool = AtomicBool::new(false);

fn tracker() -> MutexGuard<'static, Tracker> {
    TRACKER.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How many watched games are running.
pub fn running() -> usize {
    tracker().running.len()
}

pub fn is_tracing() -> bool {
    TRACING.load(Relaxed)
}

/// Replaces the watch list, then picks up watched games that are already running.
/// Returns the names of those newly found games.
pub fn set_watch(names: Vec<String>) -> Vec<String> {
    let mut t = tracker();
    t.set_watch(names);
    // Without exit events a game found here would count as running forever.
    if !is_tracing() {
        return vec![];
    }
    // The snapshot is taken under the lock, so the exit event of any process in it is
    // handled after the process is tracked, never before.
    processes().iter().filter_map(|(pid, exe)| t.started(*pid, exe)).collect()
}

/// (pid, exe name) of every running process.
pub fn processes() -> Vec<(u32, String)> {
    let mut list = vec![];
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return list;
        };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snapshot, &mut entry);
        while more.is_ok() {
            let name = entry.szExeFile.split(|&c| c == 0).next().unwrap_or(&[]);
            list.push((entry.th32ProcessID, String::from_utf16_lossy(name)));
            more = Process32NextW(snapshot, &mut entry);
        }
        let _ = CloseHandle(snapshot);
    }
    list
}

// ── ETW session ───────────────────────────────────────────────────────────

const SESSION: PCWSTR = w!("HDRify");
/// Microsoft-Windows-Kernel-Process
const PROVIDER: GUID = GUID::from_u128(0x22fb2cd6_0e7b_422b_a0c7_2fad1fd0e716);
/// WINEVENT_KEYWORD_PROCESS: process start and stop only, no thread or image-load events.
const KEYWORD_PROCESS: u64 = 0x10;
const EVENT_PROCESS_START: u16 = 1;
const EVENT_PROCESS_STOP: u16 = 2;
/// EVENT_TRACE_USE_MS_FLUSH_TIMER from ntwmi.h, missing from the SDK headers: FlushTimer is
/// in milliseconds. The documented unit is seconds with a minimum of 1, and an event only
/// reaches a real-time consumer when its buffer is flushed, so without this flag every game
/// start is seen a full second late (measured: 1.00 s against 0.05 s).
const USE_MS_FLUSH_TIMER: u32 = 0x10;
const FLUSH_MS: u32 = 50;

#[repr(C)]
struct Session {
    properties: EVENT_TRACE_PROPERTIES,
    name: [u16; 16], // StartTrace copies the session name here
}

fn session(ms_flush: bool) -> Session {
    let mut s: Session = unsafe { zeroed() };
    s.properties.Wnode.BufferSize = size_of::<Session>() as u32;
    s.properties.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
    s.properties.Wnode.ClientContext = 1; // QPC timestamps
                                          // One shared buffer keeps a start and its stop in order; fine at this event rate.
    s.properties.LogFileMode = EVENT_TRACE_REAL_TIME_MODE | EVENT_TRACE_NO_PER_PROCESSOR_BUFFERING;
    s.properties.FlushTimer = 1;
    if ms_flush {
        s.properties.LogFileMode |= USE_MS_FLUSH_TIMER;
        s.properties.FlushTimer = FLUSH_MS;
    }
    s.properties.LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
    s
}

/// Starts the tracing thread. It posts [`READY`] once events flow, then [`STARTED`] and
/// [`STOPPED`] for watched games, and [`FAILED`] if tracing can't start or ends.
pub fn spawn(hwnd: HWND) {
    let hwnd = hwnd.0 as isize;
    std::thread::spawn(move || unsafe {
        if let Some(error) = trace(hwnd) {
            TRACING.store(false, Relaxed);
            post(hwnd, FAILED, error);
        }
    });
}

/// Stops the session, which ends the tracing thread. A session is a system object that
/// outlives its process, so this must run on exit.
pub fn stop() {
    TRACING.store(false, Relaxed);
    let mut s = session(false);
    unsafe {
        let _ = ControlTraceW(CONTROLTRACE_HANDLE::default(), SESSION, &mut s.properties, EVENT_TRACE_CONTROL_STOP);
    }
}

/// Runs the session until it ends. Returns the reason, or None if `stop()` ended it.
unsafe fn trace(hwnd: isize) -> Option<String> {
    stop(); // left behind if HDRify was killed last time

    let mut s = session(true);
    let mut handle = CONTROLTRACE_HANDLE::default();
    let mut e = StartTraceW(&mut handle, SESSION, &mut s.properties);
    if e == ERROR_INVALID_PARAMETER {
        // A Windows that doesn't know the millisecond flag: settle for the 1 s timer.
        s = session(false);
        e = StartTraceW(&mut handle, SESSION, &mut s.properties);
    }
    // "Already exists" right after stop() means the old session couldn't be stopped either.
    if e == ERROR_ACCESS_DENIED || e == ERROR_ALREADY_EXISTS {
        return Some("Can't watch for games: HDRify needs to run as administrator.".into());
    }
    if e != ERROR_SUCCESS {
        return Some(format!("Can't watch for games: StartTrace failed (error {}).", e.0));
    }
    let e = EnableTraceEx2(
        handle,
        &PROVIDER,
        EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
        TRACE_LEVEL_INFORMATION as u8,
        KEYWORD_PROCESS,
        0,
        0,
        None,
    );
    if e != ERROR_SUCCESS {
        stop();
        return Some(format!("Can't watch for games: EnableTraceEx2 failed (error {}).", e.0));
    }

    let mut log: EVENT_TRACE_LOGFILEW = zeroed();
    log.LoggerName = PWSTR(SESSION.as_ptr() as *mut u16);
    log.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    log.Anonymous2.EventRecordCallback = Some(on_event);
    log.Context = hwnd as *mut _;
    let consumer = OpenTraceW(&mut log);
    if consumer.Value == u64::MAX {
        let e = GetLastError();
        stop();
        return Some(format!("Can't watch for games: OpenTrace failed (error {}).", e.0));
    }

    TRACING.store(true, Relaxed);
    post(hwnd, READY, String::new());
    let e = ProcessTrace(&[consumer], None, None); // blocks until the session stops
    let _ = CloseTrace(consumer);
    // Still marked as tracing means nobody called stop(): something else ended the session.
    is_tracing().then(|| format!("Stopped watching for games: the trace session ended (error {}).", e.0))
}

unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    let record = &*record;
    if record.EventHeader.ProviderId != PROVIDER || record.UserDataLength < 4 {
        return;
    }
    let hwnd = record.UserContext as isize;
    // Both events begin with the process id, in every version of their layout.
    let pid = (record.UserData as *const u32).read_unaligned();
    match record.EventHeader.EventDescriptor.Id {
        EVENT_PROCESS_START => {
            let image = image_name(record);
            if let Some(name) = tracker().started(pid, &image) {
                post(hwnd, STARTED, name);
            }
        }
        EVENT_PROCESS_STOP => {
            if let Some(name) = tracker().stopped(pid) {
                post(hwnd, STOPPED, name);
            }
        }
        _ => {}
    }
}

/// The ImageName property of a ProcessStart event: the NT path of the new process's exe.
/// Its offset differs between event versions, so TDH looks it up by name.
unsafe fn image_name(record: &EVENT_RECORD) -> String {
    let property =
        [PROPERTY_DATA_DESCRIPTOR { PropertyName: w!("ImageName").as_ptr() as u64, ArrayIndex: u32::MAX, Reserved: 0 }];
    let mut size = 0u32;
    if TdhGetPropertySize(record, None, &property, &mut size) != 0 {
        return String::new();
    }
    let mut text = vec![0u16; size as usize / 2];
    let bytes = std::slice::from_raw_parts_mut(text.as_mut_ptr() as *mut u8, text.len() * 2);
    if TdhGetProperty(record, None, &property, bytes) != 0 {
        return String::new();
    }
    String::from_utf16_lossy(text.split(|&c| c == 0).next().unwrap_or(&[]))
}

unsafe fn post(hwnd: isize, event: usize, text: String) {
    let text = Box::into_raw(Box::new(text));
    if PostMessageW(HWND(hwnd as *mut _), WM_WATCH, WPARAM(event), LPARAM(text as isize)).is_err() {
        drop(Box::from_raw(text)); // the window is gone; nobody will take the string
    }
}

#[cfg(test)]
mod tests {
    use super::Tracker;

    fn tracker(watch: &[&str]) -> Tracker {
        Tracker { watch: watch.iter().map(|s| s.to_string()).collect(), running: vec![] }
    }

    #[test]
    fn matches_the_exe_name_at_the_end_of_a_path_ignoring_case() {
        let mut t = tracker(&["Game.exe"]);
        assert_eq!(t.started(1, r"\Device\HarddiskVolume3\Games\GAME.EXE").as_deref(), Some("Game.exe"));
        assert_eq!(t.started(2, "game.exe").as_deref(), Some("Game.exe"));
        assert_eq!(t.started(3, r"C:\x\notgame.exe"), None);
        assert_eq!(t.started(4, ""), None);
        assert_eq!(t.running.len(), 2);
    }

    #[test]
    fn a_pid_is_counted_once_and_leaves_on_exit() {
        let mut t = tracker(&["game.exe"]);
        assert!(t.started(7, "game.exe").is_some());
        assert!(t.started(7, "game.exe").is_none());
        assert_eq!(t.stopped(7).as_deref(), Some("game.exe"));
        assert_eq!(t.stopped(7), None);
        assert!(t.running.is_empty());
    }

    #[test]
    fn exit_of_an_untracked_process_is_ignored() {
        let mut t = tracker(&["game.exe"]);
        assert_eq!(t.stopped(99), None);
    }

    #[test]
    fn a_game_removed_from_the_list_while_running_stops_counting() {
        let mut t = tracker(&["a.exe", "b.exe"]);
        t.started(1, "a.exe");
        t.started(2, "b.exe");
        t.set_watch(vec!["b.exe".into()]);
        assert_eq!(t.running, vec![(2, "b.exe".to_string())]);
        assert_eq!(t.stopped(1), None);
    }
}
