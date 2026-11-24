![HDRify](docs/cover.png)

# HDRify

Turns HDR on for every HDR-capable display when a watched game starts, and puts the displays back the way they were when the last one exits. Lives in the system tray; the settings window only shows in the taskbar while it is open.

HDRify is event driven. It sleeps until Windows reports that a watched game started or exited. The one timer that repeats is at logon: while the taskbar does not exist yet, HDRify retries adding its tray icon every 2 seconds.

## How it works

### Detecting games

HDRify runs a real-time ETW session on the `Microsoft-Windows-Kernel-Process` provider, limited to process start and stop events. One background thread reads them and compares each new process against the watch list; only a watched game wakes the UI thread. In testing, 500 unrelated process starts cost HDRify under 0.05 ms of CPU each.

The session uses a 50 ms flush timer, so a game is seen about 50 ms after its process is created and HDRify asks Windows for HDR right away. The switch itself is Windows' part and took 0.1 to 0.4 s on the test PC, so HDR was on 0.15 to 0.45 s after launch. That timer needs an ETW flag (`EVENT_TRACE_USE_MS_FLUSH_TIMER`) that is in `ntwmi.h` but not in the SDK headers. On a Windows that rejects it, HDRify falls back to the documented 1 second timer and HDR comes on about a second later. WMI's `Win32_ProcessStartTrace`, which earlier versions used, took 1.1 to 1.6 s.

### Switching HDR

Display state goes through the `DisplayConfig` API. On Windows 11 24H2 and later HDRify uses `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2` and `DISPLAYCONFIG_SET_HDR_STATE`. The older "advanced color" calls are only the fallback for older Windows: on 24H2 they also report SDR monitors with automatic colour management as "supported and enabled", and switching those toggles colour management instead of HDR.

### Restoring

When the first watched game starts, HDRify records every display's HDR state and then switches HDR on. Five seconds after the last watched game exits it puts each display back to the recorded state. The delay means a game that restarts itself (through its launcher or Steam) causes one mode switch instead of three. If you exit HDRify, log off or shut down while a game is running, the state is put back at once.

Restoring is on by default. Untick "Put HDR back the way it was when the last game exits" to leave HDR on after a game.

If no HDR display is connected and active when the game starts (the TV is off, or on another input), HDRify waits and switches HDR on when one appears. The same goes for putting it back: a display that is off when the game exits is put back when it comes on again, as long as HDRify is still running.

## Features

- HDR on when a watched game starts, previous state restored when the last one exits (optional)
- Games already running when HDRify starts, or when you add them, are picked up
- Manual HDR toggle from the tray menu or the settings window; it switches every HDR-capable display at once
- Add games from the running processes or by browsing for an `.exe`
- Status, tray icon and tooltip show the real display state, including changes made with Win+Alt+B or in Settings
- Scales with display DPI, works with the keyboard (Tab, Enter, Esc, Delete in the games list)
- Starting it a second time opens the settings window of the running copy
- Settings in `%LOCALAPPDATA%\hdrify\config.json`, activity log in `%LOCALAPPDATA%\hdrify\hdrify.log`

## Requirements

- Windows 10 1903 or later, or Windows 11, with an HDR-capable display
- Administrator rights. A normal user account can't start a trace session for kernel process events, so the release exe asks for elevation (UAC) when it starts. Windows also lets members of the Performance Log Users group run trace sessions; HDRify has not been tested that way.
- Nothing else to install. The exe has the C runtime linked in.

## Starting with Windows

Windows does not auto-start programs that need elevation from the Startup folder or the Run key. `install.ps1` sets up the alternative. From an elevated PowerShell, after building:

```powershell
.\install.ps1
```

It copies `target\release\hdrify.exe` to `C:\Program Files\HDRify` and registers a scheduled task named HDRify that starts it at logon with highest privileges, normal priority and no time limit. Run it again after every rebuild.

Run it from the Windows account that plays the games: the task starts at that account's logon. Don't run it while a watched game is running. It stops the running copy without letting it put HDR back, and the new copy then takes "HDR on" as the state to return to.

Don't point the task at the build folder. The task starts the exe elevated with no prompt, so any program that can replace the file gets administrator rights at your next logon, and a build folder is writable by every program you run.

Settings and the log stay in `%LOCALAPPDATA%\hdrify`, which your own programs can also write to. HDRify reads `config.json` there and writes `config.json`, `config.json.tmp`, `config.json.bad` and `hdrify.log`. A program running as you could edit the settings, or redirect that folder so those writes land somewhere else. On a PC where programs running as you are not trusted, HDRify is not safe to run elevated.

## Building

```
cargo build --release
```

The release exe embeds a manifest that requires administrator. Debug builds (`cargo build`, `cargo run`, `cargo test`) do not, so they run from a normal terminal; game detection then reports that it needs administrator rights and everything else works. Start the terminal as administrator to exercise detection in a debug build.

```
cargo test
cargo test -- --ignored --nocapture displays
```

The second command prints what HDRify reads from your displays and changes nothing.
