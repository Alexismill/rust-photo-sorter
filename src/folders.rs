//! Source and target folders: opening, browsing and refreshing the source
//! folder, and managing the list of target folders. Sorting photos into them
//! stays in `app.rs`.

use crate::app::{name_of, PhotoSorter};
use crate::config::TargetFolder;
use crate::files;
use egui::Key;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A folder inside the source, as listed in the sidebar.
pub struct Subfolder {
    pub path: PathBuf,
    /// Photos directly inside it, so you can see where sorting is left.
    pub photos: usize,
}

impl PhotoSorter {
    // ── Source folder ───────────────────────────────────────────────────────

    pub fn load_photos_from(&mut self, folder: PathBuf) {
        self.photos = files::list_photos(&folder);
        self.current_index = 0;
        self.scroll_to_current = true;
        self.textures.clear();
        self.thumbnails.clear();
        self.unreadable.clear();
        self.selection.clear();
        self.selection_anchor = None;
        self.full_texture = None;
        self.reset_zoom();
        self.loader.forget_pending();
        self.history.clear();
        self.status = photos_found(self.photos.len());
        self.config.source_folder = Some(folder);
        self.refresh_folders();
    }

    /// `⟳`: re-reads the source folder after changes made outside the app.
    /// Unlike `load_photos_from`, keeps the current photo, the selection and
    /// the undo history.
    pub fn refresh_source(&mut self) {
        let Some(source) = self.config.source_folder.clone() else { return };
        self.replace_photos(files::list_photos(&source));

        // A file may have been edited in place: decode everything again.
        self.textures.clear();
        self.thumbnails.clear();
        self.unreadable.clear();
        self.full_texture = None;

        self.refresh_folders();
        self.status = format!("Refreshed. {}", photos_found(self.photos.len()));
    }

    /// When the window gets the focus back: picks up photos and folders added
    /// or removed meanwhile. Lighter than `⟳`: decoded images are kept, so
    /// switching windows never flickers, and nothing shows if nothing changed.
    pub fn refresh_on_focus(&mut self) {
        let Some(source) = self.config.source_folder.clone() else { return };
        self.refresh_folders();
        let photos = files::list_photos(&source);
        if photos != self.photos {
            self.replace_photos(photos);
            self.status = format!("Source folder changed. {}", photos_found(self.photos.len()));
        }
    }

    /// Swaps in a fresh listing of the source folder, keeping the current
    /// photo and the selection where they still exist.
    fn replace_photos(&mut self, photos: Vec<PathBuf>) {
        let current = self.current_photo().cloned();
        self.photos = photos;
        let present: HashSet<&PathBuf> = self.photos.iter().collect();
        self.selection.retain(|p| present.contains(p));
        if let Some(index) = current.and_then(|c| self.photos.iter().position(|p| *p == c)) {
            self.current_index = index;
        }
        self.clamp_index();
        self.drop_stale_full_texture();
        self.scroll_to_current = true;
    }

    /// Re-reads the source's subfolders and its next sibling, e.g. after
    /// creating a folder elsewhere.
    pub fn refresh_folders(&mut self) {
        self.subfolders = match &self.config.source_folder {
            Some(source) => files::list_subfolders(source)
                .into_iter()
                .map(|path| Subfolder {
                    photos: files::count_photos(&path),
                    path,
                })
                .collect(),
            None => Vec::new(),
        };
        self.next_folder = self.sibling_folder(1);
    }

    /// Keeps a subfolder's count right when photos are sorted into it or
    /// taken back, without re-reading the disk.
    pub(crate) fn count_moved(&mut self, folder: &Path, added: isize) {
        if let Some(subfolder) = self.subfolders.iter_mut().find(|s| s.path == folder) {
            subfolder.photos = subfolder.photos.saturating_add_signed(added);
        }
    }

    /// The folder `step` places away from the source among its siblings,
    /// skipping target folders: walks through 100CANON, 101CANON, ...
    pub fn sibling_folder(&self, step: isize) -> Option<PathBuf> {
        let source = self.config.source_folder.as_ref()?;
        let siblings: Vec<PathBuf> = files::list_subfolders(source.parent()?)
            .into_iter()
            .filter(|f| f == source || !self.is_target(f))
            .collect();
        let index = siblings.iter().position(|f| f == source)?;
        siblings.get(index.checked_add_signed(step)?).cloned()
    }

    /// `Alt+Left` / `Alt+Right`, and the button shown once a folder is empty.
    pub fn open_sibling(&mut self, step: isize) {
        let Some(folder) = self.sibling_folder(step) else {
            self.status = match step {
                1 => "No next folder.".into(),
                _ => "No previous folder.".into(),
            };
            return;
        };
        let name = name_of(&folder);
        self.load_photos_from(folder);
        self.status = format!("{name}: {}", self.status);
    }

