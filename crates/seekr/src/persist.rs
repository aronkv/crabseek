//! The download list, kept in `~/.local/share/seekr/downloads.json` so a
//! restart does not lose it. Unfinished downloads are queued again on the
//! next start and resume from their `.part` files, like Nicotine+ does
//! with its `downloads.json`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDownload {
    pub username: String,
    pub filename: String,
    #[serde(flatten)]
    pub status: SavedStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SavedStatus {
    /// Queued or transferring when seekr quit; queued again on start.
    Pending,
    Completed {
        path: PathBuf,
    },
    Failed {
        reason: String,
    },
}

/// The saved list; empty if there is none. A corrupt file is moved aside
/// instead of failing the start.
pub fn load(path: &Path) -> Vec<SavedDownload> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(%e, "could not read the saved downloads");
            return Vec::new();
        }
    };
    match serde_json::from_str(&text) {
        Ok(list) => list,
        Err(e) => {
            let backup = path.with_extension("json.broken");
            tracing::warn!(%e, backup = %backup.display(), "saved downloads are corrupt");
            let _ = std::fs::rename(path, backup);
            Vec::new()
        }
    }
}

pub fn save(path: &Path, list: &[SavedDownload]) -> anyhow::Result<()> {
    config::write_private(path, &serde_json::to_string_pretty(list)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_corrupt_file() {
        let dir = std::env::temp_dir().join(format!("seekr-persist-{}", std::process::id()));
        let path = dir.join("downloads.json");
        assert!(load(&path).is_empty());

        let list = vec![
            SavedDownload {
                username: "alice".into(),
                filename: "a\\1.flac".into(),
                status: SavedStatus::Pending,
            },
            SavedDownload {
                username: "bob".into(),
                filename: "b\\2.flac".into(),
                status: SavedStatus::Completed {
                    path: "/music/2.flac".into(),
                },
            },
            SavedDownload {
                username: "carol".into(),
                filename: "c\\3.flac".into(),
                status: SavedStatus::Failed {
                    reason: "File not shared.".into(),
                },
            },
        ];
        save(&path, &list).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"status\": \"pending\""));
        assert_eq!(load(&path), list);

        std::fs::write(&path, "{ not json").unwrap();
        assert!(load(&path).is_empty());
        assert!(path.with_extension("json.broken").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
