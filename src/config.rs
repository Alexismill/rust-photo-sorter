//! Settings that survive between runs.

use egui::Key;
use std::path::PathBuf;

/// Keys the app keeps for itself, so they can never be bound to a folder.
/// Checked when binding *and* when reading shortcuts back, so a config saved
/// by an older version cannot smuggle one of these in.
pub const RESERVED_KEYS: &[Key] = &[
    Key::ArrowLeft,
    Key::ArrowRight,
    Key::Z,
    Key::Space,
    Key::Escape,
];

pub fn is_reserved(key: Key) -> bool {
    RESERVED_KEYS.contains(&key)
}

/// A destination folder and the key that sends the current photo to it.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct TargetFolder {
    pub path: PathBuf,
    pub shortcut: Option<Key>,
}

impl TargetFolder {
    pub fn new(path: PathBuf) -> Self {
        Self { path, shortcut: None }
    }

    /// Short name shown in the sidebar (the last segment of the path).
    pub fn display_name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub struct Config {
    pub target_folders: Vec<TargetFolder>,
    pub source_folder: Option<PathBuf>,
}

impl Config {
    /// Index of the folder already bound to this key, if any.
    pub fn shortcut_owner(&self, key: Key) -> Option<usize> {
        self.target_folders
            .iter()
            .position(|f| f.shortcut == Some(key))
    }

    /// Every usable shortcut, as (folder index, key). Reserved keys are
    /// filtered out rather than trusted.
    pub fn shortcuts(&self) -> Vec<(usize, Key)> {
        self.target_folders
            .iter()
            .enumerate()
            .filter_map(|(i, f)| f.shortcut.map(|k| (i, k)))
            .filter(|(_, key)| !is_reserved(*key))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_an_already_bound_key() {
        let mut config = Config::default();
        config.target_folders.push(TargetFolder {
            path: "/photos/keep".into(),
            shortcut: Some(Key::K),
        });
        config
            .target_folders
            .push(TargetFolder::new("/photos/discard".into()));

        assert_eq!(config.shortcut_owner(Key::K), Some(0));
        assert_eq!(config.shortcut_owner(Key::D), None);
        assert_eq!(config.shortcuts(), vec![(0, Key::K)]);
    }

    #[test]
    fn reserved_keys_are_never_returned_as_shortcuts() {
        // An older config could hold a key the app now reserves; reading it
        // back must not hand navigation over to a folder.
        let mut config = Config::default();
        config.target_folders.push(TargetFolder {
            path: "/photos/legacy".into(),
            shortcut: Some(Key::Space),
        });
        config.target_folders.push(TargetFolder {
            path: "/photos/keep".into(),
            shortcut: Some(Key::K),
        });

        assert_eq!(config.shortcuts(), vec![(1, Key::K)]);
    }

    #[test]
    fn display_name_is_the_last_path_segment() {
        let folder = TargetFolder::new("/home/me/photos/Holidays 2024".into());
        assert_eq!(folder.display_name(), "Holidays 2024");
    }
}
