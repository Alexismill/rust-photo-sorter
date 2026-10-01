//! Application state and sorting logic.
//!
//! This module draws nothing: it decides. Rendering lives in `ui.rs`.

use crate::config::{Config, TargetFolder};
use crate::files;
use crate::loader::{Loader, Quality};
use eframe::CreationContext;
use egui::{Key, TextureHandle, TextureOptions, Vec2};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

/// How many preview textures stay in memory: the current photo and its
/// neighbours.
const TEXTURE_CACHE_SIZE: usize = 5;

/// How many contact-sheet thumbnails stay in memory (~170 KB each).
const THUMB_CACHE_SIZE: usize = 400;

/// Minimum width of a contact-sheet cell, in points. Cells stretch to fill
/// the row, so the actual width is between this and twice this.
pub const MIN_CELL: f32 = 170.0;

/// Offsets from the current photo that get preloaded.
/// Order matters: 0 first, then the next one, which is the likeliest.
const PRELOAD_OFFSETS: [isize; 4] = [0, 1, -1, 2];

/// Furthest you can zoom in. Past 4:1 there is nothing left to judge.
pub const MAX_ZOOM: f32 = 4.0;

/// Scale change per wheel notch (about five notches from fit to 1:1).
pub const ZOOM_STEP: f32 = 1.15;

/// egui scroll points per wheel notch. Varies by platform and mouse: tweak
/// this if the wheel zooms too fast or too slow.
const POINTS_PER_NOTCH: f32 = 80.0;

/// A decoded preview, kept together with the size of the photo it came from.
pub struct Preview {
    pub texture: TextureHandle,
    /// Dimensions of the real photo, before downscaling. Zoom is expressed
    /// against these, so 1:1 means one sensor pixel per screen pixel.
    pub original_size: Vec2,
}

/// How the photo is framed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ZoomMode {
    /// Whole photo scaled to the window. Recomputed on every resize.
    Fit,
    /// Fixed scale, where 1.0 is one image pixel per screen pixel.
    Manual(f32),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    Single,
    /// Contact sheet: a grid of thumbnails with multi-select.
    Grid,
}

/// A completed move, kept so it can be undone.
pub struct MoveRecord {
    pub from: PathBuf,
    pub to: PathBuf,
    /// Position in `photos` at the moment it was removed.
    pub index: usize,
}

pub struct PhotoSorter {
    pub config: Config,

    pub photos: Vec<PathBuf>,
    pub current_index: usize,
    /// Folders inside the source folder, for quick navigation. Read on each
    /// folder change or with ⟳, not every frame.
    pub subfolders: Vec<PathBuf>,

    /// Downscaled images for everyday viewing, one per nearby photo.
    pub textures: HashMap<PathBuf, Preview>,
    /// Full-resolution image, loaded once the preview is not sharp enough for
    /// the current zoom. At most one (a 24 MP frame is ~96 MB on the GPU).
    pub full_texture: Option<(PathBuf, TextureHandle)>,
    pub loader: Loader,
    /// Files that failed to decode, so they are not requested again.
    pub unreadable: HashSet<PathBuf>,

    /// Kept across navigation, to compare a burst at the same magnification.
    pub zoom: ZoomMode,
    /// Offset of the view centre from the image centre, in image pixels.
    /// Also kept across navigation.
    pub pan: Vec2,

    pub view: View,
    /// Contact-sheet thumbnails, for the photos on screen and around them.
    pub thumbnails: HashMap<PathBuf, TextureHandle>,
    /// Selected photos on the contact sheet. Keyed by path, like the
    /// textures, so moving photos out of the list cannot shift it.
    pub selection: HashSet<PathBuf>,
    /// Where a Shift+click range starts: the last photo clicked without Shift.
    pub selection_anchor: Option<PathBuf>,
    /// Set when the current photo changed from the keyboard, so the contact
    /// sheet scrolls to keep it in view.
    pub scroll_to_current: bool,
    /// Contact-sheet scroll offset and visible height, from the last frame.
    pub grid_viewport: (f32, f32),

    /// When `Some(i)`, we are waiting for a key to bind to folder `i`.
    pub listening_for_key: Option<usize>,
    /// Name typed in the sidebar for a new folder inside the source folder.
    pub new_folder_name: String,
    /// Set by `Ctrl+N`, so the sidebar puts the cursor in that field.
    pub focus_new_folder: bool,
    /// One entry per sort action, so a batch is undone in a single step.
    pub history: Vec<Vec<MoveRecord>>,
    pub status: String,
}

