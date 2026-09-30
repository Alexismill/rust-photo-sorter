//! Application state and sorting logic.
//!
//! This module draws nothing: it decides. Rendering lives in `ui.rs`.

use crate::config::{Config, TargetFolder};
use crate::files;
use crate::loader::{Loader, Quality};
use eframe::CreationContext;
use egui::{Key, TextureHandle, TextureOptions, Vec2};
use std::collections::HashMap;
use std::path::PathBuf;

/// How many preview textures stay in memory: the current photo and its
/// neighbours.
const TEXTURE_CACHE_SIZE: usize = 5;

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

/// A completed move, kept so it can be undone.
pub struct MoveRecord {
    pub from: PathBuf,
    pub to: PathBuf,
    pub index: usize,
}

pub struct PhotoSorter {
    pub config: Config,

    pub photos: Vec<PathBuf>,
    pub current_index: usize,

    /// Downscaled images for everyday viewing, one per nearby photo.
    pub textures: HashMap<PathBuf, Preview>,
    /// Full-resolution image, loaded once the preview is not sharp enough for
    /// the current zoom. At most one (a 24 MP frame is ~96 MB on the GPU).
    pub full_texture: Option<(PathBuf, TextureHandle)>,
    pub loader: Loader,

    /// Kept across navigation, to compare a burst at the same magnification.
    pub zoom: ZoomMode,
    /// Offset of the view centre from the image centre, in image pixels.
    /// Also kept across navigation.
    pub pan: Vec2,

    /// When `Some(i)`, we are waiting for a key to bind to folder `i`.
    pub listening_for_key: Option<usize>,
    pub history: Vec<MoveRecord>,
    pub status: String,
}

impl PhotoSorter {
    pub fn new(cc: &CreationContext<'_>) -> Self {
        let config: Config = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, eframe::APP_KEY))
            .unwrap_or_default();

        let mut app = Self {
            config,
            photos: Vec::new(),
            current_index: 0,
            textures: HashMap::new(),
            full_texture: None,
            loader: Loader::new(cc.egui_ctx.clone()),
            zoom: ZoomMode::Fit,
            pan: Vec2::ZERO,
            listening_for_key: None,
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
        self.textures.clear();
        self.full_texture = None;
        self.reset_zoom();
        self.loader.forget_pending();
        self.history.clear();
        self.status = match self.photos.len() {
            0 => "No images in this folder.".to_string(),
            1 => "1 photo found.".to_string(),
            n => format!("{n} photos found."),
        };
        self.config.source_folder = Some(folder);
    }

    pub fn current_photo(&self) -> Option<&PathBuf> {
        self.photos.get(self.current_index)
    }

    // ── Navigation ──────────────────────────────────────────────────────────

    pub fn next(&mut self) {
        if !self.photos.is_empty() {
            self.current_index = (self.current_index + 1).min(self.photos.len() - 1);
            self.drop_stale_full_texture();
        }
    }

    pub fn previous(&mut self) {
        self.current_index = self.current_index.saturating_sub(1);
        self.drop_stale_full_texture();
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

    pub fn add_target_folder(&mut self, path: PathBuf) {
        if self.config.target_folders.iter().any(|f| f.path == path) {
            self.status = "That folder is already in the list.".into();
        } else if Some(&path) == self.config.source_folder.as_ref() {
            self.status = "The target folder cannot be the source folder.".into();
        } else {
            self.config.target_folders.push(TargetFolder::new(path));
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

    // ── Sorting ─────────────────────────────────────────────────────────────

    pub fn sort_current_into(&mut self, folder_idx: usize) {
        let Some(source) = self.current_photo().cloned() else { return };
        let Some(folder) = self.config.target_folders.get(folder_idx) else { return };
        let target_dir = folder.path.clone();
        let label = folder.display_name();

        if !target_dir.is_dir() {
            self.status = format!("Folder \"{label}\" no longer exists.");
            return;
        }
        let Some(file_name) = source.file_name() else { return };
        let destination = files::unique_destination(&target_dir, file_name);

        match files::move_file(&source, &destination) {
            Ok(()) => {
                let was_renamed = destination.file_name() != Some(file_name);
                self.status = if was_renamed {
                    format!(
                        "{} -> {label}  (renamed to {}: a file with that name existed)",
                        file_name.to_string_lossy(),
                        destination.file_name().unwrap_or_default().to_string_lossy()
                    )
                } else {
                    format!("{} -> {label}", file_name.to_string_lossy())
                };
                self.history.push(MoveRecord {
                    from: source.clone(),
                    to: destination,
                    index: self.current_index,
                });
                self.textures.remove(&source);
                self.photos.remove(self.current_index);
                self.clamp_index();
                self.drop_stale_full_texture();
            }
            Err(e) => self.status = format!("Move failed: {e}"),
        }
    }

    pub fn undo(&mut self) {
        let Some(record) = self.history.pop() else {
            self.status = "Nothing to undo.".into();
            return;
        };
        match files::move_file(&record.to, &record.from) {
            Ok(()) => {
                let index = record.index.min(self.photos.len());
                self.photos.insert(index, record.from.clone());
                self.current_index = index;
                self.drop_stale_full_texture();
                self.status = format!(
                    "Undone: {} is back in the source folder.",
                    record.from.file_name().unwrap_or_default().to_string_lossy()
                );
            }
            Err(e) => {
                // The file did not move, so put the record back and let the
                // user try again.
                self.status = format!("Could not undo: {e}");
                self.history.push(record);
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
            if !self.textures.contains_key(path) {
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
                    self.status = format!(
                        "Unreadable image ({}): {e}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                    continue;
                }
            };

            let name = format!("{}:{quality:?}", path.to_string_lossy());
            let texture = ctx.load_texture(name, decoded.image, TextureOptions::LINEAR);

            match quality {
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

// ── Zoom geometry ───────────────────────────────────────────────────────────
// Pure functions with no egui context, so they can be unit-tested.

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
