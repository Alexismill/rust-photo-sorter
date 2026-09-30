//! Image decoding on background threads (a 24 MP JPEG takes about 0.4 s,
//! which would freeze the UI).
//!
//! Two queues: previews are preloaded ahead of the user, so their queue is
//! often busy. A full-resolution request has its own thread so it does not
//! wait behind those preloads.

use egui::ColorImage;
use image::ImageDecoder; // brings the .orientation() method onto the decoder
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

/// Previews are scaled down to this maximum size before reaching the GPU.
/// A raw 24 MP frame takes 96 MB as RGBA; capped at 2400 px, about 23 MB.
pub const MAX_DISPLAY_PX: u32 = 2400;

/// How much detail to decode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    /// Scaled down to [`MAX_DISPLAY_PX`]: what the fit-to-window view needs.
    Preview,
    /// Every pixel, for 1:1 zoom. Expensive, so only ever one at a time.
    Full,
}

/// A decoded image, plus the size it had before any downscaling, so that
/// "1:1" means one photo pixel per screen pixel, not one preview pixel.
pub struct Decoded {
    pub image: ColorImage,
    pub original_size: egui::Vec2,
}

pub type LoadResult = (PathBuf, Quality, Result<Decoded, String>);

/// Decodes an image and applies its EXIF orientation.
pub fn decode(path: &Path, quality: Quality) -> Result<Decoded, String> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;

    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;

    // Phone photos are stored in the sensor's own orientation and set upright
    // by an EXIF tag. Skip this step and every portrait shot shows up sideways.
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);

    let mut img = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);

    // Measured after rotation, so a portrait shot reports portrait dimensions.
    let original_size = egui::Vec2::new(img.width() as f32, img.height() as f32);

    if quality == Quality::Preview && img.width().max(img.height()) > MAX_DISPLAY_PX {
        img = img.thumbnail(MAX_DISPLAY_PX, MAX_DISPLAY_PX);
    }

    let rgba = img.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(Decoded {
        image: ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()),
        original_size,
    })
}

/// Handle to the decoding threads.
///
/// Tracks what has already been requested, so the same work is never queued
/// twice.
pub struct Loader {
    preview_sender: Sender<PathBuf>,
    full_sender: Sender<PathBuf>,
    receiver: Receiver<LoadResult>,
    pending: Vec<(PathBuf, Quality)>,
}

impl Loader {
    pub fn new(ctx: egui::Context) -> Self {
        let (result_tx, result_rx) = std::sync::mpsc::channel::<LoadResult>();
        let preview_sender = spawn_worker(Quality::Preview, result_tx.clone(), ctx.clone());
        let full_sender = spawn_worker(Quality::Full, result_tx, ctx);

        Self {
            preview_sender,
            full_sender,
            receiver: result_rx,
            pending: Vec::new(),
        }
    }

    pub fn is_pending(&self, path: &Path, quality: Quality) -> bool {
        self.pending
            .iter()
            .any(|(p, q)| p == path && *q == quality)
    }

    /// Requests a decode. No-op if that exact work is already queued.
    pub fn request(&mut self, path: &Path, quality: Quality) {
        if self.is_pending(path, quality) {
            return;
        }
        self.pending.push((path.to_path_buf(), quality));
        let sender = match quality {
            Quality::Preview => &self.preview_sender,
            Quality::Full => &self.full_sender,
        };
        let _ = sender.send(path.to_path_buf());
    }

    /// Collects everything the threads have finished decoding. Never blocks.
    pub fn drain(&mut self) -> Vec<LoadResult> {
        let mut finished = Vec::new();
        while let Ok(result) = self.receiver.try_recv() {
            let (path, quality, _) = &result;
            self.pending.retain(|(p, q)| p != path || q != quality);
            finished.push(result);
        }
        finished
    }

    /// Forgets in-flight requests, used when the source folder changes.
    pub fn forget_pending(&mut self) {
        self.pending.clear();
    }
}

/// Starts one worker thread and returns the channel that feeds it.
fn spawn_worker(
    quality: Quality,
    results: Sender<LoadResult>,
    ctx: egui::Context,
) -> Sender<PathBuf> {
    let (request_tx, request_rx) = std::sync::mpsc::channel::<PathBuf>();

    std::thread::spawn(move || {
        // recv() blocks without burning CPU, and returns Err once the other
        // end of the channel is dropped, i.e. when the app shuts down.
        while let Ok(path) = request_rx.recv() {
            let result = decode(&path, quality);
            if results.send((path, quality, result)).is_err() {
                break;
            }
            // Wake the UI up: otherwise it sleeps until the next event.
            ctx.request_repaint();
        }
    });

    request_tx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn large_image_path() -> PathBuf {
        let dir = std::env::temp_dir().join("photo_sorter_test_large");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("large.png");
        if !path.exists() {
            let large = image::RgbImage::new(MAX_DISPLAY_PX + 1200, MAX_DISPLAY_PX);
            image::DynamicImage::ImageRgb8(large).save(&path).unwrap();
        }
        path
    }

    #[test]
    fn applies_exif_orientation() {
        // The file is 200x100 on disk and carries EXIF Orientation = 6
        // (rotate 90 degrees), so it must come out as 100x200.
        let decoded = decode(&fixture("portrait_exif.jpg"), Quality::Preview).unwrap();
        assert_eq!(
            decoded.image.size,
            [100, 200],
            "EXIF orientation was not applied: portrait photos will show up sideways"
        );
    }

    #[test]
    fn preview_scales_large_images_down() {
        let decoded = decode(&large_image_path(), Quality::Preview).unwrap();
        let size = decoded.image.size;
        assert!(
            size[0].max(size[1]) <= MAX_DISPLAY_PX as usize,
            "image was not scaled down: {size:?}"
        );
        // the aspect ratio is preserved
        let ratio = size[0] as f32 / size[1] as f32;
        let expected = (MAX_DISPLAY_PX + 1200) as f32 / MAX_DISPLAY_PX as f32;
        assert!((ratio - expected).abs() < 0.01, "distorted ratio: {ratio}");
    }

    #[test]
    fn a_preview_still_reports_the_original_size() {
        // Zoom levels are expressed against the real image, so a downscaled
        // preview must still say how big the photo actually is.
        let decoded = decode(&large_image_path(), Quality::Preview).unwrap();
        assert_eq!(
            decoded.original_size,
            egui::Vec2::new((MAX_DISPLAY_PX + 1200) as f32, MAX_DISPLAY_PX as f32)
        );
        assert!(decoded.image.size[0] < decoded.original_size.x as usize);
    }

    #[test]
    fn full_quality_keeps_every_pixel() {
        // This is what makes 1:1 zoom meaningful: no resampling at all, so
        // what the user inspects is the real sensor data.
        let decoded = decode(&large_image_path(), Quality::Full).unwrap();
        assert_eq!(
            decoded.image.size,
            [(MAX_DISPLAY_PX + 1200) as usize, MAX_DISPLAY_PX as usize]
        );
    }

    #[test]
    fn reports_an_unreadable_file() {
        let dir = std::env::temp_dir().join("photo_sorter_test_unreadable");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.jpg");
        std::fs::write(&path, b"this is not an image").unwrap();

        assert!(
            decode(&path, Quality::Preview).is_err(),
            "should have returned an error"
        );
    }
}
