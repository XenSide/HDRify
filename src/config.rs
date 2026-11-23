use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct GameEntry {
    pub display_name: String,
    /// The exe filename only, e.g. "game.exe". Matched case-insensitively.
    pub exe_name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Config {
    pub games: Vec<GameEntry>,
    /// If true, HDR goes back to its pre-game state when the last watched game exits.
    pub restore_on_exit: bool,
}

/// `%LOCALAPPDATA%\hdrify`
pub fn dir() -> PathBuf {
    let mut p = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    p.push("hdrify");
    p
}

impl Config {
    /// Loads the config. A missing file is a normal first run. A file that can't be read or
    /// parsed is moved aside to `config.json.bad`, so the next save doesn't destroy it, and
    /// the reason comes back as the second value.
    pub fn load() -> (Self, Option<String>) {
        let path = dir().join("config.json");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => return (Self::default(), Some(format!("Could not read settings: {e}"))),
        };
        match serde_json::from_str(&text) {
            Ok(config) => (config, None),
            Err(e) => {
                let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                (Self::default(), Some(format!("Settings file was damaged, kept as config.json.bad: {e}")))
            }
        }
    }

    /// Writes to a temp file and renames it over the old one, so a crash or power cut
    /// mid-write can't leave half a file.
    pub fn save(&self) -> std::io::Result<()> {
        let dir = dir();
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("config.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(tmp, dir.join("config.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_0_1_format() {
        let c: Config =
            serde_json::from_str(r#"{"games":[{"display_name":"ds","exe_name":"ds.exe"}],"restore_on_exit":true}"#)
                .unwrap();
        assert_eq!(c.games[0].exe_name, "ds.exe");
        assert!(c.restore_on_exit);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let c: Config = serde_json::from_str(r#"{"games":[{"exe_name":"a.exe"}]}"#).unwrap();
        assert_eq!(c.games.len(), 1);
        assert!(!c.restore_on_exit);
        assert_eq!(serde_json::from_str::<Config>("{}").unwrap(), Config::default());
    }

    #[test]
    fn truncated_file_is_an_error_not_an_empty_config() {
        assert!(serde_json::from_str::<Config>(r#"{"games":[{"exe_na"#).is_err());
    }
}
