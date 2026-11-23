//! HDR state of the active displays, through the DisplayConfig API.
//!
//! Windows 11 24H2 split "advanced color" in two: HDR, and automatic colour management for
//! SDR wide-gamut panels. The older ADVANCED_COLOR calls report such an SDR monitor as
//! supported and enabled, and switching it toggles colour management, not HDR. So the
//! 24H2 calls are tried first and the older pair is only the fallback for older Windows.

use std::mem::{size_of, zeroed};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_PATH_INFO, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID};

// From wingdi.h (SDK 10.0.26100); the windows crate doesn't have these yet.
const GET_ADVANCED_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(15);
const SET_HDR_STATE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(16);

/// DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2. The older DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO
/// is the same struct without the last field.
#[repr(C)]
struct ColorInfo {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
    color_encoding: u32,
    bits_per_color_channel: u32,
    active_color_mode: u32,
}

/// DISPLAYCONFIG_SET_HDR_STATE and DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE: bit 0 = enable.
#[repr(C)]
struct SetState {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Display {
    adapter: LUID,
    target: u32,
    /// The display can do HDR.
    pub supported: bool,
    /// HDR is switched on for this display.
    pub enabled: bool,
    /// Read with the 24H2 call, so it must be set with the 24H2 call.
    hdr_api: bool,
}

/// The HDR state of every active display.
pub fn query() -> Vec<Display> {
    unsafe {
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![];
        // The display set can change between the two calls; retry a few times if it did.
        for _ in 0..3 {
            let (mut n_paths, mut n_modes) = (0u32, 0u32);
            if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes).is_err() {
                return vec![];
            }
            paths = vec![zeroed(); n_paths as usize];
            let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![zeroed(); n_modes as usize];
            let result = QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut n_paths,
                paths.as_mut_ptr(),
                &mut n_modes,
                modes.as_mut_ptr(),
                None,
            );
            paths.truncate(n_paths as usize);
            match result {
                ERROR_SUCCESS => break,
                ERROR_INSUFFICIENT_BUFFER => paths.clear(),
                _ => return vec![],
            }
        }

        paths
            .iter()
            .filter_map(|p| {
                let (adapter, target) = (p.targetInfo.adapterId, p.targetInfo.id);
                let mut info: ColorInfo = zeroed();
                info.header = header(GET_ADVANCED_COLOR_INFO_2, size_of::<ColorInfo>(), adapter, target);
                if DisplayConfigGetDeviceInfo(&mut info.header) == 0 {
                    // bit 4 = highDynamicRangeSupported, bit 5 = highDynamicRangeUserEnabled
                    let (supported, enabled) = (info.value & 0x10 != 0, info.value & 0x20 != 0);
                    return Some(Display { adapter, target, supported, enabled, hdr_api: true });
                }
                info.header = header(
                    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                    size_of::<ColorInfo>() - size_of::<u32>(),
                    adapter,
                    target,
                );
                if DisplayConfigGetDeviceInfo(&mut info.header) != 0 {
                    return None;
                }
                // bit 0 = advancedColorSupported, bit 1 = advancedColorEnabled
                let (supported, enabled) = (info.value & 1 != 0, info.value & 2 != 0);
                Some(Display { adapter, target, supported, enabled, hdr_api: false })
            })
            .collect()
    }
}

/// Switches HDR on or off on every HDR-capable display in `displays` that isn't already
/// there. Returns how many displays changed.
pub fn set_all(displays: &[Display], on: bool) -> usize {
    displays.iter().filter(|d| d.supported && d.enabled != on && set(d, on)).count()
}

/// Puts every display that is still connected back to the state it has in `saved`.
/// Returns how many displays changed.
pub fn restore(saved: &[Display]) -> usize {
    query()
        .iter()
        .filter(|now| {
            saved
                .iter()
                .find(|s| s.adapter == now.adapter && s.target == now.target)
                .is_some_and(|s| s.supported && s.enabled != now.enabled && set(now, s.enabled))
        })
        .count()
}

fn header(
    kind: DISPLAYCONFIG_DEVICE_INFO_TYPE,
    size: usize,
    adapter: LUID,
    target: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id: target }
}

fn set(d: &Display, on: bool) -> bool {
    let kind = if d.hdr_api { SET_HDR_STATE } else { DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE };
    let request = SetState { header: header(kind, size_of::<SetState>(), d.adapter, d.target), value: on as u32 };
    unsafe { DisplayConfigSetDeviceInfo(&request.header) == 0 }
}

#[cfg(test)]
mod tests {
    /// Reads the real displays; changes nothing. `cargo test -- --ignored --nocapture displays`
    #[test]
    #[ignore = "needs a desktop session"]
    fn displays() {
        let displays = super::query();
        println!("{displays:#?}");
        assert!(!displays.is_empty());
    }
}
