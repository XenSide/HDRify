/// WMI-based process monitoring using Win32_ProcessStartTrace / Win32_ProcessStopTrace.
///
/// Both classes are backed by ETW (Event Tracing for Windows) and deliver events
/// immediately — zero polling. Requires administrator privileges (SeSecurityPrivilege),
/// which the embedded manifest requests via requireAdministrator.
use crate::hdr::DisplayState;
use crate::manager::AppState;
use serde::Deserialize;
use std::sync::{mpsc, Arc, Mutex};
use wmi::{COMLibrary, WMIConnection};

#[derive(Debug)]
pub enum ProcessEvent {
    /// Process started.  If HDR was enabled immediately by the monitor thread,
    /// the pre-enable display states are included so the manager can use them
    /// for later restoration.
    Started(String, Option<Vec<DisplayState>>),
    Stopped(String),
    WmiError(String),
}

// Struct names must exactly match the WMI class so wmi::notification() builds
// "SELECT * FROM Win32_ProcessStartTrace" automatically.
#[allow(non_camel_case_types)]
#[derive(Deserialize, Debug)]
struct Win32_ProcessStartTrace {
    #[serde(rename = "ProcessName")]
    process_name: String,
}

#[allow(non_camel_case_types)]
#[derive(Deserialize, Debug)]
struct Win32_ProcessStopTrace {
    #[serde(rename = "ProcessName")]
    process_name: String,
}

pub fn spawn_monitors(tx: mpsc::Sender<ProcessEvent>, shared: Arc<Mutex<AppState>>) {
    let tx_start = tx.clone();
    std::thread::Builder::new()
        .name("wmi-start".into())
        .spawn(move || {
            let com = match COMLibrary::new() {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx_start
                        .send(ProcessEvent::WmiError(format!("COM init failed: {e:?}")));
                    return;
                }
            };
            // wmi is declared before iter so it drops AFTER iter (LIFO order),
            // satisfying the borrow checker for the iterator lifetime.
            let wmi = match WMIConnection::new(com) {
                Ok(w) => w,
                Err(e) => {
                    let _ = tx_start
                        .send(ProcessEvent::WmiError(format!("WMI connect failed: {e:?}")));
                    return;
                }
            };
            let iter = match wmi.notification::<Win32_ProcessStartTrace>() {
                Ok(i) => i,
                Err(e) => {
                    let _ = tx_start.send(ProcessEvent::WmiError(format!(
                        "WMI start trace failed (admin required): {e:?}"
                    )));
                    return;
                }
            };
            for item in iter {
                match item {
                    Ok(e) => {
                        let name = e.process_name;
                        // Fast path: enable HDR right here in the WMI thread,
                        // before the manager loop even wakes up.
                        let pre_enable_states = {
                            let state = shared.lock().unwrap();
                            let watched = state
                                .config
                                .games
                                .iter()
                                .any(|g| g.exe_name.eq_ignore_ascii_case(&name));
                            let should_enable = watched
                                && state.active_count == 0
                                && !state.hdr_manually_on
                                && !state.hdr_override_off;
                            drop(state); // release lock before Win32 calls
                            if should_enable {
                                Some(crate::hdr::enable_all())
                            } else {
                                None
                            }
                        };
                        let _ = tx_start.send(ProcessEvent::Started(name, pre_enable_states));
                    }
                    Err(e) => {
                        let _ = tx_start.send(ProcessEvent::WmiError(format!(
                            "Start event error: {e:?}"
                        )));
                    }
                }
            }
        })
        .expect("failed to spawn wmi-start thread");

    let tx_stop = tx;
    std::thread::Builder::new()
        .name("wmi-stop".into())
        .spawn(move || {
            let com = match COMLibrary::new() {
                Ok(c) => c,
                Err(e) => {
                    let _ =
                        tx_stop.send(ProcessEvent::WmiError(format!("COM init failed: {e:?}")));
                    return;
                }
            };
            let wmi = match WMIConnection::new(com) {
                Ok(w) => w,
                Err(e) => {
                    let _ = tx_stop.send(ProcessEvent::WmiError(format!(
                        "WMI connect failed: {e:?}"
                    )));
                    return;
                }
            };
            let iter = match wmi.notification::<Win32_ProcessStopTrace>() {
                Ok(i) => i,
                Err(e) => {
                    let _ = tx_stop.send(ProcessEvent::WmiError(format!(
                        "WMI stop trace failed (admin required): {e:?}"
                    )));
                    return;
                }
            };
            for item in iter {
                match item {
                    Ok(e) => {
                        let _ = tx_stop.send(ProcessEvent::Stopped(e.process_name));
                    }
                    Err(e) => {
                        let _ = tx_stop.send(ProcessEvent::WmiError(format!(
                            "Stop event error: {e:?}"
                        )));
                    }
                }
            }
        })
        .expect("failed to spawn wmi-stop thread");
}
