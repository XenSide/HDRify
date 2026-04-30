#![windows_subsystem = "windows"]

mod app;
mod config;
mod hdr;
mod manager;
mod wmi_monitor;

// Shared Win32 message constants used by both app.rs and manager.rs.
// WM_APP = 0x8000; we use WM_APP + 1 as our private message.
pub const WM_HDR: u32 = 0x8001;

// WPARAM values carried on WM_HDR
pub const HDR_UPDATE:      usize = 0; // re-read shared state, refresh controls + tray
pub const HDR_SHOW:        usize = 1; // show settings window and bring to front
pub const HDR_HIDE:        usize = 2; // hide settings window
pub const HDR_EXIT:        usize = 3; // clean up and quit
pub const HDR_MENU:        usize = 4; // drain the menu event queue
pub const HDR_TOGGLE_VIS:  usize = 5; // toggle window visibility (tray left-click)
pub const HDR_PICKER_DONE: usize = 6; // picker window closed, refresh game list
use windows::Win32::System::Threading::{
    GetCurrentProcess, SetPriorityClass, IDLE_PRIORITY_CLASS,
    PROCESS_POWER_THROTTLING_STATE, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, SetProcessInformation, ProcessPowerThrottling
};

fn main() {
    unsafe {
        let process = GetCurrentProcess();
        let _ = SetPriorityClass(process, IDLE_PRIORITY_CLASS);
        let mut throttling = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        };
        let _ = SetProcessInformation(
            process,
            ProcessPowerThrottling,
            &mut throttling as *mut _ as *mut core::ffi::c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
    }
    app::run();
}
