//! The few settings A1 needs, stored as JSON in the app's config folder.
//! A2 replaces this with the ported layered config.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// ACE-Step 1.5 folder (contains `acestep/`, `checkpoints/`, `.venv/`). Empty = not set.
    pub ace_step_dir: String,
    /// Where takes are written (`<workspace>/output/<take>`).
    pub workspace_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { ace_step_dir: String::new(), workspace_dir: default_workspace().to_string_lossy().into_owned() }
    }
}

fn default_workspace() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    home.unwrap_or_else(std::env::temp_dir).join("NusicStudio")
}

/// Missing or unreadable file = defaults (first run).
pub fn load(path: &Path) -> Settings {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("nusic-settings-{name}-{}", std::process::id())).join("settings.json")
    }

    #[test]
    fn first_run_gives_defaults() {
        let s = load(&path("missing"));
        assert_eq!(s.ace_step_dir, "");
        assert!(s.workspace_dir.ends_with("NusicStudio"));
    }

    #[test]
    fn save_then_load_round_trips() {
        let p = path("roundtrip");
        let s = Settings { ace_step_dir: "C:/ACE-Step-1.5".into(), workspace_dir: "D:/music".into() };
        save(&p, &s).unwrap();
        assert_eq!(load(&p), s);
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let p = path("partial");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, r#"{"ace_step_dir":"C:/ace"}"#).unwrap();
        let s = load(&p);
        assert_eq!(s.ace_step_dir, "C:/ace");
        assert!(s.workspace_dir.ends_with("NusicStudio"));
    }
}
