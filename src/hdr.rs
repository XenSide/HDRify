use std::mem::{size_of, zeroed};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo, GetDisplayConfigBufferSizes,
    QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
    DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::LUID;

#[derive(Clone, Debug)]
pub struct DisplayState {
    pub adapter_id: LUID,
    pub target_id: u32,
    pub hdr_enabled: bool,
    pub hdr_supported: bool,
}

pub fn query_displays() -> Vec<DisplayState> {
    unsafe {
        let mut path_count = 0u32;
        let mut mode_count = 0u32;

        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
            .is_err()
        {
            return vec![];
        }

        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> =
            (0..path_count).map(|_| zeroed()).collect();
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> =
            (0..mode_count).map(|_| zeroed()).collect();

        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
        .is_err()
        {
            return vec![];
        }

        let mut result = Vec::new();
        for path in paths.iter().take(path_count as usize) {
            let mut info: DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO = zeroed();
            info.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO;
            info.header.size = size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32;
            info.header.adapterId = path.targetInfo.adapterId;
            info.header.id = path.targetInfo.id;

            let ret = DisplayConfigGetDeviceInfo(
                std::ptr::addr_of_mut!(info.header),
            );
            if ret == 0 {
                let v = info.Anonymous.value;
                result.push(DisplayState {
                    adapter_id: path.targetInfo.adapterId,
                    target_id: path.targetInfo.id,
                    // DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO bit layout:
                    //   bit 0 (value & 1) = advancedColorSupported (hardware capability)
                    //   bit 1 (value & 2) = advancedColorEnabled   (currently on)
                    hdr_supported: (v & 1) != 0,
                    hdr_enabled: (v & 2) != 0,
                });
            }
        }
        result
    }
}

pub fn set_hdr(adapter_id: LUID, target_id: u32, enable: bool) -> bool {
    unsafe {
        let mut req: DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE = zeroed();
        req.header.r#type = DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE;
        req.header.size = size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>() as u32;
        req.header.adapterId = adapter_id;
        req.header.id = target_id;
        req.Anonymous.value = u32::from(enable);

        DisplayConfigSetDeviceInfo(std::ptr::addr_of_mut!(req.header)) == 0
    }
}

/// Enable HDR on all HDR-capable displays. Returns the states captured before
/// enabling so they can be restored with `restore()`.
pub fn enable_all() -> Vec<DisplayState> {
    let states = query_displays();
    for s in &states {
        if s.hdr_supported {
            set_hdr(s.adapter_id, s.target_id, true);
        }
    }
    states
}

/// Restore the HDR state that was saved before a game started.
pub fn restore(states: &[DisplayState]) {
    for s in states {
        set_hdr(s.adapter_id, s.target_id, s.hdr_enabled);
    }
}
