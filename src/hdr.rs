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

/// The HDR-capable displays in `now` that `known` doesn't have: the ones that were
/// connected or switched on since `known` was taken.
fn added(known: &[Display], now: &[Display]) -> Vec<Display> {
    now.iter().filter(|d| d.supported && !known.iter().any(|k| k.same(d))).copied().collect()
}

/// What a running game needs, as (displays to record, displays to switch on). Both are
/// the HDR displays that `known` doesn't have yet. When the last restore is still `owed`,
/// the known displays that are present are switched on too: they stay recorded with
/// their older state, but this game wants them on.
pub fn wanted(known: &[Display], now: &[Display], owed: bool) -> (Vec<Display>, Vec<Display>) {
    let new = added(known, now);
    let mut switch = new.clone();
    if owed {
        switch.extend(now.iter().filter(|d| d.supported && known.iter().any(|k| k.same(d))));
    }
    (new, switch)
}

impl Display {
    /// The same physical output, whatever its state.
    pub fn same(&self, other: &Display) -> bool {
        self.adapter == other.adapter && self.target == other.target
    }
}

/// Puts every display that is connected and active back to the state it has in `saved`.
/// Returns how many displays changed, and the entries of `saved` that are still owed:
/// their display is off or gone right now, or it refused the change.
pub fn restore(saved: &[Display]) -> (usize, Vec<Display>) {
    let mut now = query();
    let changed = changes(saved, &now).iter().filter(|(display, on)| set(display, *on)).count();
    if changed > 0 {
        now = query();
    }
    (changed, unrestored(saved, &now))
}

/// The HDR displays in `saved` that `now` doesn't show in their saved state.
fn unrestored(saved: &[Display], now: &[Display]) -> Vec<Display> {
    let done = |s: &Display| now.iter().any(|n| n.same(s) && n.enabled == s.enabled);
    saved.iter().filter(|s| s.supported && !done(s)).copied().collect()
}

/// The displays in `now` that are not in the state `saved` has for them, each with the
/// state to give it. A display that wasn't there when `saved` was taken is left alone.
fn changes(saved: &[Display], now: &[Display]) -> Vec<(Display, bool)> {
    now.iter()
        .filter_map(|n| {
            let s = saved.iter().find(|s| s.same(n))?;
            (s.supported && s.enabled != n.enabled).then_some((*n, s.enabled))
        })
        .collect()
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
    use super::*;

    fn display(target: u32, supported: bool, enabled: bool) -> Display {
        Display { adapter: LUID { LowPart: 1, HighPart: 0 }, target, supported, enabled, hdr_api: true }
    }

    #[test]
    fn restore_only_touches_known_displays_that_changed() {
        let saved =
            [display(1, true, false), display(2, true, true), display(3, false, false), display(4, true, false)];
        let now = [
            display(1, true, true),  // we switched it on: back off
            display(2, true, true),  // was on before the game: stays
            display(3, false, true), // not an HDR display: never touched
            display(5, true, true),  // plugged in during the game: not ours
        ]; // display 4 was unplugged
        assert_eq!(changes(&saved, &now), vec![(display(1, true, true), false)]);
        // The user switched HDR off by hand on a display that had it on: it goes back on.
        assert_eq!(changes(&saved, &[display(2, true, false)]), vec![(display(2, true, false), true)]);
        assert_eq!(changes(&[], &now), vec![]);
    }

    #[test]
    fn unrestored_lists_what_is_still_owed() {
        let saved =
            [display(1, true, false), display(2, true, true), display(3, false, false), display(4, true, false)];
        // 1 is back off, 2 was left on, 3 is not an HDR display, 4 is switched off (not listed).
        let now = [display(1, true, false), display(2, true, true), display(3, false, true)];
        assert_eq!(unrestored(&saved, &now), vec![display(4, true, false)]);
        // 1 refused the change and is still on.
        assert_eq!(unrestored(&saved[..1], &[display(1, true, true)]), vec![display(1, true, false)]);
        // Nothing can be read at all: everything that could have changed is owed.
        assert_eq!(unrestored(&saved, &[]), vec![saved[0], saved[1], saved[3]]);
    }

    #[test]
    fn wanted_covers_first_game_late_display_and_owed_restore() {
        let (monitor, tv, sdr) = (display(1, true, false), display(2, true, false), display(3, false, false));
        // First game: every HDR display is new.
        assert_eq!(wanted(&[], &[monitor, sdr], false), (vec![monitor], vec![monitor]));
        // The TV comes on during the game: only the TV.
        let monitor_on = display(1, true, true);
        assert_eq!(wanted(&[monitor], &[monitor_on, tv, sdr], false), (vec![tv], vec![tv]));
        // Nothing changed: nothing to do.
        assert_eq!(wanted(&[monitor, tv], &[monitor_on, tv], false), (vec![], vec![]));
        // A new game while the monitor is still owed a restore: switch it, record nothing new.
        assert_eq!(wanted(&[monitor], &[monitor, sdr], true), (vec![], vec![monitor]));
        // Owed, but that display is still off: nothing to switch.
        assert_eq!(wanted(&[tv], &[sdr], true), (vec![], vec![]));
    }

    #[test]
    fn added_finds_new_hdr_displays_only() {
        let known = [display(1, true, false)];
        let now = [display(1, true, true), display(2, true, false), display(3, false, false)];
        assert_eq!(added(&[], &now), vec![display(1, true, true), display(2, true, false)]);
        assert_eq!(added(&known, &now), vec![display(2, true, false)]);
        assert_eq!(added(&known, &known), vec![]);
    }

    /// Reads the real displays; changes nothing. `cargo test -- --ignored --nocapture displays`
    #[test]
    #[ignore = "needs a desktop session"]
    fn displays() {
        let displays = query();
        println!("{displays:#?}");
        assert!(!displays.is_empty());
    }
}
