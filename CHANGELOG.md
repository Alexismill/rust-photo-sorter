# Changelog

All notable changes to this project are documented here. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- The source folder is re-read when the window gets the focus back, to pick
  up photos and folders added or removed meanwhile. `⟳` stays, to also
  reload photos edited in place.

### Fixed

- Undo no longer overwrites a file that took the photo's name in the
  meantime: the photo is put back under a free name instead.

## [0.2.0] - 2026-10-01

Quicker folder handling, without going through the folder dialog.

### Added

- **New folder** field (`Ctrl+N`): type a name and press `Enter` to create a
  folder inside the source folder, add it as a target and bind its key.
- `🗁` button next to the source and target folders, to show them in the file
  explorer.
- The source folder's subfolders are listed in the sidebar, with their photo
  count. One click opens a subfolder, `⬆ ..` opens the parent.
- `+` in front of a subfolder adds it to the target folders.
- `⟳` re-reads the source folder after changes made outside the app, keeping
  the current photo, the selection and the undo history.
- `Alt+←` / `Alt+→` open the previous / next folder beside the source folder,
  skipping target folders. An emptied folder offers the next one.
- Clickable breadcrumb for the source folder's path.

### Changed

- A target folder opened as the source folder can no longer be sorted into.

## [0.1.0] - 2026-09-30

First release.

### Added

- Keyboard-driven sorting: bind a key to each target folder, press it to move
  the current photo there.
- Zoom around the pointer, pan, fit-to-window / 1:1 toggle.
- Contact sheet (`G`): a grid of thumbnails to sort several photos at once.
- Undo (`Ctrl+Z`), a whole batch at once.
- Photos are moved, never overwritten nor deleted.

[Unreleased]: https://github.com/Alexismill/rust-photo-sorter/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Alexismill/rust-photo-sorter/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Alexismill/rust-photo-sorter/releases/tag/v0.1.0
