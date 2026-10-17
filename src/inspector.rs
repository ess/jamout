use eframe::egui;
use jamseq::ipc::{IpcCommand, ParamType, TrackParamStatus};
use crate::search_input::{SearchInput, SearchablePicker};
use crate::sliding_panel::SlidingPanelStack;
use crate::JamOutApp;

pub fn render_grouped_parameter_list<F>(
    ui: &mut egui::Ui,
    id_salt_prefix: &str,
    params: &[TrackParamStatus],
    filter_text: &str,
    mut on_change: F,
) where
    F: FnMut(&str, f32),
{
    let filter_lower = filter_text.trim().to_lowercase();
    let filtered_params: Vec<&TrackParamStatus> = params
        .iter()
        .filter(|p| {
            if filter_lower.is_empty() {
                true
            } else {
                p.name.to_lowercase().contains(&filter_lower) || p.group.to_lowercase().contains(&filter_lower)
            }
        })
        .collect();

    if filtered_params.is_empty() {
        ui.label("No parameters match the search filter.");
        return;
    }

    // Preserve original discovery / plugin parameter order when grouping
    let mut groups: Vec<(String, Vec<&TrackParamStatus>)> = Vec::new();
    for p in filtered_params {
        if let Some((_, list)) = groups.iter_mut().find(|(g, _)| g == &p.group) {
            list.push(p);
        } else {
            groups.push((p.group.clone(), vec![p]));
        }
    }

    for (group_name, group_params) in groups {
        egui::CollapsingHeader::new(format!("📁 {} ({})", group_name, group_params.len()))
            .id_salt(format!("{}_grp_{}", id_salt_prefix, group_name))
            .default_open(true)
            .show(ui, |ui| {
                for param in group_params {
                    ui.horizontal(|ui| {
                        ui.set_min_width(130.0);
                        let label = match param.name.as_str() {
                            "cutoff" => "Cutoff (Hz)",
                            "resonance" => "Resonance",
                            "glide_ms" => "Glide (ms)",
                            "drive" => "Drive / Sat",
                            "attack_ms" => "Attack (ms)",
                            "decay_ms" => "Decay (ms)",
                            "sustain" => "Sustain",
                            "release_ms" => "Release (ms)",
                            other => other,
                        };
                        ui.label(label);

                        match &param.param_type {
                            ParamType::Bool => {
                                let mut b = param.value > 0.5;
                                let chk_label = if b { "On" } else { "Off" };
                                if ui.checkbox(&mut b, chk_label).changed() {
                                    on_change(&param.name, if b { 1.0 } else { 0.0 });
                                }
                            }
                            ParamType::Enum { labels } => {
                                let is_normalized = labels.len() > 1 && (param.max - param.min) <= 1.0;
                                let current_idx = if is_normalized {
                                    let norm = ((param.value - param.min) / (param.max - param.min)).clamp(0.0, 1.0);
                                    (norm * (labels.len() - 1) as f32).round() as usize
                                } else {
                                    (param.value - param.min).round() as usize
                                };
                                let current_label = labels.get(current_idx).cloned()
                                    .or_else(|| param.display_value.clone())
                                    .unwrap_or_else(|| format!("{:.0}", param.value));

                                egui::ComboBox::from_id_salt(format!("{}_enum_{}", id_salt_prefix, param.name))
                                    .selected_text(current_label)
                                    .show_ui(ui, |ui| {
                                        for (i, lbl) in labels.iter().enumerate() {
                                            let is_selected = i == current_idx;
                                            if ui.selectable_label(is_selected, lbl).clicked() {
                                                let target_val = if is_normalized {
                                                    param.min + (i as f32 / (labels.len() - 1) as f32) * (param.max - param.min)
                                                } else {
                                                    param.min + i as f32
                                                };
                                                on_change(&param.name, target_val);
                                            }
                                        }
                                    });
                            }
                            ParamType::Int => {
                                let mut val = param.value.round() as i64;
                                let min_i = param.min.round() as i64;
                                let max_i = param.max.round() as i64;
                                let resp = ui.add(egui::Slider::new(&mut val, min_i..=max_i).step_by(1.0));
                                if resp.changed() || resp.drag_stopped() {
                                    on_change(&param.name, val as f32);
                                }
                                if let Some(ref disp) = param.display_value {
                                    ui.monospace(disp);
                                }
                            }
                            ParamType::Float => {
                                let mut val = param.value;
                                let resp = ui.add(egui::Slider::new(&mut val, param.min..=param.max));
                                if resp.changed() || resp.drag_stopped() {
                                    on_change(&param.name, val);
                                }
                                if let Some(ref disp) = param.display_value {
                                    ui.monospace(disp);
                                }
                            }
                        }
                    });
                }
            });
    }
}

