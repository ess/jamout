use std::sync::mpsc::Sender;
use eframe::egui;
use jamseq::ipc::{IpcCommand, IpcStatusResponse};
use jamseq::timeline::{SongSection, TempoOverride, TimeSignature, TrackBlock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPianoRollTarget {
    pub track_id: String,
    pub block_id: String,
    pub section_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NoteMoveDragState {
    pub note_on_id: u64,
    pub note_off_id: Option<u64>,
    pub orig_tick: u64,
    pub orig_pitch: u8,
    pub duration: u64,
    pub grab_offset_ticks: i64,
    pub velocity: f32,
    pub current_tick: u64,
    pub current_pitch: u8,
}

#[derive(Debug)]
pub struct PianoRollState {
    pub note_pitch: u8,
    pub scroll_y: Option<f32>,
    pub zoom_x: f32,
    pub note_duration_ticks: u64,
    pub drag_start: Option<(u64, u8)>,
    pub note_move_drag: Option<NoteMoveDragState>,
    pub resizing_block: Option<u64>,
    pub freehand_drift: bool,
    pub drag_bias_ticks: i64,
}

impl Default for PianoRollState {
    fn default() -> Self {
        Self {
            note_pitch: 60,
            scroll_y: None,
            zoom_x: 1.0,
            note_duration_ticks: 240,
            drag_start: None,
            note_move_drag: None,
            resizing_block: None,
            freehand_drift: false,
            drag_bias_ticks: 0,
        }
    }
}

pub enum PianoRollAction {
    OpenMeterDialog {
        section_id: Option<String>,
        track_id: Option<String>,
    },
    OpenTempoDialog {
        section_id: String,
        track_id: Option<String>,
    },
    StatusMsg(String),
}

impl PianoRollState {
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        target: &OpenPianoRollTarget,
        status: Option<&IpcStatusResponse>,
        cmd_sender: &Sender<IpcCommand>,
        mut on_action: impl FnMut(PianoRollAction),
    ) {
        let status = match status {
            Some(s) => s,
            None => {
                ui.label("Connecting to audio engine...");
                return;
            }
        };

        let track_id = &target.track_id;
        let block_id = &target.block_id;

        let track = match status.tracks.iter().find(|t| &t.id == track_id) {
            Some(t) => t.clone(),
            None => {
                ui.label("Track no longer exists.");
                return;
            }
        };

        let maybe_sec = target.section_id.as_ref().and_then(|sec_id| {
            status.sections.iter().find(|s| &s.id == sec_id).cloned()
        });

        let block = if let Some(ref sec) = maybe_sec {
            match sec.track_blocks.get(track_id).cloned() {
                Some(b) => b,
                None => {
                    ui.label("Track block no longer exists.");
                    return;
                }
            }
        } else {
            match status.timeline_blocks.iter().find(|b| &b.id == block_id && &b.track_id == track_id).cloned() {
                Some(b) => b,
                None => {
                    ui.label("Timeline clip no longer exists.");
                    return;
                }
            }
        };

        let project_ts = status.time_signature;
        let project_bpm = status.bpm;
        let sec_ts = maybe_sec.as_ref().and_then(|s| s.time_signature).unwrap_or(project_ts);
        let sec_bpm = maybe_sec.as_ref().map(|s| s.effective_bpm(project_bpm)).unwrap_or(project_bpm);
        let eff_block_ts = block.effective_time_signature(sec_ts);
        let is_polymeter = block.time_signature.is_some();
        let eff_block_bpm = block.effective_bpm(sec_bpm);

        // --- ROW 1: BLOCK PROPERTIES TOOLBAR ---
        ui.horizontal(|ui| {
            if let Some(ref sec) = maybe_sec {
                ui.strong("Block:");
                ui.label(egui::RichText::new(&block.id).strong().color(egui::Color32::from_rgb(255, 215, 0)));
                ui.small(format!("(Section: {}, Track: {})", sec.name, track.name));

                ui.separator();

                // Meter / Polymeter Button
                let meter_text = if is_polymeter {
                    format!("⚡ {}/{} [Polymeter]", eff_block_ts.numerator, eff_block_ts.denominator)
                } else {
                    format!("📏 {}/{}", eff_block_ts.numerator, eff_block_ts.denominator)
                };
                let meter_color = if is_polymeter {
                    egui::Color32::from_rgb(140, 50, 160)
                } else {
                    egui::Color32::from_rgb(45, 55, 70)
                };
                if ui.add(egui::Button::new(egui::RichText::new(meter_text).small()).fill(meter_color))
                    .on_hover_text("Configure block meter or independent polymeter")
                    .clicked()
                {
                    on_action(PianoRollAction::OpenMeterDialog {
                        section_id: Some(sec.id.clone()),
                        track_id: Some(track_id.clone()),
                    });
                }

                // Tempo / Polytempo Button
                let tempo_text = match block.tempo {
                    Some(TempoOverride::AbsoluteBpm(b)) => format!("⏱ {:.1} BPM", b),
                    Some(TempoOverride::Ratio(n, d)) => format!("⏱ {}:{} ({:.1} BPM)", n, d, eff_block_bpm),
                    None => format!("⏱ {:.1} BPM", eff_block_bpm),
                };
                let tempo_color = match block.tempo {
                    Some(TempoOverride::Ratio(_, _)) => egui::Color32::from_rgb(180, 100, 30),
                    Some(TempoOverride::AbsoluteBpm(_)) => egui::Color32::from_rgb(30, 90, 160),
                    None => egui::Color32::from_rgb(45, 55, 70),
                };
                if ui.add(egui::Button::new(egui::RichText::new(tempo_text).small()).fill(tempo_color))
                    .on_hover_text("Configure block tempo or metric modulation ratio")
                    .clicked()
                {
                    on_action(PianoRollAction::OpenTempoDialog {
                        section_id: sec.id.clone(),
                        track_id: Some(track_id.clone()),
                    });
                }
            } else {
                ui.strong("Clip:");
                ui.label(egui::RichText::new(&block.id).strong().color(egui::Color32::from_rgb(255, 215, 0)));
                ui.small(format!("(Track: {})", track.name));

                ui.separator();

                ui.label("Start Tick:");
                let mut edit_start = block.start_offset_ticks;
                if ui.add(egui::DragValue::new(&mut edit_start).speed(60.0)).changed() {
                    let mut updated = block.clone();
                    updated.start_offset_ticks = edit_start;
                    let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
                }
            }

            ui.separator();

            // Length in Bars & Beats
            let tpb = eff_block_ts.ticks_per_bar().max(1);
            let tp_beat = eff_block_ts.ticks_per_beat().max(1);
            let current_bars = (block.length_ticks / tpb) as u32;
            let current_rem_ticks = block.length_ticks % tpb;
            let current_beats = (current_rem_ticks / tp_beat) as u32;

            ui.label("Length:");
            ui.label("Bars:");
            let mut edit_bars = current_bars;
            if ui.add(egui::DragValue::new(&mut edit_bars).range(0..=128)).changed() {
                let new_len = (edit_bars as u64 * tpb + current_beats as u64 * tp_beat).max(tp_beat);
                let mut updated = block.clone();
                updated.length_ticks = new_len;
                let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
            }

            ui.label("Beats:");
            let mut edit_beats = current_beats;
            let max_beats = (eff_block_ts.numerator.saturating_sub(1)).max(1);
            if ui.add(egui::DragValue::new(&mut edit_beats).range(0..=max_beats)).changed() {
                let new_len = (current_bars as u64 * tpb + edit_beats as u64 * tp_beat).max(tp_beat);
                let mut updated = block.clone();
                updated.length_ticks = new_len;
                let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
            }

            if ui.small_button("-1 Beat").on_hover_text("Shorten block by 1 beat").clicked() {
                let new_len = block.length_ticks.saturating_sub(tp_beat).max(tp_beat);
                let mut updated = block.clone();
                updated.length_ticks = new_len;
                let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
            }
            if ui.small_button("+1 Beat").on_hover_text("Lengthen block by 1 beat").clicked() {
                let new_len = block.length_ticks + tp_beat;
                let mut updated = block.clone();
                updated.length_ticks = new_len;
                let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
            }
            if ui.small_button("-1 Bar").on_hover_text("Shorten block by 1 bar").clicked() {
                let new_len = block.length_ticks.saturating_sub(tpb).max(tp_beat);
                let mut updated = block.clone();
                updated.length_ticks = new_len;
                let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
            }
            if ui.small_button("+1 Bar").on_hover_text("Lengthen block by 1 bar").clicked() {
                let new_len = block.length_ticks + tpb;
                let mut updated = block.clone();
                updated.length_ticks = new_len;
                let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
            }

            let frac_bars = block.length_ticks as f64 / tpb as f64;
            ui.small(egui::RichText::new(format!("({:.2} bars)", frac_bars)).color(egui::Color32::from_rgb(180, 210, 255)));
        });

        ui.add_space(3.0);

        // --- ROW 2: PIANO ROLL TOOLS ---
        let row_height = 14.0f32;
        let min_note = 24u8;  // C1
        let max_note = 84u8;  // C6
        let num_notes = (max_note - min_note + 1) as usize;

        ui.horizontal(|ui| {
            let octave = (self.note_pitch / 12) as i32 - 1;
            let note_idx = (self.note_pitch % 12) as usize;
            let note_names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
            ui.strong("Pitch:");
            ui.label(egui::RichText::new(format!("{}{}", note_names[note_idx], octave)).strong().color(egui::Color32::from_rgb(255, 230, 100)));

            ui.separator();
            ui.label("Jump:");
            if ui.small_button("C1").clicked() { self.scroll_y = Some(((max_note - 24) as f32 * row_height - 180.0).max(0.0)); }
            if ui.small_button("C2").clicked() { self.scroll_y = Some(((max_note - 36) as f32 * row_height - 180.0).max(0.0)); }
            if ui.small_button("C3").clicked() { self.scroll_y = Some(((max_note - 48) as f32 * row_height - 180.0).max(0.0)); }
            if ui.small_button("C4").clicked() { self.scroll_y = Some(((max_note - 60) as f32 * row_height - 180.0).max(0.0)); }
            if ui.small_button("C5").clicked() { self.scroll_y = Some(((max_note - 72) as f32 * row_height - 180.0).max(0.0)); }
            if ui.small_button("C6").clicked() { self.scroll_y = Some(((max_note - 84) as f32 * row_height - 180.0).max(0.0)); }

            if ui.small_button("Center Notes").clicked() {
                let note_pitches: Vec<u8> = block.events.iter().filter_map(|e| {
                    if let jamseq::timeline::ScheduledAction::NoteOn { note, .. } = e.action {
                        Some(note)
                    } else {
                        None
                    }
                }).collect();
                if !note_pitches.is_empty() {
                    let avg = note_pitches.iter().map(|&p| p as f32).sum::<f32>() / note_pitches.len() as f32;
                    self.scroll_y = Some(((max_note as f32 - avg) * row_height - 180.0).max(0.0));
                } else {
                    self.scroll_y = Some(((max_note - 60) as f32 * row_height - 180.0).max(0.0));
                }
            }

            ui.separator();
            ui.label("Zoom:");
            if ui.small_button("➖").on_hover_text("Zoom Out horizontally (Ctrl+Scroll)").clicked() {
                self.zoom_x = (self.zoom_x / 1.25).max(0.25);
            }
            let zoom_pct = (self.zoom_x * 100.0).round() as i32;
            if ui.selectable_label(self.zoom_x == 1.0, format!("{}%", zoom_pct)).on_hover_text("Reset Zoom to 100%").clicked() {
                self.zoom_x = 1.0;
            }
            if ui.small_button("➕").on_hover_text("Zoom In horizontally (Ctrl+Scroll)").clicked() {
                self.zoom_x = (self.zoom_x * 1.25).min(8.0);
            }

            ui.separator();
            ui.label("Snap Grid:");
            let snap_labels = [("1/4", 960), ("1/8", 480), ("1/16", 240), ("1/32", 120), ("1/64", 60)];
            for (label, ticks) in snap_labels {
                if ui.selectable_label(self.note_duration_ticks == ticks, label).clicked() {
                    self.note_duration_ticks = ticks;
                }
            }
            ui.checkbox(&mut self.freehand_drift, "Freehand").on_hover_text("Unquantized note placement (or hold Shift while drawing)");
        });

        ui.separator();

        // Handle Ctrl + MouseWheel for horizontal zoom
        let (ctrl_held, raw_scroll_delta) = ui.input(|i| (i.modifiers.command || i.modifiers.ctrl, i.raw_scroll_delta));
        if ctrl_held && raw_scroll_delta.y != 0.0 {
            let zoom_factor = 1.15f32;
            let zoom_delta = if raw_scroll_delta.y > 0.0 { zoom_factor } else { 1.0 / zoom_factor };
            self.zoom_x = (self.zoom_x * zoom_delta).clamp(0.25, 8.0);
        }

        let total_ticks = block.length_ticks;
        let view_ticks = total_ticks.max(3840);
        let avail_w = ui.available_width() - 52.0;
        let key_width = 44.0;
        let piano_height = (num_notes as f32 * row_height).max(280.0);

        let scroll = egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .id_salt(format!("piano_roll_v_scroll_{}_{}", block_id, track_id));

        if let Some(offset_y) = self.scroll_y.take() {
            scroll.vertical_scroll_offset(offset_y).show(ui, |ui| {
                self.draw_grid(ui, &block, &track, key_width, row_height, min_note, max_note, num_notes, piano_height, avail_w, total_ticks, view_ticks, eff_block_ts, maybe_sec.as_ref(), status, cmd_sender, &mut on_action);
            });
        } else {
            scroll.show(ui, |ui| {
                self.draw_grid(ui, &block, &track, key_width, row_height, min_note, max_note, num_notes, piano_height, avail_w, total_ticks, view_ticks, eff_block_ts, maybe_sec.as_ref(), status, cmd_sender, &mut on_action);
            });
        }
    }

    fn draw_grid(
        &mut self,
        ui: &mut egui::Ui,
        block: &TrackBlock,
        track: &jamseq::ipc::TrackStatus,
        key_width: f32,
        row_height: f32,
        min_note: u8,
        max_note: u8,
        num_notes: usize,
        piano_height: f32,
        avail_w: f32,
        total_ticks: u64,
        view_ticks: u64,
        eff_block_ts: TimeSignature,
        maybe_sec: Option<&SongSection>,
        status: &IpcStatusResponse,
        cmd_sender: &Sender<IpcCommand>,
        on_action: &mut impl FnMut(PianoRollAction),
    ) {
        ui.horizontal_top(|ui| {
            // Left piano keys
            let (keys_rect, keys_resp) = ui.allocate_exact_size(
                egui::vec2(key_width, piano_height),
                egui::Sense::click_and_drag(),
            );
            let painter = ui.painter();

            for i in 0..num_notes {
                let pitch = max_note - i as u8;
                let y = keys_rect.top() + i as f32 * row_height;
                let r = egui::Rect::from_min_size(egui::pos2(keys_rect.left(), y), egui::vec2(key_width, row_height));
                let is_black = [1, 3, 6, 8, 10].contains(&(pitch % 12));
                let is_c = pitch % 12 == 0;

                if keys_resp.clicked() || keys_resp.dragged() {
                    if let Some(pos) = keys_resp.interact_pointer_pos() {
                        if r.contains(pos) {
                            self.note_pitch = pitch;
                        }
                    }
                }

                let is_active_pitch = pitch == self.note_pitch;
                let fill = if is_active_pitch {
                    egui::Color32::from_rgb(255, 215, 0)
                } else if is_black {
                    egui::Color32::from_rgb(25, 25, 30)
                } else {
                    egui::Color32::from_rgb(230, 230, 235)
                };

                painter.rect_filled(r, 1.0, fill);
                painter.rect_stroke(r, 0.0, egui::Stroke::new(0.5, egui::Color32::from_rgb(70, 70, 80)));

                if is_c {
                    let octave = (pitch / 12) as i32 - 1;
                    painter.text(
                        egui::pos2(r.left() + 4.0, r.center().y),
                        egui::Align2::LEFT_CENTER,
                        format!("C{}", octave),
                        egui::FontId::proportional(10.0),
                        if is_active_pitch { egui::Color32::BLACK } else { egui::Color32::from_rgb(40, 40, 50) },
                    );
                }
            }

            // Right scrollable note grid
            egui::ScrollArea::horizontal()
                .auto_shrink([false; 2])
                .id_salt(format!("piano_roll_h_scroll_{}_{}", block.id, track.id))
                .show(ui, |ui| {
                    let grid_width = (avail_w * self.zoom_x).max(200.0);
                    let (grid_rect, grid_resp) = ui.allocate_exact_size(
                        egui::vec2(grid_width, piano_height),
                        egui::Sense::click_and_drag(),
                    );

                    let tp_beat = eff_block_ts.ticks_per_beat().max(1);
                    let end_x = grid_rect.left() + (block.length_ticks as f32 / view_ticks as f32).clamp(0.0, 1.0) * grid_rect.width();

                    let badge_rect = egui::Rect::from_center_size(
                        egui::pos2(end_x, grid_rect.top() + 10.0),
                        egui::vec2(52.0, 16.0),
                    );
                    let is_hovering_badge = ui.rect_contains_pointer(badge_rect);
                    let is_resizing_block_now = self.resizing_block.is_some();

                    if is_hovering_badge || is_resizing_block_now {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }

                    if grid_resp.drag_started_by(egui::PointerButton::Primary) {
                        if let Some(pos) = grid_resp.interact_pointer_pos() {
                            if badge_rect.contains(pos) || (pos.x >= end_x - 6.0 && pos.x <= end_x + 6.0) {
                                self.resizing_block = Some(block.length_ticks);
                                self.drag_start = None;
                                self.note_move_drag = None;
                            } else {
                                self.resizing_block = None;
                                let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.0, 1.0);
                                let clicked_tick = (pct_x * total_ticks as f32) as u64;
                                let snap_grid = self.note_duration_ticks.max(10);
                                let shift_held = ui.input(|i| i.modifiers.shift);
                                let final_tick = if shift_held || self.freehand_drift {
                                    (clicked_tick as i64 + self.drag_bias_ticks).max(0) as u64
                                } else {
                                    (clicked_tick / snap_grid) * snap_grid
                                };

                                let row_idx = ((pos.y - grid_rect.top()) / row_height).floor() as u8;
                                let clicked_pitch = max_note.saturating_sub(row_idx);
                                self.note_pitch = clicked_pitch;

                                // Check if user clicked an existing note to move it
                                let mut hit_note = None;
                                for (i, ev) in block.events.iter().enumerate() {
                                    if let jamseq::timeline::ScheduledAction::NoteOn { note, velocity } = ev.action {
                                        if note == clicked_pitch {
                                            let note_off = block.events.iter().skip(i + 1).find(|off_ev| {
                                                off_ev.tick > ev.tick && match off_ev.action {
                                                    jamseq::timeline::ScheduledAction::NoteOff { note: off_n } => off_n == note,
                                                    _ => false,
                                                }
                                            });
                                            let note_off_tick = note_off.map(|off| off.tick).unwrap_or(ev.tick + self.note_duration_ticks);
                                            let duration = note_off_tick.saturating_sub(ev.tick).max(10);
                                            let end_tick = ev.tick + duration;
                                            if clicked_tick >= ev.tick && clicked_tick < end_tick {
                                                hit_note = Some((ev.id, note_off.map(|off| off.id), ev.tick, note, duration, clicked_tick as i64 - ev.tick as i64, velocity));
                                                break;
                                            }
                                        }
                                    }
                                }

                                if let Some((note_on_id, note_off_id, orig_tick, orig_pitch, duration, grab_offset, velocity)) = hit_note {
                                    self.note_move_drag = Some(NoteMoveDragState {
                                        note_on_id,
                                        note_off_id,
                                        orig_tick,
                                        orig_pitch,
                                        duration,
                                        grab_offset_ticks: grab_offset,
                                        velocity,
                                        current_tick: orig_tick,
                                        current_pitch: orig_pitch,
                                    });
                                    self.drag_start = None;
                                } else {
                                    self.note_move_drag = None;
                                    if final_tick < block.length_ticks && !block.has_note_at(clicked_pitch, clicked_tick) && !block.has_note_at(clicked_pitch, final_tick) {
                                        self.drag_start = Some((final_tick, clicked_pitch));
                                    } else {
                                        self.drag_start = None;
                                    }
                                }
                            }
                        }
                    }

                    // Dragging in progress
                    if grid_resp.dragged() {
                        if let Some(_initial_len) = self.resizing_block {
                            if let Some(pos) = grid_resp.interact_pointer_pos() {
                                let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.02, 1.0);
                                let raw_tick = (pct_x * view_ticks as f32) as u64;
                                let shift_held = ui.input(|i| i.modifiers.shift);
                                let snapped_tick = if shift_held || self.freehand_drift {
                                    raw_tick
                                } else {
                                    ((raw_tick + tp_beat / 2) / tp_beat) * tp_beat
                                };
                                let new_len = snapped_tick.max(tp_beat);
                                let mut updated = block.clone();
                                updated.length_ticks = new_len;
                                let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
                            }
                        } else if let Some(ref mut move_state) = self.note_move_drag {
                            if let Some(pos) = grid_resp.interact_pointer_pos() {
                                let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.0, 1.0);
                                let raw_tick = (pct_x * total_ticks as f32) as u64;
                                let row_idx = ((pos.y - grid_rect.top()) / row_height).floor().clamp(0.0, (num_notes.saturating_sub(1)) as f32) as u8;
                                let new_pitch = max_note.saturating_sub(row_idx);

                                let shift_held = ui.input(|i| i.modifiers.shift);
                                let unadjusted_tick = (raw_tick as i64 - move_state.grab_offset_ticks).max(0) as u64;
                                let snap_grid = self.note_duration_ticks.max(10);
                                let snapped_tick = if shift_held || self.freehand_drift {
                                    unadjusted_tick
                                } else {
                                    (unadjusted_tick / snap_grid) * snap_grid
                                };
                                let max_tick = block.length_ticks.saturating_sub(move_state.duration);
                                let clamped_tick = snapped_tick.min(max_tick);

                                move_state.current_tick = clamped_tick;
                                move_state.current_pitch = new_pitch;
                            }
                        }
                    }

                    // Mouse release
                    if grid_resp.drag_stopped_by(egui::PointerButton::Primary) {
                        if self.resizing_block.is_some() {
                            self.resizing_block = None;
                        } else if let Some(move_state) = self.note_move_drag.take() {
                            if move_state.current_tick != move_state.orig_tick || move_state.current_pitch != move_state.orig_pitch {
                                let mut updated = block.clone();
                                updated.events.retain(|e| {
                                    e.id != move_state.note_on_id && Some(e.id) != move_state.note_off_id
                                });

                                if updated.has_note_at(move_state.current_pitch, move_state.current_tick) {
                                    on_action(PianoRollAction::StatusMsg(format!("Cannot move: pitch {} is already occupied at tick {}", move_state.current_pitch, move_state.current_tick)));
                                } else {
                                    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                                    updated.add_note(
                                        now_ms,
                                        now_ms + 1,
                                        move_state.current_tick,
                                        move_state.duration,
                                        move_state.current_pitch,
                                        move_state.velocity,
                                    );
                                    let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
                                    self.note_pitch = move_state.current_pitch;
                                    on_action(PianoRollAction::StatusMsg(format!("Block '{}': Moved note to pitch {} at tick {}", block.id, move_state.current_pitch, move_state.current_tick)));
                                }
                            }
                        } else if let Some((start_tick, pitch)) = self.drag_start.take() {
                            if !block.has_note_at(pitch, start_tick) {
                                let max_len = if let Some(next_start) = block.next_note_start(pitch, start_tick) {
                                    next_start.saturating_sub(start_tick)
                                } else {
                                    block.length_ticks.saturating_sub(start_tick)
                                };
                                let shift_held = ui.input(|i| i.modifiers.shift);
                                let is_unquantized = shift_held || self.freehand_drift;
                                let min_len = if is_unquantized { 10u64 } else { self.note_duration_ticks };
                                let snap_grid = self.note_duration_ticks.max(10);
                                let half_grid = snap_grid / 2;
                                let final_duration = if let Some(pos) = grid_resp.interact_pointer_pos() {
                                    let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.0, 1.0);
                                    let current_tick = (pct_x * total_ticks as f32) as u64;
                                    if current_tick > start_tick {
                                        let drag_len = if is_unquantized {
                                            (current_tick - start_tick).max(min_len)
                                        } else {
                                            (((current_tick - start_tick + half_grid) / snap_grid) * snap_grid).max(min_len)
                                        };
                                        drag_len.min(max_len)
                                    } else {
                                        self.note_duration_ticks.min(max_len)
                                    }
                                } else {
                                    self.note_duration_ticks.min(max_len)
                                };

                                if final_duration > 0 {
                                    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                                    let mut updated = block.clone();
                                    updated.add_note(now_ms, now_ms + 1, start_tick, final_duration, pitch, 0.9);
                                    let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
                                    on_action(PianoRollAction::StatusMsg(format!("Block '{}': Added note {} at tick {} (length: {} ticks)", block.id, pitch, start_tick, final_duration)));
                                }
                            }
                        }
                    } else if grid_resp.clicked_by(egui::PointerButton::Primary) && self.drag_start.is_none() && self.note_move_drag.is_none() && self.resizing_block.is_none() {
                        if let Some(pos) = grid_resp.interact_pointer_pos() {
                            if !is_hovering_badge {
                                let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.0, 1.0);
                                let clicked_tick = (pct_x * total_ticks as f32) as u64;
                                let shift_held = ui.input(|i| i.modifiers.shift);
                                let snap_grid = self.note_duration_ticks.max(10);
                                let final_tick = if shift_held || self.freehand_drift {
                                    (clicked_tick as i64 + self.drag_bias_ticks).max(0) as u64
                                } else {
                                    (clicked_tick / snap_grid) * snap_grid
                                };

                                let row_idx = ((pos.y - grid_rect.top()) / row_height).floor() as u8;
                                let clicked_pitch = max_note.saturating_sub(row_idx);
                                self.note_pitch = clicked_pitch;

                                if final_tick >= block.length_ticks {
                                    on_action(PianoRollAction::StatusMsg(format!("Cannot place note past block end (tick {})", block.length_ticks)));
                                } else if block.has_note_at(clicked_pitch, clicked_tick) || block.has_note_at(clicked_pitch, final_tick) {
                                    on_action(PianoRollAction::StatusMsg(format!("Block '{}': Note already exists on pitch {} at tick {}", block.id, clicked_pitch, clicked_tick)));
                                } else {
                                    let max_len = if let Some(next_start) = block.next_note_start(clicked_pitch, final_tick) {
                                        next_start.saturating_sub(final_tick)
                                    } else {
                                        block.length_ticks.saturating_sub(final_tick)
                                    };
                                    let final_duration = self.note_duration_ticks.min(max_len);
                                    if final_duration > 0 {
                                        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                                        let mut updated = block.clone();
                                        updated.add_note(now_ms, now_ms + 1, final_tick, final_duration, clicked_pitch, 0.9);
                                        let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
                                        on_action(PianoRollAction::StatusMsg(format!("Block '{}': Added note {} at tick {} (length: {} ticks)", block.id, clicked_pitch, final_tick, final_duration)));
                                    }
                                }
                            }
                        }
                    } else if grid_resp.secondary_clicked() {
                        if let Some(pos) = grid_resp.interact_pointer_pos() {
                            let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.0, 1.0);
                            let clicked_tick = (pct_x * total_ticks as f32) as u64;
                            let row_idx = ((pos.y - grid_rect.top()) / row_height).floor() as u8;
                            let clicked_pitch = max_note.saturating_sub(row_idx);

                            let mut on_idx = None;
                            let mut off_idx = None;

                            for (i, ev) in block.events.iter().enumerate() {
                                if let jamseq::timeline::ScheduledAction::NoteOn { note, .. } = ev.action {
                                    if note == clicked_pitch {
                                        let note_off = block.events.iter().enumerate().skip(i + 1).find(|(_, off_ev)| {
                                            off_ev.tick > ev.tick
                                                && match off_ev.action {
                                                    jamseq::timeline::ScheduledAction::NoteOff { note: off_n } => off_n == note,
                                                    _ => false,
                                                }
                                        });
                                        let note_off_tick = note_off.map(|(_, off)| off.tick).unwrap_or(ev.tick + self.note_duration_ticks);
                                        let duration = note_off_tick.saturating_sub(ev.tick).max(10);
                                        let end_tick = ev.tick + duration;
                                        let tolerance = (duration / 4).clamp(1, 15);
                                        let is_direct_hit = clicked_tick >= ev.tick && clicked_tick < end_tick;
                                        let is_near_hit = clicked_tick >= ev.tick.saturating_sub(tolerance) && clicked_tick <= end_tick + tolerance;
                                        if is_direct_hit || is_near_hit {
                                            on_idx = Some(i);
                                            off_idx = note_off.map(|(off_i, _)| off_i);
                                            break;
                                        }
                                    }
                                }
                            }

                            if on_idx.is_none() {
                                let mut closest_dist = i64::MAX;
                                let max_dist = (self.note_duration_ticks / 2).clamp(10, 60) as i64;
                                for (i, ev) in block.events.iter().enumerate() {
                                    if let jamseq::timeline::ScheduledAction::NoteOn { note, .. } = ev.action {
                                        if note == clicked_pitch {
                                            let note_off = block.events.iter().enumerate().skip(i + 1).find(|(_, off_ev)| {
                                                off_ev.tick > ev.tick
                                                    && match off_ev.action {
                                                    jamseq::timeline::ScheduledAction::NoteOff { note: off_n } => off_n == note,
                                                    _ => false,
                                                }
                                            });
                                            let note_off_tick = note_off.map(|(_, off)| off.tick).unwrap_or(ev.tick + self.note_duration_ticks);
                                            let start_dist = (ev.tick as i64 - clicked_tick as i64).abs();
                                            let end_dist = (note_off_tick as i64 - clicked_tick as i64).abs();
                                            let d = start_dist.min(end_dist);
                                            if d <= max_dist && d < closest_dist {
                                                closest_dist = d;
                                                on_idx = Some(i);
                                                off_idx = note_off.map(|(off_i, _)| off_i);
                                            }
                                        }
                                    }
                                }
                            }

                            if let Some(on_i) = on_idx {
                                let mut updated = block.clone();
                                let mut indices_to_remove = vec![on_i];
                                if let Some(off_i) = off_idx {
                                    indices_to_remove.push(off_i);
                                }
                                indices_to_remove.sort_unstable_by(|a, b| b.cmp(a));
                                for idx in indices_to_remove {
                                    if idx < updated.events.len() {
                                        updated.events.remove(idx);
                                    }
                                }
                                let _ = cmd_sender.send(IpcCommand::UpdateBlock { block: updated });
                                on_action(PianoRollAction::StatusMsg(format!("Block '{}': Deleted note {} at tick {}", block.id, clicked_pitch, clicked_tick)));
                            }
                        }
                    }

                    let grid_painter = ui.painter();
                    for i in 0..num_notes {
                        let pitch = max_note - i as u8;
                        let y = grid_rect.top() + i as f32 * row_height;
                        let r = egui::Rect::from_min_size(egui::pos2(grid_rect.left(), y), egui::vec2(grid_width, row_height));
                        let is_c = pitch % 12 == 0;
                        let is_black = [1, 3, 6, 8, 10].contains(&(pitch % 12));
                        let is_hovered = ui.rect_contains_pointer(r);

                        let row_bg = if is_hovered {
                            if is_black { egui::Color32::from_rgb(36, 40, 52) } else { egui::Color32::from_rgb(46, 52, 66) }
                        } else if is_black {
                            egui::Color32::from_rgb(22, 22, 28)
                        } else {
                            egui::Color32::from_rgb(32, 32, 38)
                        };
                        grid_painter.rect_filled(r, 0.0, row_bg);

                        if is_hovered {
                            grid_painter.rect_stroke(
                                r,
                                0.0,
                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(100, 160, 255, 60)),
                            );
                        }

                        if is_c {
                            grid_painter.line_segment(
                                [egui::pos2(grid_rect.left(), r.bottom()), egui::pos2(grid_rect.right(), r.bottom())],
                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(80, 140, 240, 50)),
                            );
                        }
                    }

                    // Vertical grid lines
                    let ticks_per_bar = eff_block_ts.ticks_per_bar().max(1);
                    let ticks_per_beat = eff_block_ts.ticks_per_beat().max(1);
                    let candidate_step = self.note_duration_ticks.clamp(10, 240);
                    let px_per_tick = grid_rect.width() / total_ticks.max(1) as f32;
                    let grid_step = if candidate_step < 240 && (px_per_tick * candidate_step as f32) >= 6.0 {
                        candidate_step
                    } else if self.note_duration_ticks < 240 && (px_per_tick * 120.0) >= 6.0 {
                        120
                    } else if (px_per_tick * 240.0) >= 4.0 {
                        240
                    } else {
                        ticks_per_beat
                    };

                    let mut t = 0u64;
                    while t <= total_ticks {
                        let pct = (t as f32 / total_ticks.max(1) as f32).clamp(0.0, 1.0);
                        let x = grid_rect.left() + grid_rect.width() * pct;
                        let is_bar = t % ticks_per_bar == 0;
                        let is_beat = t % ticks_per_beat == 0;

                        let stroke = if is_bar {
                            egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120))
                        } else if is_beat {
                            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(200, 200, 200, 60))
                        } else {
                            egui::Stroke::new(0.5, egui::Color32::from_rgba_unmultiplied(150, 150, 150, 25))
                        };

                        grid_painter.line_segment([egui::pos2(x, grid_rect.top()), egui::pos2(x, grid_rect.bottom())], stroke);
                        t += grid_step;
                    }

                    // Draw Note Blocks
                    for (i, ev) in block.events.iter().enumerate() {
                        if let jamseq::timeline::ScheduledAction::NoteOn { note, velocity } = ev.action {
                            if note >= min_note && note <= max_note {
                                let note_off = block.events.iter().skip(i + 1).find(|off_ev| {
                                    off_ev.tick > ev.tick && match off_ev.action {
                                        jamseq::timeline::ScheduledAction::NoteOff { note: off_n } => off_n == note,
                                        _ => false,
                                    }
                                });
                                let note_off_tick = note_off.map(|off| off.tick).unwrap_or(ev.tick + self.note_duration_ticks);
                                let duration = note_off_tick.saturating_sub(ev.tick).max(10);

                                if let Some(ref move_state) = self.note_move_drag {
                                    if move_state.note_on_id == ev.id {
                                        continue;
                                    }
                                }

                                let row = (max_note - note) as f32;
                                let y = grid_rect.top() + row * row_height;
                                let note_pct = (ev.tick as f32 / total_ticks.max(1) as f32).clamp(0.0, 1.0);
                                let note_x = grid_rect.left() + grid_rect.width() * note_pct;
                                let note_w = (grid_rect.width() * (duration as f32 / total_ticks.max(1) as f32)).max(6.0);

                                let note_rect = egui::Rect::from_min_size(
                                    egui::pos2(note_x, y + 1.0),
                                    egui::vec2(note_w, row_height - 2.0),
                                );

                                let vel_norm = (velocity as f32).clamp(0.1, 1.0);
                                let base_r = (240.0 * vel_norm) as u8;
                                let base_g = (160.0 + 80.0 * (1.0 - vel_norm)) as u8;
                                let base_b = 60u8;

                                grid_painter.rect_filled(note_rect, 2.0, egui::Color32::from_rgb(base_r, base_g, base_b));
                                grid_painter.rect_stroke(note_rect, 2.0, egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 235, 140)));

                                if note_w > 18.0 {
                                    let octave = (note / 12) as i32 - 1;
                                    let note_names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
                                    grid_painter.text(
                                        egui::pos2(note_rect.left() + 2.0, note_rect.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        format!("{}{}", note_names[(note % 12) as usize], octave),
                                        egui::FontId::proportional(9.0),
                                        egui::Color32::BLACK,
                                    );
                                }
                            }
                        }
                    }

                    // Draw moving note preview
                    if let Some(ref move_state) = self.note_move_drag {
                        if move_state.current_pitch >= min_note && move_state.current_pitch <= max_note {
                            let row = (max_note - move_state.current_pitch) as f32;
                            let y = grid_rect.top() + row * row_height;
                            let note_pct = (move_state.current_tick as f32 / total_ticks.max(1) as f32).clamp(0.0, 1.0);
                            let note_x = grid_rect.left() + grid_rect.width() * note_pct;
                            let note_w = (grid_rect.width() * (move_state.duration as f32 / total_ticks.max(1) as f32)).max(6.0);

                            let drag_rect = egui::Rect::from_min_size(
                                egui::pos2(note_x, y + 1.0),
                                egui::vec2(note_w, row_height - 2.0),
                            );
                            grid_painter.rect_filled(drag_rect, 2.0, egui::Color32::from_rgba_unmultiplied(100, 255, 100, 150));
                            grid_painter.rect_stroke(drag_rect, 2.0, egui::Stroke::new(1.5, egui::Color32::from_rgb(150, 255, 150)));
                        }
                    }

                    // Keyboard navigation & playback shortcuts
                    if ui.input(|i| i.key_pressed(egui::Key::Space)) {
                        let is_playing = status.is_playing;
                        if is_playing {
                            let _ = cmd_sender.send(IpcCommand::Pause);
                        } else {
                            let _ = cmd_sender.send(IpcCommand::Play);
                        }
                    }

                    // Shaded inactive area beyond block end
                    if end_x < grid_rect.right() {
                        let inactive_rect = egui::Rect::from_min_max(
                            egui::pos2(end_x, grid_rect.top()),
                            egui::pos2(grid_rect.right(), grid_rect.bottom()),
                        );
                        grid_painter.rect_filled(inactive_rect, 0.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 110));
                    }

                    // Playhead within block
                    let current_tick = status.current_tick;
                    let playhead_tick = if let Some(ref sec) = maybe_sec {
                        let sec_pos = status.sections.iter().scan(0u64, |acc, s| {
                            let start = *acc;
                            *acc += s.length_ticks();
                            Some((s.id.clone(), start, s.length_ticks()))
                        }).find(|(id, _, _)| id == &sec.id);

                        if let Some((_, sec_start, _)) = sec_pos {
                            let blk_start = sec_start + block.start_offset_ticks;
                            let blk_end = blk_start + block.effective_length_ticks();
                            if current_tick >= blk_start && current_tick < blk_end && block.length_ticks > 0 {
                                Some(current_tick - blk_start)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        let blk_start = block.start_offset_ticks;
                        let blk_end = blk_start + block.effective_length_ticks();
                        if current_tick >= blk_start && current_tick < blk_end && block.length_ticks > 0 {
                            Some(current_tick - blk_start)
                        } else {
                            None
                        }
                    };

                    if let Some(p_tick) = playhead_tick {
                        if total_ticks > 0 {
                            let playhead_pct = (p_tick as f32 / total_ticks as f32).clamp(0.0, 1.0);
                            let playhead_x = grid_rect.left() + grid_rect.width() * playhead_pct;
                            grid_painter.line_segment(
                                [egui::pos2(playhead_x, grid_rect.top()), egui::pos2(playhead_x, grid_rect.bottom())],
                                egui::Stroke::new(2.5, egui::Color32::WHITE),
                            );
                        }
                    }

                    // End marker line & drag handle
                    let marker_color = if is_resizing_block_now || is_hovering_badge {
                        egui::Color32::from_rgb(255, 90, 90)
                    } else {
                        egui::Color32::from_rgb(210, 60, 60)
                    };
                    grid_painter.line_segment(
                        [egui::pos2(end_x, grid_rect.top()), egui::pos2(end_x, grid_rect.bottom())],
                        egui::Stroke::new(2.0, marker_color),
                    );

                    grid_painter.rect_filled(badge_rect, 3.0, marker_color);
                    grid_painter.text(
                        badge_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "◀ END ▶",
                        egui::FontId::proportional(10.0),
                        egui::Color32::WHITE,
                    );

                    if is_resizing_block_now {
                        let current_bars = block.length_ticks as f64 / tp_beat as f64 / eff_block_ts.numerator as f64;
                        let tooltip_text = format!("Block Length: {} ticks ({:.2} bars)", block.length_ticks, current_bars);
                        egui::show_tooltip_text(ui.ctx(), ui.layer_id(), egui::Id::new("piano_roll_resize_tip"), tooltip_text);
                    }
                });
        });
    }
}
