# HDRify

Automatically enables HDR on all capable displays when a watched game launches, then optionally restores the previous state when it exits. Lives in the system tray with no taskbar presence.

Event Driven, no polling at all (won't use system resources untill the very moment a game is opened)

## Features

- Automatic HDR on/off tied to watched game executables
- Per-display HDR state snapshot and restore on exit (optional)
- Manual HDR toggle from the tray menu or settings window
- Add games from running processes or by browsing for an `.exe`
- Tray tooltip and menu reflect current HDR status
- Config persisted to `%LOCALAPPDATA%\hdrify\config.json`

## How it works

HDRify registers a WMI `Win32_ProcessStartTrace` / `Win32_ProcessStopTrace` subscription (ETW-backed) to receive process start and stop events with zero polling. When a watched executable is detected, HDR is enabled immediately on the WMI monitor thread — before the main event loop even wakes up — minimizing the delay between game launch and HDR activation.

Display state is managed via the `DisplayConfig` Win32 API (`DisplayConfigGetDeviceInfo` / `DisplayConfigSetDeviceInfo`), which controls HDR per-display and per-adapter. The pre-launch state of every display is saved so it can be restored accurately on exit, regardless of which displays had HDR on or off beforehand.

The entire application is event-driven. No thread polls or sleeps in a loop; everything is wired through `mpsc` channels with blocking `recv()`.


## Requirements

- Windows 10/11 with an HDR-capable display
- Administrator privileges (required by WMI ETW process tracing)

The embedded application manifest requests `requireAdministrator`, so Windows will prompt for UAC elevation on launch.

## Building

```
cargo build --release
```

The release profile uses `lto = "thin"` and `strip = true` for a compact binary.
