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
}