    /// `Alt+Up`: opens the folder holding the source folder.
    pub fn open_parent(&mut self) {
        let parent = self.config.source_folder.as_ref().and_then(|f| f.parent());
        let Some(parent) = parent.map(Path::to_path_buf) else {
            self.status = "No parent folder.".into();
            return;
        };
        self.load_photos_from(parent);
    }

    /// Something dropped on the window: a folder becomes the source, a photo
    /// opens its folder on that photo.
    pub fn open_dropped(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.load_photos_from(path);
            return;
        }
        let folder = path.parent().filter(|_| files::is_supported_image(&path));
        let Some(folder) = folder.map(Path::to_path_buf) else {
            self.status = "Drop a folder or a photo to open it.".into();
            return;
        };
        self.load_photos_from(folder);
        if let Some(index) = self.photos.iter().position(|p| *p == path) {
            self.current_index = index;
        }
    }

    /// `🗁`: shows a source or target folder in the file explorer.
    pub fn reveal_folder(&mut self, folder: &Path) {
        if !folder.is_dir() {
            self.status = format!("Folder \"{}\" no longer exists.", name_of(folder));
        } else if let Err(e) = files::reveal_in_file_manager(folder) {
            self.status = format!("Could not open the file explorer: {e}");
        }
    }

    // ── Target folders ──────────────────────────────────────────────────────

    /// Returns `false` if the folder was refused.
    pub fn add_target_folder(&mut self, path: PathBuf) -> bool {
        if self.is_target(&path) {
            self.status = "That folder is already in the list.".into();
            false
        } else if Some(&path) == self.config.source_folder.as_ref() {
            self.status = "The target folder cannot be the source folder.".into();
            false
        } else {
            self.config.target_folders.push(TargetFolder::new(path));
            // Targets are skipped when walking through sibling folders.
            self.next_folder = self.sibling_folder(1);
            true
        }
    }

    /// Creates the folder typed in the sidebar inside the source folder, adds
    /// it as a target and waits for its key.
    pub fn create_target_folder(&mut self) {
        let Some(source) = self.config.source_folder.clone() else {
            self.status = "Open a source folder first.".into();
            return;
        };
        let path = match files::create_subfolder(&source, &self.new_folder_name) {
            Ok(path) => path,
            Err(e) => {
                self.status = format!("Could not create the folder: {e}");
                return;
            }
        };
        self.refresh_folders();
        if self.add_target_and_bind(path) {
            self.new_folder_name.clear();
        }
    }

    /// Adds a target and waits for its key at once: the quick paths from the
    /// sidebar. Returns `false` if the folder was refused.
    pub fn add_target_and_bind(&mut self, path: PathBuf) -> bool {
        if !self.add_target_folder(path) {
            return false;
        }
        let index = self.config.target_folders.len() - 1;
        let name = self.config.target_folders[index].display_name();
        self.status = format!("\"{name}\" added.");
        self.listening_for_key = Some(index);
        true
    }

    pub fn is_target(&self, path: &Path) -> bool {
        self.config.target_folders.iter().any(|f| f.path == path)
    }

    pub fn remove_target_folder(&mut self, index: usize) {
        if index < self.config.target_folders.len() {
            self.config.target_folders.remove(index);
            self.next_folder = self.sibling_folder(1);
        }
        // Stop waiting for a key if that folder was the one being bound.
        if self.listening_for_key == Some(index) {
            self.listening_for_key = None;
        }
    }

    /// Binds `key` to folder `folder_idx`. Returns `false` if the key is
    /// reserved or already taken by another folder.
    pub fn assign_shortcut(&mut self, folder_idx: usize, key: Key) -> bool {
        if crate::config::is_reserved(key) {
            self.status = format!("[{key:?}] is reserved by the app.");
            return false;
        }
        match self.config.shortcut_owner(key) {
            Some(other) if other != folder_idx => {
                self.status = format!(
                    "[{key:?}] is already taken by \"{}\".",
                    self.config.target_folders[other].display_name()
                );
                false
            }
            _ => {
                self.config.target_folders[folder_idx].shortcut = Some(key);
                self.status = format!(
                    "[{key:?}] -> \"{}\"",
                    self.config.target_folders[folder_idx].display_name()
                );
                true
            }
        }
    }
}

fn photos_found(count: usize) -> String {
    match count {
        0 => "No images in this folder.".to_string(),
        1 => "1 photo found.".to_string(),
        n => format!("{n} photos found."),
    }
}

#[cfg(test)]
mod tests {
    use crate::app::tests::{sorter_with_photos, stems};
    use crate::app::View;

    #[test]
    fn a_new_folder_is_created_in_the_source_and_awaits_its_key() {
        let (mut app, _) = sorter_with_photos("new_folder");
        let source = app.config.source_folder.clone().unwrap();

        app.new_folder_name = "Keep".into();
        app.create_target_folder();
        assert!(source.join("Keep").is_dir());
        assert_eq!(app.config.target_folders[1].path, source.join("Keep"));
        assert_eq!(app.listening_for_key, Some(1));
        assert!(app.new_folder_name.is_empty());
        // The new folder is not a photo.
        assert_eq!(app.photos.len(), 5);
    }

