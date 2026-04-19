use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GameEntry {
    pub display_name: String,
    /// The exe filename only, e.g. "game.exe". Matched case-insensitively.
    pub exe_name: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Config {
    pub games: Vec<GameEntry>,
    /// If true, HDR is restored to its pre-game state when the last watched game exits.
    pub restore_on_exit: bool,
}

impl Config {
    pub fn path() -> PathBuf {
        let mut p = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
        p.push("hdrify");
        p.push("config.json");
        p
    }

    pub fn load() -> Self {
        let path = Self::path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }
}
