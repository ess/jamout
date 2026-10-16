use eframe::egui;
use jamseq::ipc::IpcCommand;
use jamseq::timeline::{LoopMode, TempoOverride, TrackArrangerMode, TrackBlock};
use crate::{
    compute_section_col_width, BlockDragKind, BlockDragState, JamOutApp, OpenPianoRollTarget,
    TimelineBlockDragState, SECTION_HEADER_HEIGHT, TRACK_HEADER_WIDTH, TRACK_ROW_HEIGHT,
};

pub fn render_arranger(app: &mut JamOutApp, ui: &mut egui::Ui) {
    let total_ticks = app.last_status.as_ref().map(|s| s.total_ticks).unwrap_or(0);
    let current_tick = app.last_status.as_ref().map(|s| s.current_tick).unwrap_or(0);
    let sections = app.last_status.as_ref().map(|s| s.sections.clone()).unwrap_or_default();
    let tracks = app.last_status.as_ref().map(|s| s.tracks.clone()).unwrap_or_default();
    let project_bpm = app.last_status.as_ref().map(|s| s.bpm).unwrap_or(120.0);
    let project_ts = app.last_status.as_ref().map(|s| s.time_signature).unwrap_or_default();

    // --- 1. TOP HEADER & TRANSPORT TIMELINE ---
    ui.horizontal(|ui| {
        ui.heading("Arrangement Matrix Arranger");
        ui.separator();
        ui.label(egui::RichText::new(format!("Base Tempo: {:.1} BPM", project_bpm)).strong());

        ui.separator();
        ui.label("Playhead:");
        let mut seek_tick = current_tick;
        if ui.add(egui::Slider::new(&mut seek_tick, 0..=total_ticks.max(1)).text("Ticks")).changed() {
            app.send_cmd(IpcCommand::Seek { tick: seek_tick });
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("➕ Add Section").on_hover_text("Define or append a new song section").clicked() {
                app.show_section_dialog = true;
            }
        });
    });

    ui.add_space(6.0);

    // --- 3. MATRIX ARRANGER GRID (Sections Columns x Tracks Rows) ---
    egui::ScrollArea::both().show(ui, |ui| {
        // Header Row: Track Column Header + Each Section Column Header
        ui.horizontal(|ui| {
            // Top-Left Corner Cell (Track Column Header)
            let (tl_rect, _) = ui.allocate_exact_size(
                egui::vec2(TRACK_HEADER_WIDTH, SECTION_HEADER_HEIGHT),
                egui::Sense::hover(),
            );
            ui.painter().rect_filled(
                tl_rect,
                2.0,
                egui::Color32::from_rgb(28, 32, 40),
            );
            let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(tl_rect.shrink(4.0)));
            child_ui.vertical(|ui| {
                ui.strong("TRACKS");
                ui.horizontal(|ui| {
                    if ui.small_button("➕ MIDI").clicked() {
                        let count = tracks.len() + 1;
                        let id = format!("track_{}", count);
                        let name = format!("MIDI Track {}", count);
                        app.send_cmd(IpcCommand::AddTrack { id: id.clone(), name: name.clone(), track_type: "synth".to_string() });
                        app.track_name_edits.insert(id, name);
                    }
                    if ui.small_button("➕ Audio").clicked() {
                        let count = tracks.len() + 1;
                        let id = format!("audio_{}", count);
                        let name = format!("Audio Track {}", count);
                        app.send_cmd(IpcCommand::AddTrack { id: id.clone(), name: name.clone(), track_type: "audio".to_string() });
                        app.track_name_edits.insert(id, name);
                    }
                });
            });

            ui.separator();

            // Section Column Headers
            if sections.is_empty() {
                ui.label("(No sections in project. Click '➕ Add Section' to create your first arrangement section.)");
            }

            let mut accum_ticks = 0u64;
            for sec in sections.iter() {
                let sec_len = sec.length_ticks();
                let sec_start_tick = accum_ticks;
                accum_ticks += sec_len;

                let col_width = compute_section_col_width(sec_len, 1.0);
                let (sec_hdr_rect, _) = ui.allocate_exact_size(
                    egui::vec2(col_width, SECTION_HEADER_HEIGHT),
                    egui::Sense::hover(),
                );

                // Background fill and subtle border for section header card
                ui.painter().rect_filled(
                    sec_hdr_rect,
                    3.0,
                    egui::Color32::from_rgb(38, 44, 56),
                );
                ui.painter().rect_stroke(
                    sec_hdr_rect,
                    3.0,
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(55, 65, 80)),
                );

                let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(sec_hdr_rect.shrink(4.0)));
                child_ui.vertical(|ui| {
                    // Row 1: Section Name + [▶ Seek] + [⚙ Properties]
                    ui.horizontal(|ui| {
                        ui.strong(egui::RichText::new(&sec.name).color(egui::Color32::WHITE));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("⚙").on_hover_text("Open Section Properties (Name, Length, Meter, Tempo)").clicked() {
                                app.open_section_properties(sec);
                            }
                            if ui.small_button("▶").on_hover_text("Seek playhead to section downbeat").clicked() {
                                app.send_cmd(IpcCommand::Seek { tick: sec_start_tick });
                            }
                        });
                    });

                    // Row 2: Summary info
                    let sec_eff_ts = sec.time_signature.unwrap_or(project_ts);
                    let eff_bpm = sec.effective_bpm(project_bpm);
                    let length_str = if sec_len == 0 {
                        "Empty (0t)".to_string()
                    } else {
                        let tpb = sec_eff_ts.ticks_per_bar().max(1);
                        let bars = sec_len as f64 / tpb as f64;
                        format!("{:.1}b ({}t)", bars, sec_len)
                    };
                    let tempo_str = match sec.tempo {
                        Some(TempoOverride::AbsoluteBpm(b)) => format!("{:.1} BPM", b),
                        Some(TempoOverride::Ratio(n, d)) => format!("{}:{} ({:.1} BPM)", n, d, eff_bpm),
                        None => format!("{:.1} BPM", eff_bpm),
                    };
                    let meter_str = format!("{}/{}", sec_eff_ts.numerator, sec_eff_ts.denominator);

                    ui.small(
                        egui::RichText::new(format!("{} • {} • {}", length_str, meter_str, tempo_str))
                            .color(egui::Color32::from_rgb(175, 185, 205))
                    );
                });

                ui.separator();
            }

            // Append Section button column
            let (new_sec_rect, _) = ui.allocate_exact_size(
                egui::vec2(100.0, SECTION_HEADER_HEIGHT),
                egui::Sense::hover(),
            );
            let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(new_sec_rect));
            child_ui.centered_and_justified(|ui| {
                if ui.button("➕ Section").on_hover_text("Add a new song section").clicked() {
                    app.show_section_dialog = true;
                }
            });
        });

        ui.separator();

        // Track Rows: Each Track intersects all Sections
        for (t_idx, track) in tracks.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.set_min_height(TRACK_ROW_HEIGHT);

                // Track Control Header (Leftmost TRACK_HEADER_WIDTH)
                let (hdr_rect, _) = ui.allocate_exact_size(
                    egui::vec2(TRACK_HEADER_WIDTH, TRACK_ROW_HEIGHT),
                    egui::Sense::hover(),
                );
                ui.painter().rect_filled(
                    hdr_rect,
                    2.0,
                    egui::Color32::from_rgb(28, 32, 40),
                );
                let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(hdr_rect.shrink(4.0)));
                child_ui.vertical(|ui| {
                    let (need_rename, new_name_str) = {
                        let current_buf = app.track_name_edits.entry(track.id.clone()).or_insert_with(|| track.name.clone());
                        let resp = ui.add(egui::TextEdit::singleline(current_buf).desired_width(TRACK_HEADER_WIDTH - 20.0));
                        if resp.lost_focus() && *current_buf != track.name && !current_buf.trim().is_empty() {
                            (true, current_buf.trim().to_string())
                        } else {
                            (false, String::new())
                        }
                    };
                    if need_rename {
                        app.send_cmd(IpcCommand::RenameTrack { track_id: track.id.clone(), new_name: new_name_str.clone() });
                        app.status_msg = format!("Renamed track to '{}'", new_name_str);
                    }

                    ui.horizontal(|ui| {
                        ui.small(if track.current_bpm > 0.0 { format!("{:.1} BPM", track.current_bpm) } else { format!("{:.1} BPM", project_bpm) });

                        let mode_str = match track.arranger_mode {
                            TrackArrangerMode::Section => "Matrix",
                            TrackArrangerMode::Timeline => "Timeline",
                        };
                        if ui.small_button(format!("[{}]", mode_str)).on_hover_text("Switch track arranger mode between Section Matrix and continuous Timeline").clicked() {
                            let new_mode = match track.arranger_mode {
                                TrackArrangerMode::Section => TrackArrangerMode::Timeline,
                                TrackArrangerMode::Timeline => TrackArrangerMode::Section,
                            };
                            app.send_cmd(IpcCommand::SetTrackArrangerMode { track_id: track.id.clone(), mode: new_mode });
                        }

                        if ui.small_button("🎹").on_hover_text("Open Piano Roll for this track").clicked() {
                            if track.arranger_mode == TrackArrangerMode::Timeline {
                                if let Some(tb) = app.last_status.as_ref().and_then(|st| st.timeline_blocks.iter().find(|b| b.track_id == track.id)) {
                                    app.open_piano_roll = Some(OpenPianoRollTarget {
                                        track_id: track.id.clone(),
                                        block_id: tb.id.clone(),
                                        section_id: None,
                                    });
                                    app.selected_track_id = track.id.clone();
                                } else {
                                    app.status_msg = format!("Track '{}' has no timeline clips yet. Click in the timeline lane to create one.", track.name);
                                }
                            } else {
                                if let Some((s_id, blk_id)) = sections.iter().find_map(|s| s.track_blocks.get(&track.id).map(|b| (s.id.clone(), b.id.clone()))) {
                                    app.open_piano_roll = Some(OpenPianoRollTarget {
                                        track_id: track.id.clone(),
                                        block_id: blk_id,
                                        section_id: Some(s_id),
                                    });
                                    app.selected_track_id = track.id.clone();
                                } else {
                                    app.status_msg = format!("Track '{}' has no blocks yet. Click '+ Add Block' in a section.", track.name);
                                }
                            }
                        }
                        if ui.small_button("🎛").on_hover_text("Tweak Instruments & DSP").clicked() {
                            app.edit_master_insert_slot = None;
                            app.edit_track_id = Some(track.id.clone());
                        }

                        let mut muted = track.muted;
                        if ui.toggle_value(&mut muted, "M").clicked() {
                            app.send_cmd(IpcCommand::SetMute { track_id: track.id.clone(), muted });
                        }

                        let mut solo = track.solo;
                        if ui.toggle_value(&mut solo, "S").clicked() {
                            app.send_cmd(IpcCommand::SetSolo { track_id: track.id.clone(), solo });
                        }

                        if ui.small_button("🗑").on_hover_text("Delete track").clicked() {
                            app.send_cmd(IpcCommand::DeleteTrack { track_id: track.id.clone() });
                        }
                    });
                });

                ui.separator();

                if track.arranger_mode == TrackArrangerMode::Timeline {
                    // Continuous Horizontal Timeline Lane
                    let total_section_width: f32 = sections.iter().map(|s| compute_section_col_width(s.length_ticks(), 1.0)).sum::<f32>()
                        + (sections.len().saturating_sub(1) as f32 * (ui.spacing().item_spacing.x + 1.0));
                    let lane_width = total_section_width.max(200.0);
                    let total_timeline_ticks = total_ticks.max(1);

                    let (lane_rect, lane_resp) = ui.allocate_exact_size(
                        egui::vec2(lane_width, TRACK_ROW_HEIGHT),
                        egui::Sense::click_and_drag(),
                    );

                    // Lane background
                    ui.painter().rect_filled(
                        lane_rect,
                        2.0,
                        egui::Color32::from_rgb(20, 24, 30),
                    );

                    // Section boundary markers across the lane
                    let mut sec_cursor = 0u64;
                    for sec in sections.iter() {
                        let s_len = sec.length_ticks();
                        if s_len > 0 && sec_cursor > 0 {
                            let sec_pct = (sec_cursor as f32 / total_timeline_ticks as f32).clamp(0.0, 1.0);
                            let sx = lane_rect.left() + sec_pct * lane_rect.width();
                            ui.painter().line_segment(
                                [egui::pos2(sx, lane_rect.top()), egui::pos2(sx, lane_rect.bottom())],
                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
                            );
                            ui.painter().text(
                                egui::pos2(sx + 4.0, lane_rect.top() + 2.0),
                                egui::Align2::LEFT_TOP,
                                &sec.name,
                                egui::FontId::proportional(9.0),
                                egui::Color32::from_rgba_unmultiplied(160, 180, 210, 120),
                            );
                        }
                        sec_cursor += s_len;
                    }

                    let track_timeline_blocks: Vec<TrackBlock> = app.last_status.as_ref()
                        .map(|st| st.timeline_blocks.iter().filter(|b| b.track_id == track.id).cloned().collect())
                        .unwrap_or_default();

                    if track_timeline_blocks.is_empty() {
                        ui.painter().text(
                            lane_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "➕ Double-click to create continuous Timeline Clip",
                            egui::FontId::proportional(11.0),
                            egui::Color32::from_rgba_unmultiplied(160, 175, 200, 100),
                        );
                    }

                    let pointer_pos = lane_resp.interact_pointer_pos();

                    // Render timeline blocks
                    for block in &track_timeline_blocks {
                        let s_pct = (block.start_offset_ticks as f32 / total_timeline_ticks as f32).clamp(0.0, 1.0);
                        let l_pct = (block.base_length_ticks as f32 / total_timeline_ticks as f32).max(0.005);
                        let bx = lane_rect.left() + s_pct * lane_rect.width();
                        let bw = (l_pct * lane_rect.width()).max(48.0).min(lane_rect.right() - bx);
                        let block_rect = egui::Rect::from_min_size(
                            egui::pos2(bx, lane_rect.top() + 4.0),
                            egui::vec2(bw, lane_rect.height() - 8.0),
                        );

                        let is_open_in_piano_roll = app.open_piano_roll.as_ref().map(|t| t.section_id.is_none() && t.block_id == block.id && t.track_id == block.track_id).unwrap_or(false);
                        let card_color = if t_idx % 3 == 0 {
                            egui::Color32::from_rgb(35, 65, 95)
                        } else if t_idx % 3 == 1 {
                            egui::Color32::from_rgb(75, 45, 90)
                        } else {
                            egui::Color32::from_rgb(45, 80, 55)
                        };

                        let stroke = if is_open_in_piano_roll {
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 215, 0))
                        } else {
                            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60))
                        };

                        let painter = ui.painter();
                        painter.rect(block_rect, 4.0, card_color, stroke);

                        // Resize handle on right edge
                        let resize_handle_rect = egui::Rect::from_min_max(
                            egui::pos2(block_rect.right() - 8.0, block_rect.top()),
                            egui::pos2(block_rect.right() + 4.0, block_rect.bottom()),
                        );
                        let is_hovering_resize = ui.rect_contains_pointer(resize_handle_rect);
                        let is_resizing_this = app.timeline_block_drag.as_ref().map(|d| d.block_id == block.id && d.kind == BlockDragKind::Resize).unwrap_or(false);
                        if is_hovering_resize || is_resizing_this {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                        }
                        let grip_col = if is_resizing_this || is_hovering_resize {
                            egui::Color32::from_rgb(255, 215, 0)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 70)
                        };
                        painter.line_segment(
                            [egui::pos2(block_rect.right() - 2.0, block_rect.top() + 4.0), egui::pos2(block_rect.right() - 2.0, block_rect.bottom() - 4.0)],
                            egui::Stroke::new(2.0, grip_col),
                        );

                        // Interactive Buttons: Piano Roll & Delete
                        let piano_btn_rect = egui::Rect::from_min_size(
                            egui::pos2(block_rect.left() + 3.0, block_rect.top() + 3.0),
                            egui::vec2(16.0, 15.0),
                        );
                        let del_btn_rect = egui::Rect::from_min_size(
                            egui::pos2(block_rect.right() - 19.0, block_rect.top() + 3.0),
                            egui::vec2(16.0, 15.0),
                        );

                        let piano_id = ui.make_persistent_id(format!("tl_pbtn_{}", block.id));
                        let piano_resp = ui.interact(piano_btn_rect, piano_id, egui::Sense::click()).on_hover_text("Open Piano Roll");

                        let del_id = ui.make_persistent_id(format!("tl_dbtn_{}", block.id));
                        let del_resp = ui.interact(del_btn_rect, del_id, egui::Sense::click()).on_hover_text("Delete Clip");

                        if piano_resp.clicked() {
                            app.selected_track_id = track.id.clone();
                            app.open_piano_roll = Some(OpenPianoRollTarget {
                                track_id: block.track_id.clone(),
                                block_id: block.id.clone(),
                                section_id: None,
                            });
                        }
                        if del_resp.clicked() {
                            app.send_cmd(IpcCommand::RemoveBlock {
                                track_id: block.track_id.clone(),
                                block_id: block.id.clone(),
                                section_id: None,
                            });
                            if app.open_piano_roll.as_ref().map(|t| t.section_id.is_none() && t.block_id == block.id).unwrap_or(false) {
                                app.open_piano_roll = None;
                            }
                        }

                        let piano_color = if piano_resp.hovered() { egui::Color32::YELLOW } else { egui::Color32::from_rgba_unmultiplied(255, 255, 255, 210) };
                        painter.text(piano_btn_rect.center(), egui::Align2::CENTER_CENTER, "🎹", egui::FontId::proportional(11.0), piano_color);

                        let del_color = if del_resp.hovered() { egui::Color32::from_rgb(255, 100, 100) } else { egui::Color32::from_rgba_unmultiplied(220, 220, 220, 150) };
                        painter.text(del_btn_rect.center(), egui::Align2::CENTER_CENTER, "🗑", egui::FontId::proportional(10.0), del_color);

                        // Block Title
                        painter.text(
                            egui::pos2(piano_btn_rect.right() + 3.0, block_rect.top() + 4.0),
                            egui::Align2::LEFT_TOP,
                            format!("{} (T:{})", block.id, block.start_offset_ticks),
                            egui::FontId::proportional(10.5),
                            egui::Color32::WHITE,
                        );

                        // Note event previews
                        for ev in &block.events {
                            if let jamseq::timeline::ScheduledAction::NoteOn { .. } = ev.action {
                                let ev_pct = (ev.tick as f32 / block.base_length_ticks.max(1) as f32).clamp(0.0, 1.0);
                                let nx = block_rect.left() + ev_pct * block_rect.width();
                                painter.line_segment(
                                    [egui::pos2(nx, block_rect.top() + 18.0), egui::pos2(nx, block_rect.bottom() - 6.0)],
                                    egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180)),
                                );
                            }
                        }
                    }

                    // Lane Dragging & Clicking
                    if lane_resp.drag_started() {
                        if let Some(pos) = pointer_pos {
                            if let Some(hit_block) = track_timeline_blocks.iter().find(|b| {
                                let s_pct = (b.start_offset_ticks as f32 / total_timeline_ticks as f32).clamp(0.0, 1.0);
                                let l_pct = (b.base_length_ticks as f32 / total_timeline_ticks as f32).max(0.005);
                                let bx = lane_rect.left() + s_pct * lane_rect.width();
                                let bw = (l_pct * lane_rect.width()).max(48.0).min(lane_rect.right() - bx);
                                let r = egui::Rect::from_min_size(egui::pos2(bx, lane_rect.top() + 4.0), egui::vec2(bw, lane_rect.height() - 8.0));
                                r.contains(pos)
                            }) {
                                let s_pct = (hit_block.start_offset_ticks as f32 / total_timeline_ticks as f32).clamp(0.0, 1.0);
                                let l_pct = (hit_block.base_length_ticks as f32 / total_timeline_ticks as f32).max(0.005);
                                let bx = lane_rect.left() + s_pct * lane_rect.width();
                                let bw = (l_pct * lane_rect.width()).max(48.0).min(lane_rect.right() - bx);
                                let r = egui::Rect::from_min_size(egui::pos2(bx, lane_rect.top() + 4.0), egui::vec2(bw, lane_rect.height() - 8.0));
                                let is_resize = pos.x >= r.right() - 10.0;
                                let is_slip = ui.input(|i| i.modifiers.alt);
                                let kind = if is_resize {
                                    BlockDragKind::Resize
                                } else if is_slip {
                                    BlockDragKind::Slip
                                } else {
                                    BlockDragKind::Slide
                                };
                                app.timeline_block_drag = Some(TimelineBlockDragState {
                                    block_id: hit_block.id.clone(),
                                    track_id: track.id.clone(),
                                    initial_start_tick: hit_block.start_offset_ticks,
                                    initial_length_ticks: hit_block.base_length_ticks,
                                    start_pos_x: pos.x,
                                    kind,
                                });
                                app.selected_track_id = track.id.clone();
                            }
                        }
                    }

                    if lane_resp.dragged() {
                        if let Some(ref drag) = app.timeline_block_drag.clone() {
                            if drag.track_id == track.id {
                                if let Some(pos) = pointer_pos {
                                    let delta_px = pos.x - drag.start_pos_x;
                                    let ticks_per_px = total_timeline_ticks as f32 / lane_rect.width();
                                    let delta_ticks = (delta_px * ticks_per_px) as i64;
                                    let shift_held = ui.input(|i| i.modifiers.shift);
                                    let snap_quantum = 240i64; // 1/16th snap
                                    let snapped_delta = if shift_held || app.freehand_drift {
                                        delta_ticks
                                    } else {
                                        ((delta_ticks + snap_quantum / 2) / snap_quantum) * snap_quantum
                                    };

                                    if let Some(mut block) = track_timeline_blocks.iter().find(|b| b.id == drag.block_id).cloned() {
                                        match drag.kind {
                                            BlockDragKind::Resize => {
                                                let new_len = (drag.initial_length_ticks as i64 + snapped_delta).max(240) as u64;
                                                block.base_length_ticks = new_len;
                                                app.send_cmd(IpcCommand::UpdateBlock { block });
                                            }
                                            BlockDragKind::Slip => {
                                                for ev in &mut block.events {
                                                    ev.tick = (ev.tick as i64 + snapped_delta).max(0) as u64;
                                                }
                                                app.send_cmd(IpcCommand::UpdateBlock { block });
                                            }
                                            BlockDragKind::Slide => {
                                                let new_start = (drag.initial_start_tick as i64 + snapped_delta).max(0) as u64;
                                                block.start_offset_ticks = new_start;
                                                app.send_cmd(IpcCommand::UpdateBlock { block });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if lane_resp.drag_stopped() {
                        app.timeline_block_drag = None;
                    }

                    if lane_resp.double_clicked() {
                        if let Some(pos) = pointer_pos {
                            let hit = track_timeline_blocks.iter().find(|b| {
                                let s_pct = (b.start_offset_ticks as f32 / total_timeline_ticks as f32).clamp(0.0, 1.0);
                                let l_pct = (b.base_length_ticks as f32 / total_timeline_ticks as f32).max(0.005);
                                let bx = lane_rect.left() + s_pct * lane_rect.width();
                                let bw = (l_pct * lane_rect.width()).max(48.0).min(lane_rect.right() - bx);
                                let r = egui::Rect::from_min_size(egui::pos2(bx, lane_rect.top() + 4.0), egui::vec2(bw, lane_rect.height() - 8.0));
                                r.contains(pos)
                            });
                            if let Some(b) = hit {
                                app.selected_track_id = track.id.clone();
                                app.open_piano_roll = Some(OpenPianoRollTarget {
                                    track_id: b.track_id.clone(),
                                    block_id: b.id.clone(),
                                    section_id: None,
                                });
                            } else {
                                let click_pct = ((pos.x - lane_rect.left()) / lane_rect.width()).clamp(0.0, 1.0);
                                let raw_tick = (click_pct * total_timeline_ticks as f32) as u64;
                                let snap = 960u64; // Snap to nearest beat
                                let new_start = (raw_tick / snap) * snap;
                                let block_id = format!("{}_clip_{}", track.id, new_start);
                                app.send_cmd(IpcCommand::AddBlock {
                                    track_id: track.id.clone(),
                                    block_id: Some(block_id.clone()),
                                    section_id: None,
                                    start_tick: Some(new_start),
                                    length_ticks: Some(3840),
                                });
                                app.selected_track_id = track.id.clone();
                                app.open_piano_roll = Some(OpenPianoRollTarget {
                                    track_id: track.id.clone(),
                                    block_id,
                                    section_id: None,
                                });
                            }
                        }
                    }

                    // Playhead on timeline lane
                    if current_tick < total_timeline_ticks {
                        let p_pct = (current_tick as f32 / total_timeline_ticks as f32).clamp(0.0, 1.0);
                        let px = lane_rect.left() + p_pct * lane_rect.width();
                        ui.painter().line_segment(
                            [egui::pos2(px, lane_rect.top()), egui::pos2(px, lane_rect.bottom())],
                            egui::Stroke::new(2.0, egui::Color32::WHITE),
                        );
                    }

                    ui.separator();
                } else {
                    // Section Cells for this Track
                    let mut cell_accum_ticks = 0u64;
                    for sec in sections.iter() {
                        let sec_len = sec.length_ticks();
                        let sec_start_tick = cell_accum_ticks;
                        cell_accum_ticks += sec_len;

                        let col_width = compute_section_col_width(sec_len, 1.0);

                        let (cell_rect, cell_resp) = ui.allocate_exact_size(
                            egui::vec2(col_width, TRACK_ROW_HEIGHT),
                            egui::Sense::click_and_drag(),
                        );

                        // Background grid lines and cell canvas
                        ui.painter().rect_filled(
                            cell_rect,
                            2.0,
                            egui::Color32::from_rgb(25, 28, 35),
                        );

                        // If track has a block inside this section:
                        if let Some(block) = sec.track_blocks.get(&track.id) {
                            let is_open_in_piano_roll = app.open_piano_roll.as_ref().map(|t| t.section_id.as_deref() == Some(&sec.id) && t.track_id == track.id).unwrap_or(false);

                            // REAPER-style horizontal item positioning within cell
                            let block_rect = if sec_len > 0 {
                                let s_ratio = (block.start_offset_ticks as f32 / sec_len as f32).clamp(0.0, 1.0);
                                let l_ratio = (block.effective_length_ticks() as f32 / sec_len as f32).clamp(0.05, 1.0);
                                let bx = cell_rect.left() + s_ratio * cell_rect.width();
                                let bw = (l_ratio * cell_rect.width()).max(60.0).min(cell_rect.right() - bx);
                                egui::Rect::from_min_size(egui::pos2(bx, cell_rect.top() + 4.0), egui::vec2(bw, cell_rect.height() - 8.0))
                            } else {
                                egui::Rect::from_min_size(egui::pos2(cell_rect.left() + 4.0, cell_rect.top() + 4.0), egui::vec2(cell_rect.width() - 8.0, cell_rect.height() - 8.0))
                            };

                            let card_color = if t_idx % 3 == 0 {
                                egui::Color32::from_rgb(35, 65, 95) // Cyan/Navy Synth
                            } else if t_idx % 3 == 1 {
                                egui::Color32::from_rgb(75, 45, 90) // Purple Lead
                            } else {
                                egui::Color32::from_rgb(45, 80, 55) // Green Drums
                            };

                            let stroke = if is_open_in_piano_roll {
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 215, 0))
                            } else {
                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60))
                            };

                            let painter = ui.painter();
                            painter.rect(block_rect, 4.0, card_color, stroke);

                            let resize_handle_rect = egui::Rect::from_min_max(
                                egui::pos2(block_rect.right() - 8.0, block_rect.top()),
                                egui::pos2(block_rect.right() + 4.0, block_rect.bottom()),
                            );
                            let is_hovering_resize = ui.rect_contains_pointer(resize_handle_rect);
                            let is_resizing_this = app.block_drag.as_ref().map(|d| d.section_id == sec.id && d.track_id == track.id && d.kind == BlockDragKind::Resize).unwrap_or(false);
                            if is_hovering_resize || is_resizing_this {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                            }
                            let grip_col = if is_resizing_this || is_hovering_resize {
                                egui::Color32::from_rgb(255, 215, 0)
                            } else {
                                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 70)
                            };
                            painter.line_segment(
                                [egui::pos2(block_rect.right() - 2.0, block_rect.top() + 4.0), egui::pos2(block_rect.right() - 2.0, block_rect.bottom() - 4.0)],
                                egui::Stroke::new(2.0, grip_col),
                            );

                            // Interactive Buttons on Block Card: Piano Roll & Delete
                            let piano_btn_rect = egui::Rect::from_min_size(
                                egui::pos2(block_rect.left() + 3.0, block_rect.top() + 3.0),
                                egui::vec2(16.0, 15.0),
                            );
                            let del_btn_rect = egui::Rect::from_min_size(
                                egui::pos2(block_rect.right() - 19.0, block_rect.top() + 3.0),
                                egui::vec2(16.0, 15.0),
                            );

                            let piano_id = ui.make_persistent_id(format!("piano_btn_{}_{}", sec.id, track.id));
                            let piano_resp = ui.interact(piano_btn_rect, piano_id, egui::Sense::click()).on_hover_text("Open Piano Roll");

                            let del_id = ui.make_persistent_id(format!("del_btn_{}_{}", sec.id, track.id));
                            let del_resp = ui.interact(del_btn_rect, del_id, egui::Sense::click()).on_hover_text("Delete Block");

                            if piano_resp.clicked() {
                                app.selected_track_id = track.id.clone();
                                app.open_piano_roll = Some(OpenPianoRollTarget {
                                    track_id: track.id.clone(),
                                    block_id: block.id.clone(),
                                    section_id: Some(sec.id.clone()),
                                });
                            }
                            if del_resp.clicked() {
                                app.send_cmd(IpcCommand::RemoveBlock {
                                    track_id: track.id.clone(),
                                    block_id: block.id.clone(),
                                    section_id: Some(sec.id.clone()),
                                });
                                if app.open_piano_roll.as_ref().map(|t| t.section_id.as_deref() == Some(&sec.id) && t.track_id == track.id).unwrap_or(false) {
                                    app.open_piano_roll = None;
                                }
                            }

                            // Draw Piano & Del buttons
                            let piano_color = if piano_resp.hovered() {
                                egui::Color32::YELLOW
                            } else {
                                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 210)
                            };
                            painter.text(
                                piano_btn_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "🎹",
                                egui::FontId::proportional(11.0),
                                piano_color,
                            );

                            let del_color = if del_resp.hovered() {
                                egui::Color32::from_rgb(255, 100, 100)
                            } else {
                                egui::Color32::from_rgba_unmultiplied(220, 220, 220, 150)
                            };
                            painter.text(
                                del_btn_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "🗑",
                                egui::FontId::proportional(10.0),
                                del_color,
                            );

                            // Block title & offset readout
                            let title_text = if block.start_offset_ticks > 0 {
                                format!("{} (+{}t)", block.id, block.start_offset_ticks)
                            } else {
                                block.id.clone()
                            };
                            painter.text(
                                egui::pos2(piano_btn_rect.right() + 3.0, block_rect.top() + 4.0),
                                egui::Align2::LEFT_TOP,
                                title_text,
                                egui::FontId::proportional(10.5),
                                egui::Color32::WHITE,
                            );

                            // Loop Mode & Metric Modulation Badges
                            let mode_pill = match block.loop_mode {
                                LoopMode::OneShot => "1x",
                                LoopMode::LoopToSection => "Loop",
                                LoopMode::FixedLoops(n) => if n == 2 { "2x" } else { "4x" },
                            };
                            painter.text(
                                egui::pos2(del_btn_rect.left() - 4.0, block_rect.top() + 4.0),
                                egui::Align2::RIGHT_TOP,
                                mode_pill,
                                egui::FontId::monospace(10.0),
                                egui::Color32::from_rgb(200, 220, 255),
                            );

                            if let Some(t_override) = block.tempo {
                                let badge_str = match t_override {
                                    TempoOverride::Ratio(n, d) => format!("{}:{}", n, d),
                                    TempoOverride::AbsoluteBpm(b) => format!("{:.0}BPM", b),
                                };
                                painter.text(
                                    egui::pos2(block_rect.left() + 6.0, block_rect.bottom() - 14.0),
                                    egui::Align2::LEFT_TOP,
                                    badge_str,
                                    egui::FontId::monospace(10.0),
                                    egui::Color32::from_rgb(255, 180, 50),
                                );
                            }

                            if let Some(ts) = block.time_signature {
                                let badge_x = if block.tempo.is_some() { block_rect.left() + 52.0 } else { block_rect.left() + 6.0 };
                                painter.text(
                                    egui::pos2(badge_x, block_rect.bottom() - 14.0),
                                    egui::Align2::LEFT_TOP,
                                    format!("{}/{}", ts.numerator, ts.denominator),
                                    egui::FontId::monospace(10.0),
                                    egui::Color32::from_rgb(220, 150, 255),
                                );
                            }

                            // Miniature Note Previews
                            for ev in &block.events {
                                if let jamseq::timeline::ScheduledAction::NoteOn { .. } = ev.action {
                                    let ev_pct = (ev.tick as f32 / block.base_length_ticks.max(1) as f32).clamp(0.0, 1.0);
                                    let nx = block_rect.left() + ev_pct * block_rect.width();
                                    painter.line_segment(
                                        [egui::pos2(nx, block_rect.top() + 18.0), egui::pos2(nx, block_rect.bottom() - 6.0)],
                                        egui::Stroke::new(2.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180)),
                                    );
                                }
                            }

                            let btn_hit = piano_resp.hovered() || del_resp.hovered() || piano_resp.clicked() || del_resp.clicked();

                            // --- INTERACTIVE CLICKS & DRAGGING ---
                            if !btn_hit {
                                if cell_resp.double_clicked() {
                                    app.selected_track_id = track.id.clone();
                                    app.open_piano_roll = Some(OpenPianoRollTarget {
                                        track_id: track.id.clone(),
                                        block_id: block.id.clone(),
                                        section_id: Some(sec.id.clone()),
                                    });
                                } else if cell_resp.clicked() {
                                    app.selected_track_id = track.id.clone();
                                }

                                if cell_resp.drag_started() {
                                    app.selected_track_id = track.id.clone();
                                    let start_pos = cell_resp.interact_pointer_pos().map(|p| p.x).unwrap_or(0.0);
                                    let is_resize = (start_pos >= block_rect.right() - 10.0 && start_pos <= block_rect.right() + 6.0) || is_hovering_resize;
                                    let is_slip = ui.input(|i| i.modifiers.alt);
                                    let kind = if is_resize {
                                        BlockDragKind::Resize
                                    } else if is_slip {
                                        BlockDragKind::Slip
                                    } else {
                                        BlockDragKind::Slide
                                    };
                                    app.block_drag = Some(BlockDragState {
                                        section_id: sec.id.clone(),
                                        track_id: track.id.clone(),
                                        initial_offset: block.start_offset_ticks,
                                        initial_length: block.base_length_ticks,
                                        start_pos_x: start_pos,
                                        kind,
                                    });
                                }
                            }

                            if cell_resp.dragged() {
                                if let Some(ref drag) = app.block_drag.clone() {
                                    if drag.section_id == sec.id && drag.track_id == track.id {
                                        if let Some(pos) = cell_resp.interact_pointer_pos() {
                                            let delta_px = pos.x - drag.start_pos_x;
                                            let ticks_per_px = if sec_len > 0 { sec_len as f32 / cell_rect.width() } else { 3840.0 / cell_rect.width() };
                                            let delta_ticks = (delta_px * ticks_per_px) as i64;
                                            let shift_held = ui.input(|i| i.modifiers.shift);

                                            match drag.kind {
                                                BlockDragKind::Resize => {
                                                    let eff_ts = block.time_signature.or(sec.time_signature).unwrap_or_default();
                                                    let tp_beat = eff_ts.ticks_per_beat().max(240);
                                                    let snapped_delta = if shift_held || app.freehand_drift {
                                                        delta_ticks
                                                    } else {
                                                        ((delta_ticks + tp_beat as i64 / 2) / tp_beat as i64) * tp_beat as i64
                                                    };
                                                    let new_length = (drag.initial_length as i64 + snapped_delta).max(tp_beat as i64) as u64;
                                                    let mut updated = block.clone();
                                                    updated.base_length_ticks = new_length;
                                                    app.send_cmd(IpcCommand::UpdateBlock {
                                                        block: updated,
                                                    });
                                                }
                                                BlockDragKind::Slip => {
                                                    let snapped_delta = if shift_held || app.freehand_drift {
                                                        delta_ticks
                                                    } else {
                                                        (delta_ticks / 240) * 240
                                                    };
                                                    let mut updated = block.clone();
                                                    for ev in &mut updated.events {
                                                        ev.tick = (ev.tick as i64 + snapped_delta).max(0) as u64;
                                                    }
                                                    app.send_cmd(IpcCommand::UpdateBlock {
                                                        block: updated,
                                                    });
                                                }
                                                BlockDragKind::Slide => {
                                                    let snapped_delta = if shift_held || app.freehand_drift {
                                                        delta_ticks
                                                    } else {
                                                        (delta_ticks / 240) * 240
                                                    };
                                                    let new_offset = (drag.initial_offset as i64 + snapped_delta).max(0) as u64;
                                                    let mut updated = block.clone();
                                                    updated.start_offset_ticks = new_offset;
                                                    app.send_cmd(IpcCommand::UpdateBlock {
                                                        block: updated,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            if cell_resp.drag_stopped() {
                                app.block_drag = None;
                            }
                        } else {
                            // Empty / Rest Cell (Silence for this track in this section)
                            let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(cell_rect));
                            child_ui.centered_and_justified(|ui| {
                                if ui.small_button("➕ Add Block").on_hover_text("Add a track block / pattern to this section").clicked() {
                                    let block_id = format!("{}_{}", sec.id, track.id);
                                    app.send_cmd(IpcCommand::AddBlock {
                                        track_id: track.id.clone(),
                                        block_id: Some(block_id.clone()),
                                        section_id: Some(sec.id.clone()),
                                        start_tick: None,
                                        length_ticks: Some(sec.length_ticks),
                                    });
                                    app.selected_track_id = track.id.clone();
                                    app.open_piano_roll = Some(OpenPianoRollTarget {
                                        track_id: track.id.clone(),
                                        block_id,
                                        section_id: Some(sec.id.clone()),
                                    });
                                }
                            });
                        }

                        // Playhead drawing across cell
                        if current_tick >= sec_start_tick && current_tick < sec_start_tick + sec_len && sec_len > 0 {
                            let rel_tick = current_tick - sec_start_tick;
                            let p_pct = (rel_tick as f32 / sec_len as f32).clamp(0.0, 1.0);
                            let px = cell_rect.left() + p_pct * cell_rect.width();
                            ui.painter().line_segment(
                                [egui::pos2(px, cell_rect.top()), egui::pos2(px, cell_rect.bottom())],
                                egui::Stroke::new(2.0, egui::Color32::WHITE),
                            );
                        }

                        ui.separator();
                    }
                }

                // Matching tail cell under the New Section column
                let (_tail_rect, _) = ui.allocate_exact_size(
                    egui::vec2(100.0, TRACK_ROW_HEIGHT),
                    egui::Sense::hover(),
                );
            });

            ui.add_space(2.0);
        }
    });
}
