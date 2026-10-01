//! Disk operations. No egui dependency, so everything here is testable
//! without starting the UI.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub const EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "gif"];

/// Lists the images in a folder, sorted by name. Does not recurse into
/// subdirectories.
pub fn list_photos(folder: &Path) -> Vec<PathBuf> {
    let mut photos: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.is_file())
        .filter(|p| is_supported_image(p))
        .collect();
    photos.sort();
    photos
}

/// Lists the folders directly inside `folder`, sorted by name regardless of
/// case. Hidden ones (".git", ...) are left out.
pub fn list_subfolders(folder: &Path) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.is_dir())
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    folders.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    folders
}

/// Each folder from the root down to `folder`, with the name to show for it:
/// "D:\Photos\2024" gives "D:", "Photos", "2024".
pub fn breadcrumb(folder: &Path) -> Vec<(String, PathBuf)> {
    let mut crumbs: Vec<(String, PathBuf)> = folder
        .ancestors()
        .map(|path| {
            let name = match path.file_name() {
                Some(name) => name.to_string_lossy().into_owned(),
                // The root has no name: show it as "D:" or "/".
                None => {
                    let root = path.to_string_lossy();
                    let trimmed = root.trim_end_matches(['\\', '/']);
                    if trimmed.is_empty() {
                        root.into_owned()
                    } else {
                        trimmed.to_owned()
                    }
                }
            };
            (name, path.to_path_buf())
        })
        .filter(|(name, _)| !name.is_empty())
        .collect();
    crumbs.reverse();
    crumbs
}

pub fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|ext| EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Finds a free name in `dir`: "photo.jpg" becomes "photo (1).jpg" if taken.
/// Needed because `fs::rename` silently overwrites an existing file.
pub fn unique_destination(dir: &Path, file_name: &OsStr) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }

    let name = Path::new(file_name);
    let stem = name
        .file_stem()
        .unwrap_or(file_name)
        .to_string_lossy()
        .to_string();
    let extension = name.extension().map(|e| e.to_string_lossy().to_string());

    for n in 1..10_000 {
        let filename = match &extension {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = dir.join(filename);
        if !candidate.exists() {
            return candidate;
        }
    }
    candidate
}

/// Creates `parent/name` and returns its path. `name` must be a plain folder
/// name, not a path. An existing folder is fine and returned as is.
pub fn create_subfolder(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    let name = name.trim();
    let mut components = Path::new(name).components();
    let is_plain_name = matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if !is_plain_name {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid folder name",
        ));
    }

    let path = parent.join(name);
    if !path.is_dir() {
        std::fs::create_dir(&path)?;
    }
    Ok(path)
}

/// Opens `folder` in the system file manager. Only a failure to launch is
/// reported: Explorer exits with an error code even when it worked.
pub fn reveal_in_file_manager(folder: &Path) -> std::io::Result<()> {
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(program).arg(folder).spawn()?;
    // Reaped off the UI thread, so the app never freezes or leaves a zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Moves a file. `fs::rename` fails across drives, hence the copy-then-delete
/// fallback.
pub fn move_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    match std::fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(_) => {
            std::fs::copy(source, destination)?;
            std::fs::remove_file(source)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates an empty working directory, fresh for each test.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("photo_sorter_test_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn name_collision_does_not_destroy_the_existing_file() {
        let dir = temp_dir("collision");
        let source_dir = dir.join("source");
        let target_dir = dir.join("target");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();

        std::fs::write(source_dir.join("IMG_001.jpg"), b"photo A").unwrap();
        std::fs::write(target_dir.join("IMG_001.jpg"), b"photo B already there").unwrap();

        let source = source_dir.join("IMG_001.jpg");
        let destination = unique_destination(&target_dir, OsStr::new("IMG_001.jpg"));
        move_file(&source, &destination).unwrap();

        assert_eq!(
            std::fs::read(target_dir.join("IMG_001.jpg")).unwrap(),
            b"photo B already there",
            "the pre-existing file was overwritten"
        );
        assert_eq!(
            std::fs::read(target_dir.join("IMG_001 (1).jpg")).unwrap(),
            b"photo A"
        );
        assert!(!source.exists(), "the source should have been moved away");
    }

    #[test]
    fn keeps_numbering_until_a_name_is_free() {
        let dir = temp_dir("numbering");
        std::fs::write(dir.join("a.jpg"), b"x").unwrap();
        std::fs::write(dir.join("a (1).jpg"), b"x").unwrap();

        assert_eq!(
            unique_destination(&dir, OsStr::new("a.jpg")),
            dir.join("a (2).jpg")
        );
    }

    #[test]
    fn lists_images_only() {
        let dir = temp_dir("listing");
        for name in ["b.jpg", "a.PNG", "notes.txt", "archive.zip", "c.jpeg"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        std::fs::create_dir(dir.join("a_subdirectory.jpg")).unwrap();

        let photos: Vec<String> = list_photos(&dir)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();

        // sorted by name, extensions case-insensitive, directories excluded
        assert_eq!(photos, vec!["a.PNG", "b.jpg", "c.jpeg"]);
    }

    #[test]
    fn breadcrumb_goes_from_the_root_down_to_the_folder() {
        let folder = std::env::temp_dir().join("Photos").join("2024");
        let crumbs = breadcrumb(&folder);

        let names: Vec<&str> = crumbs.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names[names.len() - 2..], ["Photos", "2024"]);
        assert_eq!(crumbs.last().unwrap().1, folder);
        // Starts at the root, shown without its trailing separator.
        let (root_name, root) = &crumbs[0];
        assert!(root.parent().is_none());
        assert!(root_name == "/" || !root_name.ends_with(['\\', '/']));
    }

    #[test]
    fn lists_visible_subfolders_only() {
        let dir = temp_dir("subfolders");
        for name in ["b", "A", "c", ".hidden"] {
            std::fs::create_dir(dir.join(name)).unwrap();
        }
        std::fs::write(dir.join("photo.jpg"), b"x").unwrap();

        let names: Vec<String> = list_subfolders(&dir)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec!["A", "b", "c"]);
    }

    #[test]
    fn creates_a_subfolder_and_rejects_paths() {
        let dir = temp_dir("subfolder");

        let created = create_subfolder(&dir, "  Keep ").unwrap();
        assert_eq!(created, dir.join("Keep"));
        assert!(created.is_dir());
        // Asking again for the same name just returns it.
        assert_eq!(create_subfolder(&dir, "Keep").unwrap(), created);

        for bad in ["", "   ", "..", "a/b", "../escape"] {
            assert!(create_subfolder(&dir, bad).is_err(), "{bad:?} was accepted");
        }
    }
}