impl PhotoSorter {
    pub fn new(cc: &CreationContext<'_>) -> Self {
        let config: Config = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, eframe::APP_KEY))
            .unwrap_or_default();
        Self::with_config(config, cc.egui_ctx.clone())
    }

    /// Separate from `new` so tests can build an app without a window.
    pub fn with_config(config: Config, ctx: egui::Context) -> Self {
        let mut app = Self {
            config,
            photos: Vec::new(),
            current_index: 0,
            subfolders: Vec::new(),
            textures: HashMap::new(),
            full_texture: None,
            loader: Loader::new(ctx),
            unreadable: HashSet::new(),
            zoom: ZoomMode::Fit,
            pan: Vec2::ZERO,
            view: View::Single,
            thumbnails: HashMap::new(),
            selection: HashSet::new(),
            selection_anchor: None,
            scroll_to_current: false,
            grid_viewport: (0.0, 0.0),
            listening_for_key: None,
            new_folder_name: String::new(),
            focus_new_folder: false,
            history: Vec::new(),
            status: String::new(),
        };
        // Pick up the previous session's folder, if it still exists.
        if let Some(folder) = app.config.source_folder.clone() {
            if folder.is_dir() {
                app.load_photos_from(folder);
            }
        }
        app
    }

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
        self.refresh_subfolders();
    }

    /// `⟳`: re-reads the source folder after changes made outside the app.
    /// Unlike `load_photos_from`, keeps the current photo, the selection and
    /// the undo history.
    pub fn refresh_source(&mut self) {
        let Some(source) = self.config.source_folder.clone() else { return };
        let current = self.current_photo().cloned();

        self.photos = files::list_photos(&source);
        let present: HashSet<&PathBuf> = self.photos.iter().collect();
        self.selection.retain(|p| present.contains(p));
        if let Some(index) = current.and_then(|c| self.photos.iter().position(|p| *p == c)) {
            self.current_index = index;
        }
        self.clamp_index();
        self.scroll_to_current = true;

        // A file may have been edited in place: decode everything again.
        self.textures.clear();
        self.thumbnails.clear();
        self.unreadable.clear();
        self.full_texture = None;

        self.refresh_subfolders();
        self.status = format!("Refreshed. {}", photos_found(self.photos.len()));
    }

    /// Re-reads the source's subfolders, e.g. after creating one elsewhere.
    pub fn refresh_subfolders(&mut self) {
        self.subfolders = match &self.config.source_folder {
            Some(source) => files::list_subfolders(source),
            None => Vec::new(),
        };
    }

    pub fn current_photo(&self) -> Option<&PathBuf> {
        self.photos.get(self.current_index)
    }

    // ── Navigation ──────────────────────────────────────────────────────────

    pub fn next(&mut self) {
        if !self.photos.is_empty() {
            self.current_index = (self.current_index + 1).min(self.photos.len() - 1);
            self.drop_stale_full_texture();
            self.scroll_to_current = true;
        }
    }

    pub fn previous(&mut self) {
        self.current_index = self.current_index.saturating_sub(1);
        self.drop_stale_full_texture();
        self.scroll_to_current = true;
    }

    fn clamp_index(&mut self) {
        if self.photos.is_empty() {
            self.current_index = 0;
        } else if self.current_index >= self.photos.len() {
            self.current_index = self.photos.len() - 1;
        }
    }

    // ── Zoom ────────────────────────────────────────────────────────────────

    pub fn reset_zoom(&mut self) {
        self.zoom = ZoomMode::Fit;
        self.pan = Vec2::ZERO;
    }

    /// `Space`: jump straight between the whole photo and 1:1, the two views
    /// that actually get used.
    pub fn toggle_zoom(&mut self) {
        if self.photos.is_empty() {
            return;
        }
        match self.zoom {
            ZoomMode::Fit => self.zoom = ZoomMode::Manual(1.0),
            ZoomMode::Manual(_) => self.reset_zoom(),
        }
    }

    /// The scale actually in force. `Fit` has no fixed number: it depends on
    /// the window, so it is worked out fresh each frame.
    pub fn effective_scale(&self, view: Vec2, image: Vec2) -> f32 {
        match self.zoom {
            ZoomMode::Fit => fit_scale(view, image),
            ZoomMode::Manual(scale) => scale,
        }
    }

    /// Zooms by `factor` around `anchor` (screen pixels from the viewport
    /// centre), so the point under the cursor stays under the cursor.
    pub fn zoom_by(&mut self, factor: f32, anchor: Vec2, view: Vec2, image: Vec2) {
        let minimum = fit_scale(view, image);
        let before = self.effective_scale(view, image);
        let after = (before * factor).clamp(minimum, MAX_ZOOM);
        if (after - before).abs() < f32::EPSILON {
            return;
        }

        if after <= minimum * 1.001 {
            self.reset_zoom();
            return;
        }

        // Keep the image point under the anchor exactly where it was.
        self.pan += anchor * (1.0 / before - 1.0 / after);
        self.zoom = ZoomMode::Manual(after);
        self.pan = clamped_pan(self.pan, view, image, after);
    }

    /// Turns a wheel movement into a zoom factor.
    pub fn wheel_zoom_factor(scroll_y: f32) -> f32 {
        ZOOM_STEP.powf(scroll_y / POINTS_PER_NOTCH)
    }

    /// Keeps the visible region inside the image.
    pub fn clamp_pan(&mut self, view: Vec2, image: Vec2, scale: f32) {
        self.pan = clamped_pan(self.pan, view, image, scale);
    }

    /// The full-resolution texture, but only if it belongs to the photo on
    /// screen right now.
    pub fn current_full_texture(&self) -> Option<&TextureHandle> {
        let current = self.current_photo()?;
        self.full_texture
            .as_ref()
            .filter(|(path, _)| path == current)
            .map(|(_, texture)| texture)
    }

    pub fn current_preview(&self) -> Option<&Preview> {
        self.textures.get(self.current_photo()?)
    }

    /// Asks for the full-resolution decode once the preview would have to be
    /// enlarged. On demand only: most photos are never zoomed into.
    pub fn request_full_if_needed(&mut self, scale: f32) {
        if self.current_full_texture().is_some() {
            return;
        }
        let Some(current) = self.current_photo().cloned() else { return };
        let Some(preview) = self.textures.get(&current) else { return };
        let preview_scale = preview.texture.size_vec2().x / preview.original_size.x;
        if scale > preview_scale * 1.001 {
            self.loader.request(&current, Quality::Full);
        }
    }

    fn drop_stale_full_texture(&mut self) {
        let current = self.current_photo().cloned();
        if let Some((path, _)) = &self.full_texture {
            if Some(path) != current.as_ref() {
                self.full_texture = None;
            }
        }
    }

    // ── Target folders ──────────────────────────────────────────────────────

    /// Returns `false` if the folder was refused.
    pub fn add_target_folder(&mut self, path: PathBuf) -> bool {
        if self.config.target_folders.iter().any(|f| f.path == path) {
            self.status = "That folder is already in the list.".into();
            false
        } else if Some(&path) == self.config.source_folder.as_ref() {
            self.status = "The target folder cannot be the source folder.".into();
            false
        } else {
            self.config.target_folders.push(TargetFolder::new(path));
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
        self.refresh_subfolders();
        if self.add_target_folder(path) {
            self.status = format!("\"{}\" added.", self.new_folder_name.trim());
            self.new_folder_name.clear();
            self.listening_for_key = Some(self.config.target_folders.len() - 1);
        }
    }

    pub fn remove_target_folder(&mut self, index: usize) {
        if index < self.config.target_folders.len() {
            self.config.target_folders.remove(index);
        }
        // Stop waiting for a key if that folder was the one being bound.
        if self.listening_for_key == Some(index) {
            self.listening_for_key = None;
        }
    }

    pub fn reveal_folder(&mut self, folder: &Path) {
        if !folder.is_dir() {
            self.status = format!("Folder \"{}\" no longer exists.", name_of(folder));
        } else if let Err(e) = files::reveal_in_file_manager(folder) {
            self.status = format!("Could not open the file explorer: {e}");
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

    // ── Contact sheet ───────────────────────────────────────────────────────

    pub fn toggle_view(&mut self) {
        match self.view {
            View::Single => {
                self.view = View::Grid;
                self.scroll_to_current = true;
            }
            View::Grid => {
                self.view = View::Single;
                // Free the thumbnail threads for the previews.
                self.loader.want_thumbnails(Vec::new());
            }
        }
    }

    /// Double-click on a thumbnail: show that photo on its own.
    pub fn open_in_single_view(&mut self, index: usize) {
        if index < self.photos.len() {
            self.current_index = index;
            self.drop_stale_full_texture();
            self.toggle_view();
        }
    }

    /// Click on a thumbnail, with the usual file-manager rules: a plain click
    /// selects only that photo, Ctrl+click adds or removes it, Shift+click
    /// selects everything from the last photo clicked without Shift.
    pub fn click_photo(&mut self, index: usize, ctrl: bool, shift: bool) {
        let Some(path) = self.photos.get(index).cloned() else {
            return;
        };
        let anchor = self
            .selection_anchor
            .as_ref()
            .and_then(|anchor| self.photos.iter().position(|p| p == anchor));

        match anchor {
            Some(anchor) if shift => {
                // The anchor stays where it is, so a second Shift+click
                // redraws the range from the same starting point.
                let (start, end) = (anchor.min(index), anchor.max(index));
                self.selection = self.photos[start..=end].iter().cloned().collect();
            }
            _ if ctrl => {
                if !self.selection.remove(&path) {
                    self.selection.insert(path.clone());
                }
                self.selection_anchor = Some(path);
            }
            _ => {
                self.selection = HashSet::from([path.clone()]);
                self.selection_anchor = Some(path);
            }
        }
        self.current_index = index;
        self.drop_stale_full_texture();
    }

    /// `Space` on the contact sheet: select or deselect the current photo.
    pub fn toggle_current_selected(&mut self) {
        if let Some(path) = self.current_photo().cloned() {
            if !self.selection.remove(&path) {
                self.selection.insert(path.clone());
            }
            self.selection_anchor = Some(path);
        }
    }

    pub fn select_all(&mut self) {
        self.selection = self.photos.iter().cloned().collect();
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    /// Queues thumbnails for the photos in `range`, in order, and evicts
    /// those far away from it once the cache is full.
    pub fn request_thumbnails(&mut self, range: Range<usize>) {
        let len = self.photos.len();
        let range = range.start.min(len)..range.end.min(len);

        let wanted: Vec<PathBuf> = self.photos[range.clone()]
            .iter()
            .filter(|p| !self.thumbnails.contains_key(*p) && !self.unreadable.contains(*p))
            .cloned()
            .collect();
        self.loader.want_thumbnails(wanted);

        if self.thumbnails.len() > THUMB_CACHE_SIZE {
            let margin = THUMB_CACHE_SIZE / 4;
            let keep_range = range.start.saturating_sub(margin)..(range.end + margin).min(len);
            let keep: HashSet<&PathBuf> = self.photos[keep_range].iter().collect();
            self.thumbnails.retain(|path, _| keep.contains(path));
        }
    }

    // ── Sorting ─────────────────────────────────────────────────────────────

    /// What a sort key moves: the selection on the contact sheet if there is
    /// one, otherwise the current photo. In list order.
    fn sort_targets(&self) -> Vec<PathBuf> {
        if self.view == View::Grid && !self.selection.is_empty() {
            self.photos
                .iter()
                .filter(|p| self.selection.contains(*p))
                .cloned()
                .collect()
        } else {
            self.current_photo().cloned().into_iter().collect()
        }
    }

    pub fn sort_into(&mut self, folder_idx: usize) {
        let targets = self.sort_targets();
        if targets.is_empty() {
            return;
        }
        let Some(folder) = self.config.target_folders.get(folder_idx) else {
            return;
        };
        let target_dir = folder.path.clone();
        let label = folder.display_name();
        if !target_dir.is_dir() {
            self.status = format!("Folder \"{label}\" no longer exists.");
            return;
        }
        // Possible since the source can be changed to one of the targets.
        if Some(&target_dir) == self.config.source_folder.as_ref() {
            self.status = format!("\"{label}\" is the source folder.");
            return;
        }

        let mut batch = Vec::new();
        let mut renamed = Vec::new();
        let mut error = None;
        for source in targets {
            let Some(index) = self.photos.iter().position(|p| *p == source) else {
                continue;
            };
            let Some(file_name) = source.file_name() else {
                continue;
            };
            let destination = files::unique_destination(&target_dir, file_name);
            if let Err(e) = files::move_file(&source, &destination) {
                error = Some(e);
                continue;
            }
            if destination.file_name() != Some(file_name) {
                renamed.push(name_of(&destination));
            }

            // Photos leave the list one at a time, in list order, and each
            // record keeps its index at that moment. Undoing in reverse order
            // then puts every photo back exactly where it was.
            self.photos.remove(index);
            if index < self.current_index {
                self.current_index -= 1;
            }
            self.textures.remove(&source);
            self.thumbnails.remove(&source);
            self.selection.remove(&source);
            batch.push(MoveRecord {
                from: source,
                to: destination,
                index,
            });
        }
        self.clamp_index();
        self.drop_stale_full_texture();
        self.scroll_to_current = true;

        let mut status = match batch.as_slice() {
            [] => String::new(),
            [record] => format!("{} -> {label}", name_of(&record.from)),
            _ => format!("{} photos -> {label}", batch.len()),
        };
        match renamed.as_slice() {
            [] => {}
            [new_name] if batch.len() == 1 => {
                status += &format!("  (renamed to {new_name}: a file with that name existed)");
            }
            _ => status += &format!("  ({} renamed: names already taken)", renamed.len()),
        }
        if let Some(e) = error {
            if batch.is_empty() {
                status = format!("Move failed: {e}");
            } else {
                status += &format!("  Some moves failed: {e}");
            }
        }
        self.status = status;

        if !batch.is_empty() {
            self.history.push(batch);
        }
    }

    /// Undoes the last sort action, whether it moved one photo or many.
    pub fn undo(&mut self) {
        let Some(batch) = self.history.pop() else {
            self.status = "Nothing to undo.".into();
            return;
        };

        let mut restored = Vec::new();
        let mut failed = Vec::new();
        let mut error = None;
        // Reverse order: see `sort_into`.
        for record in batch.into_iter().rev() {
            match files::move_file(&record.to, &record.from) {
                Ok(()) => {
                    let index = record.index.min(self.photos.len());
                    self.photos.insert(index, record.from.clone());
                    self.current_index = index;
                    restored.push(record.from);
                }
                Err(e) => {
                    error = Some(e);
                    failed.push(record);
                }
            }
        }
        // Files that did not move keep their record, so the user can retry.
        if !failed.is_empty() {
            failed.reverse();
            self.history.push(failed);
        }
        self.drop_stale_full_texture();
        self.scroll_to_current = true;
        // Reselect a restored batch, so it can be sorted again at once.
        if self.view == View::Grid && !restored.is_empty() {
            self.selection = restored.iter().cloned().collect();
        }

        self.status = match restored.as_slice() {
            [] => String::new(),
            [path] => format!("Undone: {} is back in the source folder.", name_of(path)),
            _ => format!("Undone: {} photos are back in the source folder.", restored.len()),
        };
        if let Some(e) = error {
            if restored.is_empty() {
                self.status = format!("Could not undo: {e}");
            } else {
                self.status += &format!("  Some could not be moved back: {e}");
            }
        }
    }

    // ── Textures ────────────────────────────────────────────────────────────

    /// Requests previews for the current photo and its neighbours, and evicts
    /// everything far away from the cache.
    pub fn request_nearby(&mut self) {
        let wanted: Vec<PathBuf> = PRELOAD_OFFSETS
            .iter()
            .filter_map(|offset| {
                let index = self.current_index as isize + offset;
                usize::try_from(index).ok()
            })
            .filter_map(|index| self.photos.get(index).cloned())
            .collect();

        for path in &wanted {
            if !self.textures.contains_key(path) && !self.unreadable.contains(path) {
                self.loader.request(path, Quality::Preview);
            }
        }

        if self.textures.len() > TEXTURE_CACHE_SIZE {
            self.textures.retain(|path, _| wanted.contains(path));
        }
    }

    /// Turns everything the background threads decoded into GPU textures.
    pub fn collect_loaded(&mut self, ctx: &egui::Context) {
        for (path, quality, result) in self.loader.drain() {
            let decoded = match result {
                Ok(decoded) => decoded,
                Err(e) => {
                    self.status = format!("Unreadable image ({}): {e}", name_of(&path));
                    // Remembered, or it would be requested again on the next
                    // frame, fail again, trigger a repaint, and so on forever.
                    self.unreadable.insert(path);
                    continue;
                }
            };

            let name = format!("{}:{quality:?}", path.to_string_lossy());
            let texture = ctx.load_texture(name, decoded.image, TextureOptions::LINEAR);

            match quality {
                Quality::Thumb => {
                    // Thumbnails from a previous source folder can still
                    // arrive after the switch.
                    if self.photos.contains(&path) {
                        self.thumbnails.insert(path, texture);
                    }
                }
                Quality::Preview => {
                    self.textures.insert(
                        path,
                        Preview {
                            texture,
                            original_size: decoded.original_size,
                        },
                    );
                }
                Quality::Full => {
                    // Dropped if the user moved on while it was decoding.
                    if self.current_photo() == Some(&path) {
                        self.full_texture = Some((path, texture));
                    }
                }
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

/// File name for status messages.
fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

// ── Geometry ────────────────────────────────────────────────────────────────
// Pure functions with no egui context, so they can be unit-tested.

/// How many contact-sheet columns fit in `width`, with cells at least
/// [`MIN_CELL`] wide. Always at least one.
pub fn grid_columns(width: f32) -> usize {
    ((width / MIN_CELL).floor() as usize).max(1)
}

/// The contact-sheet scroll offset that brings `row` fully into view, or
/// `None` if it already is. Rows are `cell` tall; `offset` and `height`
/// describe the visible part.
pub fn offset_to_reveal(row: usize, cell: f32, offset: f32, height: f32) -> Option<f32> {
    let top = row as f32 * cell;
    let bottom = top + cell;
    if top < offset || cell > height {
        Some(top)
    } else if bottom > offset + height {
        Some(bottom - height)
    } else {
        None
    }
}

/// Scale at which the whole photo just fits in the window. Never above 1.0:
/// enlarging a small image by default only makes it blurry.
pub fn fit_scale(view: Vec2, image: Vec2) -> f32 {
    (view.x / image.x).min(view.y / image.y).min(1.0)
}

/// Restricts `pan` so the viewport never runs past the edge of the image.
/// An image smaller than the viewport is pinned to the centre.
pub fn clamped_pan(pan: Vec2, view: Vec2, image: Vec2, scale: f32) -> Vec2 {
    let visible = view / scale; // in image pixels
    let max_x = ((image.x - visible.x) / 2.0).max(0.0);
    let max_y = ((image.y - visible.y) / 2.0).max(0.0);
    Vec2::new(pan.x.clamp(-max_x, max_x), pan.y.clamp(-max_y, max_y))
}

/// Which slice of the image to show: returns its on-screen size in pixels and
/// its UV corners (0..1). UVs don't depend on texture resolution, so preview
/// and full-resolution textures give the same framing.
///
/// `pan` must already have been through [`clamped_pan`].
pub fn visible_region(pan: Vec2, view: Vec2, image: Vec2, scale: f32) -> (Vec2, Vec2, Vec2) {
    let visible = Vec2::new((view.x / scale).min(image.x), (view.y / scale).min(image.y));
    let centre = image / 2.0 + pan;
    let top_left = centre - visible / 2.0;

    let uv_min = Vec2::new(top_left.x / image.x, top_left.y / image.y);
    let uv_max = Vec2::new(
        (top_left.x + visible.x) / image.x,
        (top_left.y + visible.y) / image.y,
    );
    (visible * scale, uv_min, uv_max)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: Vec2 = Vec2::new(1000.0, 800.0);
    const IMAGE: Vec2 = Vec2::new(6000.0, 4000.0);

    #[test]
    fn pan_cannot_run_past_the_edge() {
        // A wild drag to the left must stop exactly at the image border.
        let pan = clamped_pan(Vec2::new(-99_999.0, 0.0), VIEW, IMAGE, 1.0);
        assert_eq!(pan.x, -(6000.0 - 1000.0) / 2.0);

        let pan = clamped_pan(Vec2::new(0.0, 99_999.0), VIEW, IMAGE, 1.0);
        assert_eq!(pan.y, (4000.0 - 800.0) / 2.0);
    }

    #[test]
    fn zooming_in_tightens_the_pan_limit() {
        // At 2:1 only half as many image pixels fit, so you can wander further
        // from the centre before hitting the edge.
        let at_1to1 = clamped_pan(Vec2::new(-99_999.0, 0.0), VIEW, IMAGE, 1.0);
        let at_2to1 = clamped_pan(Vec2::new(-99_999.0, 0.0), VIEW, IMAGE, 2.0);
        assert!(at_2to1.x < at_1to1.x);
        assert_eq!(at_2to1.x, -(6000.0 - 500.0) / 2.0);
    }

    #[test]
    fn an_image_smaller_than_the_view_stays_centred() {
        let small = Vec2::new(400.0, 300.0);
        let pan = clamped_pan(Vec2::new(250.0, -120.0), VIEW, small, fit_scale(VIEW, small));
        assert_eq!(pan, Vec2::ZERO);
    }

    #[test]
    fn uv_stays_inside_the_texture_at_every_extreme() {
        // The failure this guards against: dragging to a corner and seeing the
        // image wrap around or sample outside itself.
        for scale in [0.2_f32, 1.0, 2.5, MAX_ZOOM] {
            for raw in [
                Vec2::new(-99_999.0, -99_999.0),
                Vec2::new(99_999.0, 99_999.0),
                Vec2::new(-99_999.0, 99_999.0),
                Vec2::ZERO,
            ] {
                let pan = clamped_pan(raw, VIEW, IMAGE, scale);
                let (_, uv_min, uv_max) = visible_region(pan, VIEW, IMAGE, scale);

                for value in [uv_min.x, uv_min.y, uv_max.x, uv_max.y] {
                    assert!(
                        (0.0..=1.0).contains(&value),
                        "uv out of range: {value} (scale {scale}, pan {pan:?})"
                    );
                }
                assert!(uv_min.x < uv_max.x && uv_min.y < uv_max.y);
            }
        }
    }

    #[test]
    fn fit_shows_the_whole_photo() {
        let scale = fit_scale(VIEW, IMAGE);
        let (screen, uv_min, uv_max) = visible_region(Vec2::ZERO, VIEW, IMAGE, scale);
        assert_eq!(uv_min, Vec2::ZERO);
        assert_eq!(uv_max, Vec2::new(1.0, 1.0));
        // and it fits inside the window
        assert!(screen.x <= VIEW.x + 0.01 && screen.y <= VIEW.y + 0.01);
    }

    #[test]
    fn one_to_one_fills_the_viewport() {
        let (screen, _, _) = visible_region(Vec2::ZERO, VIEW, IMAGE, 1.0);
        assert_eq!(screen, VIEW);
    }

    #[test]
    fn a_wheel_notch_up_zooms_in_and_down_zooms_out() {
        let up = PhotoSorter::wheel_zoom_factor(POINTS_PER_NOTCH);
        let down = PhotoSorter::wheel_zoom_factor(-POINTS_PER_NOTCH);
        assert!((up - ZOOM_STEP).abs() < 1e-5);
        assert!((down - 1.0 / ZOOM_STEP).abs() < 1e-5);
        // opposite notches cancel out exactly
        assert!((up * down - 1.0).abs() < 1e-5);
    }

    #[test]
    fn grid_columns_fill_the_width_and_never_drop_to_zero() {
        assert_eq!(grid_columns(MIN_CELL * 5.5), 5);
        assert_eq!(grid_columns(10.0), 1);
    }

    #[test]
    fn revealing_a_row_scrolls_only_when_it_is_out_of_view() {
        // Rows of 100, viewport showing 250..650.
        assert_eq!(offset_to_reveal(3, 100.0, 250.0, 400.0), None);
        assert_eq!(offset_to_reveal(1, 100.0, 250.0, 400.0), Some(100.0)); // above
        assert_eq!(offset_to_reveal(7, 100.0, 250.0, 400.0), Some(400.0)); // below
    }

    // ── Contact sheet and batch moves, on real files ──

    /// An app whose source folder holds a.jpg … e.jpg, with one empty target
    /// folder. The files are not real images; nothing here decodes them.
    fn sorter_with_photos(test: &str) -> (PhotoSorter, PathBuf) {
        let dir = std::env::temp_dir().join(format!("photo_sorter_test_app_{test}"));
        let _ = std::fs::remove_dir_all(&dir);
        let (source, target) = (dir.join("source"), dir.join("target"));
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        for name in ["a", "b", "c", "d", "e"] {
            std::fs::write(source.join(format!("{name}.jpg")), name).unwrap();
        }

        let mut app = PhotoSorter::with_config(Config::default(), egui::Context::default());
        app.load_photos_from(source);
        app.add_target_folder(target.clone());
        (app, target)
    }

    fn stems<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) -> Vec<String> {
        let mut stems: Vec<String> = paths
            .into_iter()
            .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
            .collect();
        stems.sort();
        stems
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let paths: Vec<PathBuf> = files::list_photos(dir);
        stems(&paths)
    }

    #[test]
    fn shift_click_selects_a_range_and_ctrl_click_toggles() {
        let (mut app, _) = sorter_with_photos("select");
        app.click_photo(1, false, false);
        app.click_photo(3, false, true);
        assert_eq!(stems(&app.selection), ["b", "c", "d"]);

        app.click_photo(2, true, false);
        assert_eq!(stems(&app.selection), ["b", "d"]);

        // A plain click starts over.
        app.click_photo(4, false, false);
        assert_eq!(stems(&app.selection), ["e"]);
    }

    #[test]
    fn a_batch_moves_together_and_is_undone_in_one_step() {
        let (mut app, target) = sorter_with_photos("batch");
        app.view = View::Grid;
        app.click_photo(1, false, false);
        app.click_photo(3, true, false);

        app.sort_into(0);
        assert_eq!(files_in(&target), ["b", "d"]);
        assert_eq!(stems(&app.photos), ["a", "c", "e"]);
        assert!(app.selection.is_empty());

        app.undo();
        assert!(files_in(&target).is_empty());
        // Back in their original places, not appended at the end.
        let order: Vec<String> = app
            .photos
            .iter()
            .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(order, ["a", "b", "c", "d", "e"]);
        assert!(app.history.is_empty());
        assert_eq!(stems(&app.selection), ["b", "d"]);
    }

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
    fn a_target_opened_as_source_cannot_be_sorted_into() {
        let (mut app, target) = sorter_with_photos("target_as_source");
        std::fs::write(target.join("z.jpg"), "z").unwrap();
        app.load_photos_from(target.clone());
        app.sort_into(0);
        assert_eq!(files_in(&target), ["z"]);
        assert_eq!(stems(&app.photos), ["z"]);
    }

    #[test]
    fn without_a_selection_only_the_current_photo_moves() {
        let (mut app, target) = sorter_with_photos("no_selection");
        app.view = View::Grid;
        app.current_index = 2;
        app.sort_into(0);
        assert_eq!(files_in(&target), ["c"]);
    }

    #[test]
    fn the_single_view_ignores_the_selection() {
        let (mut app, target) = sorter_with_photos("single_view");
        app.view = View::Grid;
        app.select_all();
        app.toggle_view();
        app.current_index = 0;
        app.sort_into(0);
        assert_eq!(files_in(&target), ["a"]);
    }

    #[test]
    fn moving_photos_before_the_current_one_keeps_it_on_screen() {
        let (mut app, _) = sorter_with_photos("keep_current");
        app.view = View::Grid;
        app.click_photo(0, false, false);
        app.click_photo(1, true, false);
        app.current_index = 4;
        app.sort_into(0);
        assert_eq!(stems(app.current_photo()), ["e"]);
    }

    /// Screen offset from the viewport centre of a given image point.
    fn screen_offset_of(image_point: Vec2, pan: Vec2, image: Vec2, scale: f32) -> Vec2 {
        (image_point - (image / 2.0 + pan)) * scale
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor_still() {
        let anchor = Vec2::new(320.0, -180.0); // offset from the viewport centre
        let mut pan = Vec2::ZERO;
        let mut scale = 1.0_f32;

        // The image point currently sitting under the cursor.
        let target = IMAGE / 2.0 + pan + anchor / scale;

        for _ in 0..6 {
            let next = (scale * ZOOM_STEP).min(MAX_ZOOM);
            pan += anchor * (1.0 / scale - 1.0 / next);
            scale = next;

            let offset = screen_offset_of(target, pan, IMAGE, scale);
            assert!(
                (offset - anchor).length() < 0.01,
                "point drifted to {offset:?}, expected {anchor:?} at scale {scale}"
            );
        }
    }
}
