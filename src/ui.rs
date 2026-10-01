//! egui rendering. Draws and forwards input to `PhotoSorter`; no sorting
//! logic here.

use crate::app::{grid_columns, offset_to_reveal, visible_region, PhotoSorter, View, ZoomMode};
use eframe::{egui, App};
use egui::{Color32, Key, Rect, Sense, Stroke, StrokeKind, Vec2};
use std::path::PathBuf;

const GREEN: Color32 = Color32::from_rgb(120, 200, 120);
const REVEAL_HINT: &str = "Show in the file explorer";

impl App for PhotoSorter {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.config);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.collect_loaded(&ctx);

        // Back from the file explorer: pick up what changed there.
        let focused = ctx.input(|i| i.focused);
        if focused && !self.window_focused {
            self.refresh_on_focus();
        }
        self.window_focused = focused;

        // The "press a key" mode takes over the whole window and short-circuits
        // the shortcuts, otherwise the chosen key would also trigger a move.
        if self.listening_for_key.is_some() {
            self.key_capture_screen(ui, &ctx);
            return;
        }

        self.handle_shortcuts(&ctx);
        if self.view == View::Single {
            self.request_nearby();
        }

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

        egui::CentralPanel::default_margins().show(ui, |ui| match self.view {
            View::Single => self.photo_panel(ui),
            View::Grid => self.grid_panel(ui),
        });
    }
}

