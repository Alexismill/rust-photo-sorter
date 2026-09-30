//! Image decoding on background threads (a 24 MP JPEG takes about 0.4 s,
//! which would freeze the UI).
//!
//! Three queues: previews are preloaded ahead of the user, so their queue is
//! often busy. A full-resolution request has its own thread so it does not
//! wait behind those preloads. Contact-sheet thumbnails have a pool of
//! threads and a queue that can be replaced wholesale (see [`ThumbQueue`]).

use egui::ColorImage;
use image::ImageDecoder; // brings the .orientation() method onto the decoder
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

/// Previews are scaled down to this maximum size before reaching the GPU.
/// A raw 24 MP frame takes 96 MB as RGBA; capped at 2400 px, about 23 MB.
pub const MAX_DISPLAY_PX: u32 = 2400;

/// Contact-sheet thumbnails are scaled down to this size (about 170 KB each).
pub const THUMB_PX: u32 = 256;

/// How much detail to decode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    /// Scaled down to [`THUMB_PX`], for the contact sheet.
    Thumb,
    /// Scaled down to [`MAX_DISPLAY_PX`]: what the fit-to-window view needs.
    Preview,
    /// Every pixel, for 1:1 zoom. Expensive, so only ever one at a time.
    Full,
}

impl Quality {
    fn max_size(self) -> Option<u32> {
        match self {
            Quality::Thumb => Some(THUMB_PX),
            Quality::Preview => Some(MAX_DISPLAY_PX),
            Quality::Full => None,
        }
    }
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

    if let Some(max) = quality.max_size() {
        if img.width().max(img.height()) > max {
            img = img.thumbnail(max, max);
        }
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
    thumbs: Arc<ThumbQueue>,
    receiver: Receiver<LoadResult>,
    pending: Vec<(PathBuf, Quality)>,
}

impl Loader {
    pub fn new(ctx: egui::Context) -> Self {
        let (result_tx, result_rx) = std::sync::mpsc::channel::<LoadResult>();
        let preview_sender = spawn_worker(Quality::Preview, result_tx.clone(), ctx.clone());
        let full_sender = spawn_worker(Quality::Full, result_tx.clone(), ctx.clone());

        let thumbs = Arc::new(ThumbQueue::default());
        for _ in 0..thumb_worker_count() {
            spawn_thumb_worker(thumbs.clone(), result_tx.clone(), ctx.clone());
        }

        Self {
            preview_sender,
            full_sender,
            thumbs,
            receiver: result_rx,
            pending: Vec::new(),
        }
    }

    pub fn is_pending(&self, path: &Path, quality: Quality) -> bool {
        self.pending
            .iter()
            .any(|(p, q)| p == path && *q == quality)
    }

    /// Requests a preview or full-resolution decode. No-op if that exact work
    /// is already queued. Thumbnails go through [`Self::want_thumbnails`].
    pub fn request(&mut self, path: &Path, quality: Quality) {
        let sender = match quality {
            Quality::Preview => &self.preview_sender,
            Quality::Full => &self.full_sender,
            Quality::Thumb => unreachable!("thumbnails are queued with want_thumbnails"),
        };
        if self.is_pending(path, quality) {
            return;
        }
        let _ = sender.send(path.to_path_buf());
        self.pending.push((path.to_path_buf(), quality));
    }

    /// Replaces the thumbnail queue with `paths`, in the order given.
    /// Anything queued before and not in `paths` is dropped, so scrolling
    /// quickly never leaves a backlog of photos that are no longer on screen.
    pub fn want_thumbnails(&self, paths: Vec<PathBuf>) {
        let mut state = self.thumbs.lock();
        state.queue = paths
            .into_iter()
            .filter(|p| !state.in_flight.contains(p))
            .collect();
        if !state.queue.is_empty() {
            self.thumbs.ready.notify_all();
        }
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
        self.thumbs.lock().queue.clear();
    }
}

impl Drop for Loader {
    /// The preview and full-resolution threads stop on their own when their
    /// channel is dropped; the thumbnail threads wait on a condition variable
    /// and have to be told.
    fn drop(&mut self) {
        self.thumbs.lock().shutdown = true;
        self.thumbs.ready.notify_all();
    }
}

/// Thumbnail work shared by the thumbnail threads.
///
/// A channel cannot be emptied from the sending side, which is what the
/// contact sheet needs: the set of visible photos changes on every scroll.
/// A `Mutex` around a plain queue can be replaced at will, and the `Condvar`
/// lets idle threads sleep until there is work.
#[derive(Default)]
struct ThumbQueue {
    state: Mutex<ThumbState>,
    ready: Condvar,
}

#[derive(Default)]
struct ThumbState {
    queue: VecDeque<PathBuf>,
    /// Being decoded right now, so not worth queueing again.
    in_flight: HashSet<PathBuf>,
    shutdown: bool,
}

impl ThumbQueue {
    fn lock(&self) -> std::sync::MutexGuard<'_, ThumbState> {
        // A poisoned lock means a thread panicked while holding it, which the
        // code below never does (decoding happens outside the lock).
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Half the cores, between 1 and 4: enough to fill a screen of thumbnails
/// quickly without starving the UI and preview threads.
fn thumb_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get() / 2)
        .unwrap_or(1)
        .clamp(1, 4)
}

fn spawn_thumb_worker(queue: Arc<ThumbQueue>, results: Sender<LoadResult>, ctx: egui::Context) {
    std::thread::spawn(move || loop {
        let path = {
            let mut state = queue.lock();
            loop {
                if state.shutdown {
                    return;
                }
                if let Some(path) = state.queue.pop_front() {
                    state.in_flight.insert(path.clone());
                    break path;
                }
                state = queue.ready.wait(state).unwrap_or_else(|e| e.into_inner());
            }
        };

        let result = decode(&path, Quality::Thumb);
        // Send before leaving `in_flight`: the other order leaves a moment
        // where the UI has no texture and no in-flight entry, and would
        // queue the same thumbnail again.
        let sent = results.send((path.clone(), Quality::Thumb, result)).is_ok();
        queue.lock().in_flight.remove(&path);
        if !sent {
            return;
        }
        ctx.request_repaint();
    });
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
    fn thumbnails_are_scaled_to_thumb_size() {
        let decoded = decode(&large_image_path(), Quality::Thumb).unwrap();
        assert_eq!(decoded.image.size[0], THUMB_PX as usize);
        assert!(decoded.image.size[1] < THUMB_PX as usize);
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
