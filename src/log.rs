//! Activity log: every line is timestamped and appended to `%LOCALAPPDATA%\hdrify\hdrify.log`.

use std::io::Write;
use windows::Win32::System::SystemInformation::GetLocalTime;

const MAX_BYTES: u64 = 256 * 1024;

/// Appends `msg` to the log file and returns the line to show in the window.
pub fn write(msg: &str) -> String {
    let t = unsafe { GetLocalTime() };
    let dir = crate::config::dir();
    let path = dir.join("hdrify.log");
    let _ = std::fs::create_dir_all(&dir);
    // shortcut: the file is cleared once it passes 256 KB, no rotation; add one if old history matters.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}  {msg}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
        );
    }
    format!("{:02}:{:02}:{:02}  {msg}", t.wHour, t.wMinute, t.wSecond)
}