    #[test]
    fn sibling_navigation_skips_target_folders() {
        let (mut app, _) = sorter_with_photos("siblings");
        let source = app.config.source_folder.clone().unwrap();
        let other = source.parent().unwrap().join("other");
        std::fs::create_dir(&other).unwrap();

        // Siblings: other, source, target. The last one is a target folder.
        app.open_sibling(1);
        assert_eq!(app.config.source_folder.as_ref(), Some(&source));
        app.open_sibling(-1);
        assert_eq!(app.config.source_folder.as_ref(), Some(&other));
        assert_eq!(app.next_folder.as_ref(), Some(&source));
    }

    #[test]
    fn alt_up_opens_the_parent_folder() {
        let (mut app, _) = sorter_with_photos("parent");
        let source = app.config.source_folder.clone().unwrap();
        app.open_parent();
        assert_eq!(app.config.source_folder.as_deref(), source.parent());
    }

    #[test]
    fn a_subfolder_becomes_a_target_once() {
        let (mut app, _) = sorter_with_photos("subfolder_target");
        let source = app.config.source_folder.clone().unwrap();
        std::fs::create_dir(source.join("Keep")).unwrap();
        app.refresh_folders();
        let keep = app.subfolders[0].path.clone();

        assert!(app.add_target_and_bind(keep.clone()));
        assert!(app.is_target(&keep));
        assert_eq!(app.listening_for_key, Some(1));

        app.listening_for_key = None;
        assert!(!app.add_target_and_bind(keep));
        assert_eq!(app.config.target_folders.len(), 2);
        assert_eq!(app.listening_for_key, None);
    }

    #[test]
    fn subfolder_counts_follow_sorting_and_undo() {
        let (mut app, _) = sorter_with_photos("subfolder_counts");
        let source = app.config.source_folder.clone().unwrap();
        std::fs::create_dir(source.join("Keep")).unwrap();
        std::fs::write(source.join("Keep").join("old.jpg"), "old").unwrap();
        app.refresh_folders();
        assert_eq!(app.subfolders[0].photos, 1);

        app.add_target_folder(source.join("Keep"));
        app.sort_into(1);
        assert_eq!(app.subfolders[0].photos, 2);
        app.undo();
        assert_eq!(app.subfolders[0].photos, 1);
    }

    #[test]
    fn refreshing_picks_up_outside_changes_and_keeps_the_context() {
        let (mut app, _) = sorter_with_photos("refresh");
        let source = app.config.source_folder.clone().unwrap();
        app.view = View::Grid;
        app.click_photo(1, false, false); // select b
        app.current_index = 3; // on d
        app.sort_into(0); // b moved: undo history not empty
        app.click_photo(3, false, false); // select e

        std::fs::remove_file(source.join("a.jpg")).unwrap();
        std::fs::write(source.join("f.jpg"), "f").unwrap();
        app.refresh_source();

        assert_eq!(stems(&app.photos), ["c", "d", "e", "f"]);
        assert_eq!(stems(app.current_photo()), ["e"]);
        assert_eq!(stems(&app.selection), ["e"]);
        assert_eq!(app.history.len(), 1);
    }

    #[test]
    fn refreshing_on_focus_is_silent_when_nothing_changed() {
        let (mut app, _) = sorter_with_photos("refresh_on_focus");
        let source = app.config.source_folder.clone().unwrap();
        app.status = "kept".into();
        app.refresh_on_focus();
        assert_eq!(app.status, "kept");

        app.current_index = 2; // on c
        std::fs::remove_file(source.join("a.jpg")).unwrap();
        app.refresh_on_focus();
        assert_eq!(stems(&app.photos), ["b", "c", "d", "e"]);
        assert_eq!(stems(app.current_photo()), ["c"]);
        assert_ne!(app.status, "kept");
    }

    #[test]
    fn dropping_a_folder_or_a_photo_opens_it() {
        let (mut app, target) = sorter_with_photos("drop");
        let source = app.config.source_folder.clone().unwrap();

        app.open_dropped(target.clone());
        assert_eq!(app.config.source_folder.as_ref(), Some(&target));

        // A photo opens its folder, on that photo.
        app.open_dropped(source.join("c.jpg"));
        assert_eq!(app.config.source_folder.as_ref(), Some(&source));
        assert_eq!(stems(app.current_photo()), ["c"]);

        // Anything else is refused.
        std::fs::write(source.join("notes.txt"), "x").unwrap();
        app.open_dropped(source.join("notes.txt"));
        assert_eq!(app.config.source_folder.as_ref(), Some(&source));
        assert_eq!(stems(app.current_photo()), ["c"]);
    }
}
