use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct GameEntry {
    /// The exe filename only, e.g. "game.exe". Matched case-insensitively.
    pub exe_name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    pub games: Vec<GameEntry>,
    /// If true, HDR goes back to its pre-game state when the last watched game exits.
    pub restore_on_exit: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { games: vec![], restore_on_exit: true }
    }
}

/// `%LOCALAPPDATA%\hdrify`
pub fn dir() -> PathBuf {
    let mut p = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    p.push("hdrify");
    p
}

impl Config {
    /// Loads `config.json` from `dir`. A missing file is a normal first run. A file that
    /// can't be read or parsed is moved aside to `config.json.bad`, so the next save doesn't
    /// destroy it, and what happened comes back as the second value.
    pub fn load(dir: &Path) -> (Self, Option<String>) {
        let path = dir.join("config.json");
        let problem = match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => e.to_string(),
            // Editors on Windows like to put a byte order mark in front; it isn't JSON.
            Ok(bytes) => match serde_json::from_slice::<Self>(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)) {
                Ok(mut config) => {
                    // An empty name would match every process whose name Windows can't report.
                    config.games.retain(|g| !g.exe_name.trim().is_empty());
                    return (config, None);
                }
                Err(e) => e.to_string(),
            },
        };
        let kept = match std::fs::rename(&path, path.with_extension("json.bad")) {
            Ok(()) => "it is kept as config.json.bad".to_string(),
            Err(e) => format!("and it could not be moved aside ({e}), so the next change overwrites it"),
        };
        (Self::default(), Some(format!("Settings file could not be used ({problem}); {kept}")))
    }

    /// Writes to a temp file, flushes it to disk and renames it over the old one, so a
    /// crash or power cut mid-write can't leave half a file.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("config.json.tmp");
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(tmp, dir.join("config.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty folder under the system temp folder, removed again when dropped.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("hdrify-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn write(&self, bytes: &[u8]) {
            std::fs::write(self.0.join("config.json"), bytes).unwrap();
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn game(exe: &str) -> GameEntry {
        GameEntry { exe_name: exe.into() }
    }

    #[test]
    fn missing_file_is_a_first_run_with_restore_on() {
        let dir = TempDir::new("missing");
        let (config, problem) = Config::load(&dir.0);
        assert_eq!(config, Config { games: vec![], restore_on_exit: true });
        assert_eq!(problem, None);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = TempDir::new("roundtrip");
        let config = Config { games: vec![game("a.exe"), game("B.EXE")], restore_on_exit: false };
        config.save(&dir.0).unwrap();
        config.save(&dir.0).unwrap(); // a second save replaces the first
        assert_eq!(Config::load(&dir.0), (config, None));
        assert!(!dir.0.join("config.json.tmp").exists());
    }

    #[test]
    fn reads_the_0_1_format_and_a_byte_order_mark() {
        let dir = TempDir::new("old");
        dir.write(
            b"\xEF\xBB\xBF{\"games\":[{\"display_name\":\"ds\",\"exe_name\":\"ds.exe\"}],\"restore_on_exit\":false}",
        );
        assert_eq!(Config::load(&dir.0), (Config { games: vec![game("ds.exe")], restore_on_exit: false }, None));
    }

    #[test]
    fn missing_fields_fall_back_to_defaults_and_empty_names_are_dropped() {
        let dir = TempDir::new("partial");
        dir.write(br#"{"games":[{"exe_name":"a.exe"},{"display_name":"no exe"},{"exe_name":"  "}]}"#);
        assert_eq!(Config::load(&dir.0), (Config { games: vec![game("a.exe")], restore_on_exit: true }, None));
    }

    #[test]
    fn a_damaged_file_is_moved_aside_not_overwritten() {
        // Truncated JSON, and UTF-16 as Windows PowerShell's `>` writes it.
        let utf16: Vec<u8> = "\u{feff}{\"games\":[]}".encode_utf16().flat_map(u16::to_le_bytes).collect();
        for (name, content) in [("truncated", &br#"{"games":[{"exe_na"#[..]), ("utf16", &utf16[..])] {
            let dir = TempDir::new(name);
            dir.write(content);
            let (config, problem) = Config::load(&dir.0);
            assert_eq!(config, Config::default());
            assert!(problem.unwrap().contains("kept as config.json.bad"));
            config.save(&dir.0).unwrap();
            assert_eq!(std::fs::read(dir.0.join("config.json.bad")).unwrap(), content);
        }
    }
}