pub fn render_inspector(app: &mut JamOutApp, ctx: &egui::Context) {
    // Live Plugin / Synth Fiddling & Parameter Inspector Modal
    if let Some(ref track_id) = app.edit_track_id.clone() {
        let track_opt = app.last_status.as_ref().and_then(|s| {
            s.tracks.iter().find(|t| t.id == *track_id).cloned()
        });

        if let Some(track) = track_opt {
            let mut is_open = true;
            let mut root_stack = SlidingPanelStack::docked_left(44.0);
            let inst_rect = root_stack.panel(
                ctx,
                egui::Id::new("instrument_inspector_window"),
                format!("🎛 Tweak Instrument: {}", track.name),
                &mut is_open,
                |ui| {
                    ui.horizontal(|ui| {
                        let (need_rename, new_name_str) = {
                            let current_buf = app.track_name_edits.entry(track.id.clone()).or_insert_with(|| track.name.clone());
                            let resp = ui.add(
                                egui::TextEdit::singleline(current_buf)
                                    .desired_width(180.0)
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
                    });
                    ui.small(format!("Track ID: {} | Live DSP Control", track.id));
                    ui.separator();

                    // Dynamic Plugin / Instrument Switcher
                    ui.horizontal(|ui| {
                        ui.label("Active Instrument:");
                        let current_plug = track.current_plugin.as_deref().unwrap_or("None");

                        let available = app.last_status.as_ref()
                            .map(|s| s.available_plugins.clone())
                            .unwrap_or_default();

                        let chosen_plug = SearchablePicker::show(
                            ui,
                            format!("plugin_sel_{}", track.id),
                            current_plug,
                            &mut app.plugin_search_filter,
                            &available,
                            |plug| format!("{} ({})", plug.name, plug.plugin_format),
                        );

                        if let Some(plug) = chosen_plug {
                            let display = format!("{} ({})", plug.name, plug.plugin_format);
                            app.send_cmd(IpcCommand::SwitchTrackPlugin {
                                track_id: track.id.clone(),
                                plugin_uri_or_type: plug.uri_or_path.clone(),
                                plugin_name: display,
                            });
                        }

                        if ui.button("🔄").on_hover_text("Rescan system for newly installed LV2 and CLAP plugins").clicked() {
                            app.send_cmd(IpcCommand::RescanPlugins);
                            app.status_msg = "Rescanning system LV2 and CLAP plugins...".to_string();
                        }
                    });
                    ui.separator();

                    if track.params.is_empty() {
                        ui.label("No tweakable DSP parameters exposed for this track (e.g. raw audio clip).");
                    } else {
                        ui.horizontal(|ui| {
                            ui.label(format!("Live Parameters ({})", track.params.len()));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add(
                                    SearchInput::new(&mut app.param_search_filter, "🔍 Search parameters...")
                                        .id_salt("param_search_input")
                                        .desired_width(180.0)
                                );
                            });
                        });
                        ui.add_space(4.0);

                        egui::ScrollArea::vertical()
                            .max_height(280.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                render_grouped_parameter_list(
                                    ui,
                                    &format!("track_{}", track.id),
                                    &track.params,
                                    &app.param_search_filter,
                                    |name, val| {
                                        app.send_cmd(IpcCommand::SetTrackParam {
                                            track_id: track.id.clone(),
                                            param_name: name.to_string(),
                                            value: val,
                                        });
                                    },
                                );
                            });

                        ui.add_space(8.0);
                        ui.separator();
                        ui.label("💾 Preset Management:");
                        if track.current_plugin.as_deref().map(|p| p.to_lowercase().contains("jamsynth")).unwrap_or(false) {
                            ui.horizontal(|ui| {
                                ui.label("Quick Presets:");
                                if ui.button("808 Sub").clicked() {
                                    app.send_cmd(IpcCommand::LoadTrackPreset {
                                        track_id: track.id.clone(),
                                        preset_name: "808_sub.json".to_string(),
                                    });
                                    app.status_msg = "Loaded 808 Sub preset".to_string();
                                }
                                if ui.button("Dark Reese").clicked() {
                                    app.send_cmd(IpcCommand::LoadTrackPreset {
                                        track_id: track.id.clone(),
                                        preset_name: "dark_reese.json".to_string(),
                                    });
                                    app.status_msg = "Loaded Dark Reese preset".to_string();
                                }
                                if ui.button("Cold Pad").clicked() {
                                    app.send_cmd(IpcCommand::LoadTrackPreset {
                                        track_id: track.id.clone(),
                                        preset_name: "cold_gothic_pad.json".to_string(),
                                    });
                                    app.status_msg = "Loaded Cold Pad preset".to_string();
                                }
                            });
                        }
                        ui.horizontal(|ui| {
                            ui.label("File:");
                            ui.text_edit_singleline(&mut app.preset_save_name);
                        });
                        ui.horizontal(|ui| {
                            if ui.button("💾 Save Preset As...").clicked() {
                                app.send_cmd(IpcCommand::SaveTrackPreset {
                                    track_id: track.id.clone(),
                                    preset_name: app.preset_save_name.clone(),
                                });
                                app.status_msg = format!("Saved preset to presets/{}", app.preset_save_name);
                            }
                            if ui.button("📂 Load Preset").clicked() {
                                app.send_cmd(IpcCommand::LoadTrackPreset {
                                    track_id: track.id.clone(),
                                    preset_name: app.preset_save_name.clone(),
                                });
                                app.status_msg = format!("Loaded preset from {}", app.preset_save_name);
                            }
                        });

                        ui.add_space(8.0);
                        ui.separator();

                        // --- INSERT EFFECTS CHAIN SECTION ---
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("⚡ Insert Effects Chain").strong());

                            let available = app.last_status.as_ref()
                                .map(|s| s.available_plugins.clone())
                                .unwrap_or_default();

                            let chosen_effect = SearchablePicker::show(
                                ui,
                                format!("add_insert_sel_{}", track.id),
                                "➕ Add Effect...",
                                &mut app.insert_search_filter,
                                &available,
                                |plug| format!("{} ({})", plug.name, plug.plugin_format),
                            );

                            if let Some(plug) = chosen_effect {
                                let display = format!("{} ({})", plug.name, plug.plugin_format);
                                app.send_cmd(IpcCommand::AddTrackInsert {
                                    track_id: track.id.clone(),
                                    plugin_uri_or_path: plug.uri_or_path.clone(),
                                    plugin_name: display.clone(),
                                });
                                app.status_msg = format!("Added insert effect '{}'", display);
                            }
                        });

                        if track.inserts.is_empty() {
                            ui.small("No insert effects on this track. Add effects like Reverb, Delay, or Guitar Amp above.");
                        } else {
                            for (ins_idx, ins) in track.inserts.iter().enumerate() {
                                ui.group(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.strong(format!("{}. {}", ins_idx + 1, ins.name));
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if ui.small_button("🗑").on_hover_text("Remove this insert effect").clicked() {
                                                app.send_cmd(IpcCommand::RemoveTrackInsert {
                                                    track_id: track.id.clone(),
                                                    insert_index: ins_idx,
                                                });
                                                app.status_msg = format!("Removed insert effect '{}'", ins.name);
                                                if app.edit_insert_slot == Some((track.id.clone(), ins_idx)) {
                                                    app.edit_insert_slot = None;
                                                }
                                            }
                                            let is_effect_active = app.edit_insert_slot == Some((track.id.clone(), ins_idx));
                                            let edit_btn = if is_effect_active {
                                                egui::Button::new("🎛").fill(egui::Color32::from_rgb(50, 90, 150))
                                            } else {
                                                egui::Button::new("🎛")
                                            };
                                            if ui.add(edit_btn).on_hover_text("Open Effect Controls in slide-out panel").clicked() {
                                                if is_effect_active {
                                                    app.edit_insert_slot = None;
                                                } else {
                                                    app.edit_insert_slot = Some((track.id.clone(), ins_idx));
                                                }
                                            }
                                        });
                                    });
                                });
                            }
                        }
                    }
                },
            );

            if !is_open {
                app.edit_track_id = None;
                app.edit_insert_slot = None;
            } else if let Some(parent_rect) = inst_rect {
                // Child sliding panel stack docked directly to parent instrument panel
                if let Some((ref tr_id, ins_idx)) = app.edit_insert_slot.clone() {
                    if tr_id == &track.id {
                        if let Some(ins) = track.inserts.get(ins_idx).cloned() {
                            let mut is_effect_open = true;
                            let mut effect_stack = SlidingPanelStack::from_parent(parent_rect);

                            let _effect_rect = effect_stack.panel(
                                ctx,
                                egui::Id::new("effect_inspector_window"),
                                format!("⚡ Effect: {} (Track: {})", ins.name, track.name),
                                &mut is_effect_open,
                                |ui| {
                                    ui.label(format!("Plugin: {}", ins.name));
                                    ui.small(format!("URI: {}", ins.uri));
                                    ui.separator();

                                    if ins.params.is_empty() {
                                        ui.label("No exposed control parameters for this plugin.");
                                    } else {
                                        ui.horizontal(|ui| {
                                            ui.label(format!("Parameters ({})", ins.params.len()));
                                            if ins.params.len() > 6 {
                                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                    ui.add(
                                                        SearchInput::new(&mut app.effect_param_search_filter, "🔍 Search parameters...")
                                                            .id_salt("effect_param_search_input")
                                                            .desired_width(180.0)
                                                    );
                                                });
                                            }
                                        });
                                        ui.add_space(4.0);

                                        egui::ScrollArea::vertical()
                                            .max_height(350.0)
                                            .auto_shrink([false, false])
                                            .show(ui, |ui| {
                                                render_grouped_parameter_list(
                                                    ui,
                                                    &format!("insert_{}_{}", tr_id, ins_idx),
                                                    &ins.params,
                                                    &app.effect_param_search_filter,
                                                    |name, val| {
                                                        app.send_cmd(IpcCommand::SetTrackInsertParam {
                                                            track_id: tr_id.clone(),
                                                            insert_index: ins_idx,
                                                            param_name: name.to_string(),
                                                            value: val,
                                                        });
                                                    },
                                                );
                                            });
                                    }
                                },
                            );

                            if !is_effect_open {
                                app.edit_insert_slot = None;
                            }
                        } else {
                            app.edit_insert_slot = None;
                        }
                    } else {
                        app.edit_insert_slot = None;
                    }
                }
            }
        } else {
            app.edit_track_id = None;
            app.edit_insert_slot = None;
        }
    } else {
        app.edit_insert_slot = None;
    }

    // Master Bus Insert Effect Parameter Inspector Modal
    if let Some(ins_idx) = app.edit_master_insert_slot {
        let master_ins_opt = app.last_status.as_ref().and_then(|s| {
            s.master_inserts.get(ins_idx).cloned()
        });

        if let Some(ins) = master_ins_opt {
            let mut is_effect_open = true;
            let mut stack = SlidingPanelStack::docked_left(44.0);
            stack.panel(
                ctx,
                egui::Id::new("master_effect_inspector_window"),
                format!("⚡ Master Effect: {}", ins.name),
                &mut is_effect_open,
                |ui| {
                    ui.label(format!("Plugin: {}", ins.name));
                    ui.small(format!("URI: {}", ins.uri));
                    ui.separator();

                    if ins.params.is_empty() {
                        ui.label("No exposed control parameters for this plugin.");
                    } else {
                        ui.horizontal(|ui| {
                            ui.label(format!("Parameters ({})", ins.params.len()));
                            if ins.params.len() > 6 {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.add(
                                        SearchInput::new(&mut app.effect_param_search_filter, "🔍 Search parameters...")
                                            .id_salt("master_effect_param_search_input")
                                            .desired_width(180.0)
                                    );
                                });
                            }
                        });
                        ui.add_space(4.0);

                        egui::ScrollArea::vertical()
                            .max_height(350.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                render_grouped_parameter_list(
                                    ui,
                                    &format!("master_insert_{}", ins_idx),
                                    &ins.params,
                                    &app.effect_param_search_filter,
                                    |name, val| {
                                        app.send_cmd(IpcCommand::SetMasterInsertParam {
                                            insert_index: ins_idx,
                                            param_name: name.to_string(),
                                            value: val,
                                        });
                                    },
                                );
                            });
                    }
                },
            );

            if !is_effect_open {
                app.edit_master_insert_slot = None;
            }
        } else {
            app.edit_master_insert_slot = None;
        }
    }
}
