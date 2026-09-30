//! egui rendering. Draws and forwards input to `PhotoSorter`; no sorting
//! logic here.

use crate::app::{visible_region, PhotoSorter, ZoomMode};
use eframe::{egui, App};
use egui::{Color32, Key, Rect, Sense, Vec2};

const GREEN: Color32 = Color32::from_rgb(120, 200, 120);

impl App for PhotoSorter {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.config);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.collect_loaded(&ctx);

        // The "press a key" mode takes over the whole window and short-circuits
        // the shortcuts, otherwise the chosen key would also trigger a move.
        if self.listening_for_key.is_some() {
            self.key_capture_screen(ui, &ctx);
            return;
        }

        self.handle_shortcuts(&ctx);
        self.request_nearby();

        egui::Panel::left("folders")
            .min_size(260.0)
            .show(ui, |ui| self.folders_panel(ui));

        egui::Panel::bottom("status")
            .resizable(false)
            .show(ui, |ui| {
                ui.add_space(2.0);
                ui.label(&self.status);
                ui.add_space(2.0);
            });

        egui::CentralPanel::default_margins().show(ui, |ui| self.photo_panel(ui));
    }
}

impl PhotoSorter {
    // ── Keyboard ────────────────────────────────────────────────────────────

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::Z)) {
            self.undo();
            return;
        }

        if ctx.input(|i| i.key_pressed(Key::Space)) {
            self.toggle_zoom();
        }
        if self.zoom != ZoomMode::Fit && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.reset_zoom();
        }

        if !self.photos.is_empty() {
            for (folder_idx, key) in self.config.shortcuts() {
                if ctx.input(|i| i.key_pressed(key) && !i.modifiers.command) {
                    self.sort_current_into(folder_idx);
                    break;
                }
            }
        }

        // Navigation stays live while zoomed: that is how you compare two
        // frames of a burst at the same magnification.
        if ctx.input(|i| i.key_pressed(Key::ArrowRight)) {
            self.next();
        }
        if ctx.input(|i| i.key_pressed(Key::ArrowLeft)) {
            self.previous();
        }
    }

    fn key_capture_screen(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(folder_idx) = self.listening_for_key else { return };

        let mut chosen: Option<Key> = None;
        let mut cancelled = false;
        ctx.input(|input| {
            for event in &input.events {
                if let egui::Event::Key { key, pressed: true, repeat: false, .. } = event {
                    if *key == Key::Escape {
                        cancelled = true;
                    } else if !crate::config::is_reserved(*key) {
                        chosen = Some(*key);
                    }
                }
            }
        });

        if cancelled {
            self.listening_for_key = None;
            self.status = "Key binding cancelled.".into();
        } else if let Some(key) = chosen {
            if self.assign_shortcut(folder_idx, key) {
                self.listening_for_key = None;
            }
            // If the key was taken we stay in capture mode; the error message
            // is already in the status bar.
        }

        let folder_name = self
            .config
            .target_folders
            .get(folder_idx)
            .map(|f| f.display_name())
            .unwrap_or_default();

        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "Press the key for \"{folder_name}\"\n\n{}\n(Esc to cancel)",
                        self.status
                    ))
                    .size(24.0),
                );
            });
        });
    }

    // ── Folders sidebar ─────────────────────────────────────────────────────

    fn folders_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.heading("Target folders");
        ui.separator();

        // egui renders while we iterate over `target_folders`, so the list
        // cannot be modified inside the loop: record the requested action and
        // apply it afterwards.
        let mut start_listening: Option<usize> = None;
        let mut remove: Option<usize> = None;
        let mut sort_into: Option<usize> = None;

        for (i, folder) in self.config.target_folders.iter().enumerate() {
            ui.horizontal(|ui| {
                let (label, color) = match folder.shortcut {
                    Some(key) => (format!("{key:?}"), GREEN),
                    None => ("--".to_string(), Color32::GRAY),
                };
                let key_button = egui::Button::new(
                    egui::RichText::new(label).monospace().strong().color(color),
                )
                .min_size(egui::vec2(34.0, 22.0));

                if ui
                    .add(key_button)
                    .on_hover_text("Click to set or change the shortcut")
                    .clicked()
                {
                    start_listening = Some(i);
                }

                if ui
                    .button(folder.display_name())
                    .on_hover_text(folder.path.display().to_string())
                    .clicked()
                {
                    sort_into = Some(i);
                }

                if ui.small_button("x").on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }

        if let Some(i) = start_listening {
            self.listening_for_key = Some(i);
        }
        if let Some(i) = remove {
            self.remove_target_folder(i);
        }
        if let Some(i) = sort_into {
            self.sort_current_into(i);
        }

        ui.add_space(4.0);
        if ui.button("+ Add target folder").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                self.add_target_folder(path);
            }
        }

        ui.add_space(18.0);
        ui.heading("Source");
        ui.separator();
        if ui.button("Open source folder").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                self.load_photos_from(path);
            }
        }
        if let Some(folder) = &self.config.source_folder {
            ui.label(
                egui::RichText::new(folder.display().to_string())
                    .small()
                    .weak(),
            );
        }
        if !self.photos.is_empty() {
            ui.label(format!(
                "Photo {} / {}",
                self.current_index + 1,
                self.photos.len()
            ));
            if let Some(path) = self.current_photo() {
                ui.label(
                    egui::RichText::new(path.file_name().unwrap_or_default().to_string_lossy())
                        .small(),
                );
            }
        }

        ui.add_space(18.0);
        ui.separator();
        let can_undo = !self.history.is_empty();
        ui.add_enabled_ui(can_undo, |ui| {
            if ui.button("Undo last move (Ctrl+Z)").clicked() {
                self.undo();
            }
        });

        ui.add_space(10.0);
        let zoom_label = match self.zoom {
            ZoomMode::Fit => "Zoom: fit  (Space for 1:1)".to_string(),
            ZoomMode::Manual(scale) => format!("Zoom: {:.0}%  (Space to fit)", scale * 100.0),
        };
        if ui.button(zoom_label).clicked() {
            self.toggle_zoom();
        }

        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(
                "Left / Right arrows: navigate\n\
                 Bound key: sort\n\
                 Wheel: zoom, drag: pan\n\
                 Space: fit <-> 1:1",
            )
            .small()
            .italics()
            .weak(),
        );
    }

    // ── Photo view ──────────────────────────────────────────────────────────

    fn photo_panel(&mut self, ui: &mut egui::Ui) {
        if self.photos.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("Open a source folder to get started.");
            });
            return;
        }

        // Claim the whole area as one draggable surface, so panning works
        // anywhere over the photo.
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::drag());

        // The preview is what tells us how big the real photo is, so nothing
        // can be framed until it has arrived.
        let Some(preview) = self.current_preview() else {
            ui.put(rect, egui::Spinner::new());
            return;
        };
        let image = preview.original_size;
        let preview_id = preview.texture.id();
        let preview_scale = preview.texture.size_vec2().x / image.x;

        // ── Input ───────────────────────────────────────────────────────────
        // Plain wheel arrives as a scroll delta; Ctrl+wheel and pinch arrive
        // as `zoom_delta` (egui then zeroes the scroll), so they never add up.
        let (scroll_y, zoom_delta, pointer) = ui.ctx().input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.zoom_delta(),
                i.pointer.hover_pos(),
            )
        });
        let factor = zoom_delta * PhotoSorter::wheel_zoom_factor(scroll_y);

        if response.hovered() && (factor - 1.0).abs() > 1e-4 {
            // Anchor on the pointer, falling back to the centre if egui has no
            // position for it.
            let anchor = pointer.map(|p| p - rect.center()).unwrap_or(Vec2::ZERO);
            self.zoom_by(factor, anchor, rect.size(), image);
        }

        let scale = self.effective_scale(rect.size(), image);

        if response.dragged() {
            self.pan -= response.drag_delta() / scale;
        }
        self.clamp_pan(rect.size(), image, scale);
        self.request_full_if_needed(scale);

        // ── Draw ────────────────────────────────────────────────────────────
        // Use the full-resolution texture only once the preview would be
        // enlarged. Until it arrives, the preview is drawn with the same
        // framing and sharpens in place a moment later.
        let sharp = scale <= preview_scale * 1.001;
        let full_id = self.current_full_texture().map(|texture| texture.id());
        let (texture_id, showing_full) = match full_id {
            Some(id) if !sharp => (id, true),
            _ => (preview_id, false),
        };

        let (screen_size, uv_min, uv_max) = visible_region(self.pan, rect.size(), image, scale);
        let uv = Rect::from_min_max(uv_min.to_pos2(), uv_max.to_pos2());
        let target = Rect::from_center_size(rect.center(), screen_size);
        ui.painter().image(texture_id, target, uv, Color32::WHITE);

        // ── Overlay ─────────────────────────────────────────────────────────
        if self.zoom != ZoomMode::Fit {
            let mut caption = format!(
                "{:.0}%   {} x {} px",
                scale * 100.0,
                image.x as u32,
                image.y as u32
            );
            if !sharp && !showing_full {
                caption.push_str("   (loading full resolution...)");
                ui.ctx().request_repaint();
            }
            overlay(ui, rect, &caption);

            let can_pan = screen_size.x > rect.width() - 1.0 || screen_size.y > rect.height() - 1.0;
            if can_pan {
                let cursor = if response.dragged() {
                    egui::CursorIcon::Grabbing
                } else {
                    egui::CursorIcon::Grab
                };
                response.on_hover_cursor(cursor);
            }
        }
    }
}

/// A small caption in the top-left corner of the photo area.
fn overlay(ui: &egui::Ui, rect: Rect, text: &str) {
    let pos = rect.left_top() + Vec2::new(10.0, 8.0);
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(13.0),
        Color32::WHITE,
    );
    let background = Rect::from_min_size(pos, galley.size()).expand(5.0);
    ui.painter()
        .rect_filled(background, 4.0, Color32::from_black_alpha(160));
    ui.painter().galley(pos, galley, Color32::WHITE);
}