impl PhotoSorter {
    // ── Keyboard ────────────────────────────────────────────────────────────

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        // Typing a folder name must not sort photos.
        if ctx.text_edit_focused() {
            return;
        }

        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::N)) {
            self.focus_new_folder = true;
        }

        // Before the plain arrows below, which would fire as well.
        if ctx.input(|i| i.modifiers.alt && i.key_pressed(Key::ArrowRight)) {
            self.open_sibling(1);
            return;
        }
        if ctx.input(|i| i.modifiers.alt && i.key_pressed(Key::ArrowLeft)) {
            self.open_sibling(-1);
            return;
        }

        if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::Z)) {
            self.undo();
            return;
        }

        if ctx.input(|i| i.key_pressed(Key::G) && !i.modifiers.command) {
            self.toggle_view();
        }

        let space = ctx.input(|i| i.key_pressed(Key::Space));
        let escape = ctx.input(|i| i.key_pressed(Key::Escape));
        match self.view {
            View::Single => {
                if space {
                    self.toggle_zoom();
                }
                if escape && self.zoom != ZoomMode::Fit {
                    self.reset_zoom();
                }
            }
            View::Grid => {
                if space {
                    self.toggle_current_selected();
                }
                if escape {
                    self.clear_selection();
                }
                if ctx.input(|i| i.modifiers.command && i.key_pressed(Key::A)) {
                    self.select_all();
                }
            }
        }

        if !self.photos.is_empty() {
            for (folder_idx, key) in self.config.shortcuts() {
                if ctx.input(|i| i.key_pressed(key) && !i.modifiers.command) {
                    self.sort_into(folder_idx);
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
        let mut reveal: Option<PathBuf> = None;

        for (i, folder) in self.config.target_folders.iter().enumerate() {
            ui.horizontal(|ui| {
                // A key reserved since it was bound is inactive: show it as unbound.
                let shortcut = folder
                    .shortcut
                    .filter(|key| !crate::config::is_reserved(*key));
                let (label, color) = match shortcut {
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

                if ui.small_button("🗁").on_hover_text(REVEAL_HINT).clicked() {
                    reveal = Some(folder.path.clone());
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
            self.sort_into(i);
        }
        if let Some(path) = reveal {
            self.reveal_folder(&path);
        }

        ui.add_space(4.0);
        if ui.button("+ Add target folder").clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                self.add_target_folder(path);
            }
        }

        // Quicker than the dialog: type a name, Enter, then press its key.
        let has_source = self.config.source_folder.is_some();
        ui.add_enabled_ui(has_source, |ui| {
            ui.horizontal(|ui| {
                let field = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.new_folder_name)
                            .hint_text("New folder (Ctrl+N)")
                            .desired_width(ui.available_width() - 60.0),
                    )
                    .on_hover_text("Created inside the source folder");
                if std::mem::take(&mut self.focus_new_folder) {
                    field.request_focus();
                }
                let entered = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                let create = ui.button("Create").clicked() || entered;
                if create && !self.new_folder_name.trim().is_empty() {
                    self.create_target_folder();
                }
            });
        });

        ui.add_space(18.0);
        ui.heading("Source");
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("Open source folder").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.load_photos_from(path);
                }
            }
            if let Some(folder) = self.config.source_folder.clone() {
                if ui.small_button("🗁").on_hover_text(REVEAL_HINT).clicked() {
                    self.reveal_folder(&folder);
                }
                if ui
                    .small_button("⟳")
                    .on_hover_text("Refresh the photos and subfolders")
                    .clicked()
                {
                    self.refresh_source();
                }
            }
        });
        // Breadcrumb: click any parent to jump straight up to it.
        if let Some(folder) = self.config.source_folder.clone() {
            let crumbs = crate::files::breadcrumb(&folder);
            let mut jump: Option<PathBuf> = None;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let last = crumbs.len().saturating_sub(1);
                for (i, (name, path)) in crumbs.iter().enumerate() {
                    if i == last {
                        // The current folder: nothing to jump to.
                        ui.label(egui::RichText::new(name).small().strong());
                        break;
                    }
                    let crumb = egui::Button::new(egui::RichText::new(name).small()).frame(false);
                    if ui.add(crumb).on_hover_text(path.display().to_string()).clicked() {
                        jump = Some(path.clone());
                    }
                    ui.label(egui::RichText::new("›").small().weak());
                }
            });
            if let Some(path) = jump {
                self.load_photos_from(path);
            }
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

        // Quick navigation: up to the parent, or down into a subfolder. `+`
        // makes a subfolder a target instead.
        let mut open: Option<PathBuf> = None;
        let mut add_target: Option<PathBuf> = None;
        let parent = self.config.source_folder.as_ref().and_then(|f| f.parent());
        if let Some(parent) = parent {
            let up = egui::Button::new("⬆  ..").frame(false);
            if ui.add(up).on_hover_text(parent.display().to_string()).clicked() {
                open = Some(parent.to_path_buf());
            }
        }
        egui::ScrollArea::vertical()
            .id_salt("subfolders")
            .max_height(160.0)
            .show(ui, |ui| {
                for subfolder in &self.subfolders {
                    let folder = &subfolder.path;
                    ui.horizontal(|ui| {
                        if self.is_target(folder) {
                            ui.add_enabled(false, egui::Button::new("✔").small())
                                .on_disabled_hover_text("Already a target folder");
                        } else if ui
                            .small_button("+")
                            .on_hover_text("Add as a target folder")
                            .clicked()
                        {
                            add_target = Some(folder.clone());
                        }

                        let name = folder.file_name().unwrap_or_default().to_string_lossy();
                        let text = format!("🗀  {name}  ({})", subfolder.photos);
                        let button = egui::Button::new(text).frame(false);
                        if ui.add(button).clicked() {
                            open = Some(folder.clone());
                        }
                    });
                }
            });
        if let Some(folder) = add_target {
            self.add_target_and_bind(folder);
        }
        if let Some(folder) = open {
            self.load_photos_from(folder);
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
        let view_label = match self.view {
            View::Single => "Contact sheet (G)",
            View::Grid => "Single photo (G)",
        };
        if ui.button(view_label).clicked() {
            self.toggle_view();
        }

        let help = match self.view {
            View::Single => {
                let zoom_label = match self.zoom {
                    ZoomMode::Fit => "Zoom: fit  (Space for 1:1)".to_string(),
                    ZoomMode::Manual(scale) => {
                        format!("Zoom: {:.0}%  (Space to fit)", scale * 100.0)
                    }
                };
                if ui.button(zoom_label).clicked() {
                    self.toggle_zoom();
                }
                "Left / Right arrows: navigate\n\
                 Alt+Left / Right: other folder\n\
                 Bound key: sort\n\
                 Wheel: zoom, drag: pan\n\
                 Space: fit <-> 1:1"
            }
            View::Grid => {
                ui.label(format!("{} selected", self.selection.len()));
                "Click, Ctrl+click, Shift+click: select\n\
                 Space: select current, Esc: clear\n\
                 Ctrl+A: select all\n\
                 Bound key: sort the selection\n\
                 Double-click: open\n\
                 Alt+Left / Right: other folder"
            }
        };

        ui.add_space(6.0);
        ui.label(egui::RichText::new(help).small().italics().weak());
    }

    // ── Photo view ──────────────────────────────────────────────────────────

    /// Shown instead of the photos when there are none. Once a folder is
    /// done, offers the next one.
    fn empty_panel(&mut self, ui: &mut egui::Ui) {
        if self.config.source_folder.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label("Open a source folder to get started.");
            });
            return;
        }
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 3.0);
            ui.label("No photos in this folder.");
            if let Some(next) = self.next_folder.clone() {
                ui.add_space(8.0);
                let name = next.file_name().unwrap_or_default().to_string_lossy();
                let label = format!("Next folder: {name}  (Alt+Right)");
                if ui.button(label).on_hover_text(next.display().to_string()).clicked() {
                    self.open_sibling(1);
                }
            }
        });
    }

    fn photo_panel(&mut self, ui: &mut egui::Ui) {
        if self.photos.is_empty() {
            self.empty_panel(ui);
            return;
        }

        // Claim the whole area as one draggable surface, so panning works
        // anywhere over the photo.
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::drag());

        // The preview is what tells us how big the real photo is, so nothing
        // can be framed until it has arrived.
        let Some(preview) = self.current_preview() else {
            let unreadable = self
                .current_photo()
                .is_some_and(|p| self.unreadable.contains(p));
            if unreadable {
                ui.put(rect, egui::Label::new("Unreadable image"));
            } else {
                ui.put(rect, egui::Spinner::new());
            }
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

impl PhotoSorter {
    // ── Contact sheet ───────────────────────────────────────────────────────

    fn grid_panel(&mut self, ui: &mut egui::Ui) {
        if self.photos.is_empty() {
            self.empty_panel(ui);
            return;
        }

        let columns = grid_columns(ui.available_width());
        let cell = ui.available_width() / columns as f32;
        let rows = self.photos.len().div_ceil(columns);

        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if std::mem::take(&mut self.scroll_to_current) {
            let (offset, height) = self.grid_viewport;
            if let Some(y) = offset_to_reveal(self.current_index / columns, cell, offset, height) {
                area = area.vertical_scroll_offset(y);
            }
        }

        let mut clicked = None;
        let mut double_clicked = None;
        let output = area.show_viewport(ui, |ui, viewport| {
            ui.set_height(rows as f32 * cell);
            // Only the visible rows are laid out and drawn, however long the
            // list is.
            let first_row = (viewport.min.y / cell).floor().max(0.0) as usize;
            let last_row = ((viewport.max.y / cell).ceil() as usize).min(rows);
            let visible = first_row * columns..(last_row * columns).min(self.photos.len());
            let origin = ui.max_rect().min;

            for index in visible.clone() {
                let (row, col) = (index / columns, index % columns);
                let top_left = origin + Vec2::new(col as f32 * cell, row as f32 * cell);
                let rect = Rect::from_min_size(top_left, Vec2::splat(cell)).shrink(4.0);
                let response = ui.interact(rect, ui.id().with(("thumb", index)), Sense::click());
                self.paint_thumbnail(ui, rect, index);

                if response.double_clicked() {
                    double_clicked = Some(index);
                } else if response.clicked() {
                    clicked = Some(index);
                }
                if let Some(name) = self.photos[index].file_name() {
                    response.on_hover_text(name.to_string_lossy());
                }
            }
            visible
        });
        self.grid_viewport = (output.state.offset.y, output.inner_rect.height());

        // Visible thumbnails first, then one screen further down, so plain
        // scrolling finds them ready.
        let visible = output.inner;
        self.request_thumbnails(visible.start..visible.end + visible.len());

        if let Some(index) = double_clicked {
            self.open_in_single_view(index);
        } else if let Some(index) = clicked {
            let modifiers = ui.input(|i| i.modifiers);
            self.click_photo(index, modifiers.command, modifiers.shift);
        }
    }

    fn paint_thumbnail(&self, ui: &egui::Ui, rect: Rect, index: usize) {
        let painter = ui.painter();
        let visuals = ui.visuals();
        let path = &self.photos[index];

        painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
        match self.thumbnails.get(path) {
            Some(texture) => {
                // Letterboxed: the whole photo, centred in its square cell.
                let size = texture.size_vec2();
                let scale = (rect.width() / size.x).min(rect.height() / size.y);
                let image_rect = Rect::from_center_size(rect.center(), size * scale);
                let full_uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                painter.image(texture.id(), image_rect, full_uv, Color32::WHITE);
            }
            None => {
                let text = if self.unreadable.contains(path) {
                    "unreadable"
                } else {
                    "..."
                };
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(12.0),
                    visuals.weak_text_color(),
                );
            }
        }

        if self.selection.contains(path) {
            let stroke = Stroke::new(3.0, visuals.selection.bg_fill);
            painter.rect_stroke(rect, 4.0, stroke, StrokeKind::Inside);
        }
        if index == self.current_index {
            let stroke = Stroke::new(1.5, visuals.strong_text_color());
            painter.rect_stroke(rect.expand(2.0), 5.0, stroke, StrokeKind::Outside);
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
