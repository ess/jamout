use eframe::egui;
use jamseq::ipc::IpcCommand;
use crate::search_input::SearchablePicker;
use crate::JamOutApp;

pub fn render_mixer(app: &mut JamOutApp, ui: &mut egui::Ui) {
    ui.heading("Console Mixer");
    ui.add_space(8.0);

    let tracks = app.last_status.as_ref().map(|s| s.tracks.clone()).unwrap_or_default();
    let master_vol = app.last_status.as_ref().map(|s| s.master_volume).unwrap_or(0.9);
    let master_peak = app.last_status.as_ref().map(|s| s.master_peak).unwrap_or(0.0);
    let master_inserts = app.last_status.as_ref().map(|s| s.master_inserts.clone()).unwrap_or_default();

    // --- 1. MASTER BUS ROW (Anchored at top) ---
    ui.group(|ui| {
        ui.horizontal(|ui| {
            ui.set_min_height(44.0);

            // Master Header (Fixed 180px width)
            ui.allocate_ui(egui::vec2(180.0, 40.0), |ui| {
                ui.set_width(180.0);
                ui.set_max_width(180.0);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new("🔊 MASTER BUS").strong().size(15.0));
                    ui.small("Main Stereo Output");
                });
            });

            ui.separator();

            // Master Volume Fader
            ui.vertical(|ui| {
                let mut m_vol = master_vol;
                let db_str = if m_vol <= 0.0001 {
                    "-inf dB".to_string()
                } else {
                    format!("{:.1} dB", 20.0 * m_vol.log10())
                };

                ui.horizontal(|ui| {
                    ui.label("Master Gain:");
                    ui.monospace(&db_str);
                });

                let fader_width = (ui.available_width() - 120.0).max(80.0);
                if ui.add_sized([fader_width, 20.0], egui::Slider::new(&mut m_vol, 0.0..=1.5).text("").show_value(false)).changed() {
                    app.send_cmd(IpcCommand::SetMasterVolume { volume: m_vol });
                }
            });

            ui.separator();

            // Master VU Peak Meter
            ui.allocate_ui(egui::vec2(90.0, 40.0), |ui| {
                ui.vertical(|ui| {
                    let peak_db = if master_peak <= 0.0001 {
                        "-inf dB".to_string()
                    } else {
                        format!("{:.1} dB", 20.0 * master_peak.log10())
                    };
                    ui.horizontal(|ui| {
                        ui.small("VU Peak:");
                        ui.monospace(peak_db);
                    });
                    let vu_pct = master_peak.clamp(0.0, 1.2) / 1.2;
                    let pb = egui::ProgressBar::new(vu_pct.min(1.0))
                        .desired_width(85.0)
                        .animate(false);
                    ui.add(pb);
                });
            });
        });

        // Master Inserts Section
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("⚡ Master Inserts:").strong());

            let available = app.last_status.as_ref()
                .map(|s| s.available_plugins.clone())
                .unwrap_or_default();

            let chosen_effect = SearchablePicker::show(
                ui,
                "add_master_insert_sel".to_string(),
                "➕ Add Master Effect...",
                &mut app.insert_search_filter,
                &available,
                |plug| format!("{} ({})", plug.name, plug.plugin_format),
            );

            if let Some(plug) = chosen_effect {
                let display = format!("{} ({})", plug.name, plug.plugin_format);
                app.send_cmd(IpcCommand::AddMasterInsert {
                    plugin_uri_or_path: plug.uri_or_path.clone(),
                    plugin_name: display.clone(),
                });
                app.status_msg = format!("Added Master insert effect '{}'", display);
            }

            if master_inserts.is_empty() {
                ui.small("No master insert effects (e.g. Master Limiter, Compressor, EQ).");
            }
        });

        if !master_inserts.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for (ins_idx, ins) in master_inserts.iter().enumerate() {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.strong(format!("{}. {}", ins_idx + 1, ins.name));
                            let is_effect_active = app.edit_master_insert_slot == Some(ins_idx);
                            let edit_btn = if is_effect_active {
                                egui::Button::new("🎛").fill(egui::Color32::from_rgb(50, 90, 150))
                            } else {
                                egui::Button::new("🎛")
                            };
                            if ui.add(edit_btn).on_hover_text("Open Master Effect Controls in slide-out panel").clicked() {
                                if is_effect_active {
                                    app.edit_master_insert_slot = None;
                                } else {
                                    app.edit_track_id = None;
                                    app.edit_insert_slot = None;
                                    app.edit_master_insert_slot = Some(ins_idx);
                                }
                            }
                            if ui.small_button("🗑").on_hover_text("Remove this master effect").clicked() {
                                app.send_cmd(IpcCommand::RemoveMasterInsert {
                                    insert_index: ins_idx,
                                });
                                app.status_msg = format!("Removed master insert effect '{}'", ins.name);
                                if app.edit_master_insert_slot == Some(ins_idx) {
                                    app.edit_master_insert_slot = None;
                                }
                            }
                        });
                    });
                }
            });
        }
    });

    ui.add_space(10.0);
    ui.separator();
    ui.add_space(6.0);

    if tracks.is_empty() {
        ui.label("No active tracks connected in jamseq host.");
        return;
    }

    // --- 2. INDIVIDUAL TRACK ROWS (Scrollable) ---
    ui.label(egui::RichText::new("Track Channels:").strong());
    ui.add_space(4.0);

    egui::ScrollArea::vertical().show(ui, |ui| {
        for (idx, track) in tracks.iter().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.set_min_height(48.0);

                    // Track Header (Fixed 220px width to match timeline)
                    ui.allocate_ui(egui::vec2(220.0, 44.0), |ui| {
                        ui.set_width(220.0);
                        ui.set_max_width(220.0);
                        ui.vertical(|ui| {
                            let (need_rename, new_name_str) = {
                                let current_buf = app.track_name_edits.entry(track.id.clone()).or_insert_with(|| track.name.clone());
                                let resp = ui.add(
                                    egui::TextEdit::singleline(current_buf)
                                        .desired_width(210.0)
                                        .hint_text("Track Name")
                                );
                                if resp.lost_focus() && *current_buf != track.name && !current_buf.trim().is_empty() {
                                    (true, current_buf.trim().to_string())
                                } else {
                                    (false, String::new())
                                }
                            };
                            if need_rename {
                                app.send_cmd(IpcCommand::RenameTrack {
                                    track_id: track.id.clone(),
                                    new_name: new_name_str.clone(),
                                });
                                app.status_msg = format!("Renamed track to '{}'", new_name_str);
                            }
                            ui.horizontal(|ui| {
                                ui.small(&format!("ID: {}", track.id));
                                if ui.small_button("🎛").on_hover_text("Open DSP / Synth Parameter Controls").clicked() {
                                    if app.edit_track_id.as_deref() != Some(&track.id) {
                                        app.edit_insert_slot = None;
                                    }
                                    app.edit_master_insert_slot = None;
                                    app.edit_track_id = Some(track.id.clone());
                                }
                                if idx > 0 && ui.small_button("▲").on_hover_text("Move track up").clicked() {
                                    app.send_cmd(IpcCommand::ReorderTrack { from: idx, to: idx - 1 });
                                }
                                if idx + 1 < tracks.len() && ui.small_button("▼").on_hover_text("Move track down").clicked() {
                                    app.send_cmd(IpcCommand::ReorderTrack { from: idx, to: idx + 1 });
                                }
                                if ui.small_button("🗑").on_hover_text("Delete track").clicked() {
                                    app.send_cmd(IpcCommand::DeleteTrack { track_id: track.id.clone() });
                                    app.status_msg = format!("Deleted track '{}'", track.name);
                                }
                            });
                        });
                    });

                    ui.separator();

                    // Mute & Solo Toggles
                    ui.allocate_ui(egui::vec2(80.0, 44.0), |ui| {
                        ui.horizontal(|ui| {
                            let mut muted = track.muted;
                            let mute_text = if muted {
                                egui::RichText::new("M").color(egui::Color32::from_rgb(255, 100, 100)).strong()
                            } else {
                                egui::RichText::new("M")
                            };
                            if ui.toggle_value(&mut muted, mute_text).on_hover_text("Mute track").clicked() {
                                app.send_cmd(IpcCommand::SetMute {
                                    track_id: track.id.clone(),
                                    muted,
                                });
                            }

                            let mut solo = track.solo;
                            let solo_text = if solo {
                                egui::RichText::new("S").color(egui::Color32::from_rgb(255, 215, 0)).strong()
                            } else {
                                egui::RichText::new("S")
                            };
                            if ui.toggle_value(&mut solo, solo_text).on_hover_text("Solo track").clicked() {
                                app.send_cmd(IpcCommand::SetSolo {
                                    track_id: track.id.clone(),
                                    solo,
                                });
                            }
                        });
                    });

                    ui.separator();

                    // Pan Control (-100% to +100%)
                    ui.allocate_ui(egui::vec2(130.0, 44.0), |ui| {
                        ui.vertical(|ui| {
                            let mut pan = track.pan;
                            let pan_label = if pan < -0.05 {
                                format!("Pan: L {:.0}%", -pan * 100.0)
                            } else if pan > 0.05 {
                                format!("Pan: R {:.0}%", pan * 100.0)
                            } else {
                                "Pan: Center".to_string()
                            };
                            ui.small(pan_label);
                            if ui.add_sized([120.0, 18.0], egui::Slider::new(&mut pan, -1.0..=1.0).show_value(false)).on_hover_text("Stereo pan (-1.0 Left to +1.0 Right)").changed() {
                                app.send_cmd(IpcCommand::SetPan {
                                    track_id: track.id.clone(),
                                    pan,
                                });
                            }
                        });
                    });

                    ui.separator();

                    // Volume Fader
                    ui.vertical(|ui| {
                        let mut vol = track.volume;
                        let db_str = if vol <= 0.0001 {
                            "-inf dB".to_string()
                        } else {
                            format!("{:.1} dB", 20.0 * vol.log10())
                        };
                        ui.horizontal(|ui| {
                            ui.label("Volume:");
                            ui.monospace(&db_str);
                        });
                        let slider_width = (ui.available_width() - 110.0).max(60.0);
                        if ui.add_sized([slider_width, 20.0], egui::Slider::new(&mut vol, 0.0..=1.5).show_value(false)).on_hover_text("Track channel volume").changed() {
                            app.send_cmd(IpcCommand::SetVolume {
                                track_id: track.id.clone(),
                                volume: vol,
                            });
                        }
                    });

                    ui.separator();

                    // Track Channel VU Peak Meter
                    ui.allocate_ui(egui::vec2(90.0, 44.0), |ui| {
                        ui.vertical(|ui| {
                            let peak_db = if track.peak_level <= 0.0001 {
                                "-inf dB".to_string()
                            } else {
                                format!("{:.1} dB", 20.0 * track.peak_level.log10())
                            };
                            ui.horizontal(|ui| {
                                ui.small("VU:");
                                ui.monospace(peak_db);
                            });
                            let vu_pct = track.peak_level.clamp(0.0, 1.2) / 1.2;
                            let pb = egui::ProgressBar::new(vu_pct.min(1.0))
                                .desired_width(85.0)
                                .animate(false);
                            ui.add(pb);
                        });
                    });
                });
            });
            ui.add_space(4.0);
        }
    });
}
