// No console window behind the app in release builds on Windows. Debug builds
// keep it so `println!` and panics stay visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Photo Sorter: keyboard-driven photo sorting.
//!
//!   config.rs  — target folders and shortcuts, persisted between runs
//!   files.rs   — listing, unique names, moving (no UI dependency)
//!   loader.rs  — image decoding on background threads
//!   app.rs     — state and sorting logic
//!   folders.rs — source folder browsing and the target folder list
//!   ui.rs      — egui rendering

mod app;
mod config;
mod files;
mod folders;
mod loader;
mod ui;

use app::PhotoSorter;
use eframe::egui;

/// Taskbar and title-bar icon. The icon Explorer shows on the .exe file is a
/// separate thing, embedded by `build.rs`.
fn window_icon() -> Option<egui::IconData> {
    let png = include_bytes!("../assets/icon.png");
    let image = image::load_from_memory(png).ok()?.into_rgba8();
    let (width, height) = (image.width(), image.height());
    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 860.0])
        .with_title("Photo Sorter");
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "photo-sorter",
        options,
        Box::new(|cc| Ok(Box::new(PhotoSorter::new(cc)))),
    )
}
