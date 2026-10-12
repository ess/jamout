mod sliding_panel;

use eframe::egui;
use jamseq::ipc::{IpcCommand, IpcResponse, IpcStatusResponse};
use sliding_panel::SlidingPanelStack;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("JamOut - Linux Audio Workstation"),
        ..Default::default()
    };

    eframe::run_native(
        "JamOut",
        native_options,
        Box::new(|cc| Ok(Box::new(JamOutApp::new(cc)))),
    )
}

struct JamOutApp {
    _socket_path: String,
    connected: bool,
    last_status: Option<IpcStatusResponse>,
    cmd_sender: Sender<IpcCommand>,
    status_receiver: Receiver<IpcStatusResponse>,
    
    // UI state
    record_out_filename: String,
    status_msg: String,
    _status_time: Instant,
    active_tab: Tab,
    lyrics: Vec<LyricLine>,

    // Human drift & feel controls
    pub drift_ticks: i64,
    pub drag_bias_ticks: i64,
    pub freehand_drift: bool,

    // Project switching UI state
    pub show_project_dialog: bool,
    pub project_path_input: String,
    pub new_project_title: String,
    pub new_project_bpm: f64,
    // Piano roll state
    pub selected_track_id: String,
    pub piano_roll_note_pitch: u8,

    // Track editing state
    pub track_name_edits: HashMap<String, String>,

    // Live plugin / synth editor modal
    pub edit_track_id: Option<String>,
    pub edit_insert_slot: Option<(String, usize)>, // (track_id, insert_index)
    pub preset_save_name: String,
    pub param_search_filter: String,

    // Note duration and drag drawing state
    pub note_duration_ticks: u64,
    pub drag_start: Option<(u64, u8)>, // (start_tick, pitch)
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Timeline,
    PianoRoll,
    Mixer,
    Teleprompter,
}

struct LyricLine {
    bar: u32,
    text: &'static str,
}

impl JamOutApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Load fallback icon fonts (FontAwesome and Noto Symbols) into egui
        let mut fonts = egui::FontDefinitions::default();
        if let Ok(fa_bytes) = std::fs::read("/usr/share/fonts/opentype/font-awesome/FontAwesome.otf") {
            fonts.font_data.insert(
                "FontAwesome".to_owned(),
                egui::FontData::from_owned(fa_bytes).into(),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .push("FontAwesome".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("FontAwesome".to_owned());
        }
        if let Ok(noto_sym_bytes) = std::fs::read("/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf") {
            fonts.font_data.insert(
                "NotoSymbols2".to_owned(),
                egui::FontData::from_owned(noto_sym_bytes).into(),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .push("NotoSymbols2".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("NotoSymbols2".to_owned());
        }
        if let Ok(liberation_bytes) = std::fs::read("/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf") {
            fonts.font_data.insert(
                "LiberationSans".to_owned(),
                egui::FontData::from_owned(liberation_bytes).into(),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .push("LiberationSans".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("LiberationSans".to_owned());
        }
        cc.egui_ctx.set_fonts(fonts);

        let (cmd_tx, cmd_rx) = channel::<IpcCommand>();
        let (status_tx, status_rx) = channel::<IpcStatusResponse>();

        let socket_path = "/tmp/jamseq.sock".to_string();
        let sock_clone = socket_path.clone();

        // Background worker thread managing IPC socket connection to jamseq
        thread::spawn(move || {
            let mut client: Option<(BufReader<UnixStream>, UnixStream)> = None;
            let mut failed_attempts = 0usize;
            let mut spawned_process = false;

            loop {
                // If not connected, attempt reconnect
                if client.is_none() {
                    if let Ok(s) = UnixStream::connect(&sock_clone) {
                        let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
                        let _ = s.set_write_timeout(Some(Duration::from_millis(200)));
                        if let Ok(read_clone) = s.try_clone() {
                            client = Some((BufReader::new(read_clone), s));
                            failed_attempts = 0;
                        }
                    } else {
                        failed_attempts += 1;
                        // If jamseq is not running after a few attempts, attempt to spawn it automatically in the background
                        if failed_attempts >= 3 && !spawned_process {
                            spawned_process = true;
                            // Check if `jamseq` binary exists in current directory or target/release or PATH
                            let possible_bins = [
                                "jamseq",
                                "./target/release/jamseq",
                                "./target/debug/jamseq",
                                "../target/release/jamseq",
                                "../target/debug/jamseq",
                            ];
                            for bin in &possible_bins {
                                if let Ok(_) = std::process::Command::new(bin)
                                    .arg("server")
                                    .stdin(std::process::Stdio::null())
                                    .stdout(std::process::Stdio::null())
                                    .stderr(std::process::Stdio::null())
                                    .spawn()
                                {
                                    eprintln!("[jamout] Auto-launched '{} server' daemon", bin);
                                    break;
                                }
                            }
                        }
                        thread::sleep(Duration::from_millis(300));
                        continue;
                    }
                }

                if client.is_none() {
                    thread::sleep(Duration::from_millis(300));
                    continue;
                }

                let (reader, writer) = client.as_mut().unwrap();

                // 1. Process all pending commands from UI
                let mut connection_failed = false;
                while let Ok(cmd) = cmd_rx.try_recv() {
                    if let Ok(cmd_json) = serde_json::to_string(&cmd) {
                        if writeln!(writer, "{}", cmd_json).is_err() || writer.flush().is_err() {
                            connection_failed = true;
                            break;
                        }
                        // Each command returns an immediate status line
                        let mut resp_line = String::new();
                        if reader.read_line(&mut resp_line).is_ok() && !resp_line.trim().is_empty() {
                            if let Ok(resp) = serde_json::from_str::<IpcResponse>(&resp_line) {
                                if let Some(st) = resp.status {
                                    let _ = status_tx.send(st);
                                }
                            }
                        }
                    }
                }

                if connection_failed {
                    client = None;
                    continue;
                }

                // 2. Poll status at ~30 Hz
                let get_status = serde_json::to_string(&IpcCommand::GetStatus).unwrap();
                if writeln!(writer, "{}", get_status).is_ok() && writer.flush().is_ok() {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(n) if n > 0 => {
                            if let Ok(resp) = serde_json::from_str::<IpcResponse>(&line) {
                                if let Some(st) = resp.status {
                                    let _ = status_tx.send(st);
                                }
                            }
                        }
                        Ok(_) => {
                            // Socket closed (EOF)
                            client = None;
                            continue;
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                            // Non-fatal read timeout, loop again
                        }
                        Err(_) => {
                            client = None;
                            continue;
                        }
                    }
                } else {
                    client = None;
                    continue;
                }

                thread::sleep(Duration::from_millis(33)); // 30 FPS updates
            }
        });

        let lyrics = vec![
            LyricLine { bar: 1, text: "(Intro 808 glide & gothic choir fade-in)" },
            LyricLine { bar: 5, text: "Ash in the basin, frost on the sill..." },
            LyricLine { bar: 7, text: "A breath caught in bronze when the bells fell still." },
            LyricLine { bar: 9, text: "I did not ask the dark to spare the flame..." },
            LyricLine { bar: 11, text: "Only to teach the iron how to speak my name." },
        ];

        Self {
            _socket_path: socket_path,
            connected: false,
            last_status: None,
            cmd_sender: cmd_tx,
            status_receiver: status_rx,
            record_out_filename: "takes/vocal_verse1_take1.wav".to_string(),
            status_msg: "Ready. Connect jamseq via /tmp/jamseq.sock".to_string(),
            _status_time: Instant::now(),
            active_tab: Tab::Timeline,
            lyrics,
            drift_ticks: 20,       // ±20 ticks Gaussian feel
            drag_bias_ticks: 15,   // +15 ticks lazy 90s R&B drag
            freehand_drift: false,
            show_project_dialog: false,
            project_path_input: "doc/songs/the_white_flame.json".to_string(),
            new_project_title: "My New Jam".to_string(),
            new_project_bpm: 68.0,
            selected_track_id: "bass".to_string(),
            piano_roll_note_pitch: 50, // D3 default
            track_name_edits: HashMap::new(),
            edit_track_id: None,
            edit_insert_slot: None,
            preset_save_name: "custom_patch.json".to_string(),
            param_search_filter: String::new(),
            note_duration_ticks: 480, // 1/8 note default
            drag_start: None,
        }
    }

    fn send_cmd(&self, cmd: IpcCommand) {
        let _ = self.cmd_sender.send(cmd);
    }
}

impl eframe::App for JamOutApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain incoming IPC status
        while let Ok(status) = self.status_receiver.try_recv() {
            for t in &status.tracks {
                self.track_name_edits.entry(t.id.clone()).or_insert_with(|| t.name.clone());
            }
            self.last_status = Some(status);
            self.connected = true;
        }

        // Request continuous repaint for smooth VU meters & playhead
        ctx.request_repaint_after(Duration::from_millis(33));

        // Top Control Bar (Transport, BPM, Recording, Tab Selectors)
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("🎛 JamOut");
                ui.separator();

                // Connection badge
                if self.connected {
                    ui.colored_label(egui::Color32::from_rgb(80, 220, 100), "● Connected");
                } else {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), "○ jamseq offline");
                }

                ui.separator();

                // Project Switcher Button & Title
                let proj_title = self.last_status.as_ref().map(|s| s.project_title.as_str()).unwrap_or("No Project");
                if ui.button(format!("📁 Project: {}", proj_title)).clicked() {
                    self.show_project_dialog = !self.show_project_dialog;
                }

                ui.separator();

                let is_playing = self.last_status.as_ref().map(|s| s.is_playing).unwrap_or(false);
                let is_recording = self.last_status.as_ref().map(|s| s.is_recording).unwrap_or(false);
                let current_tick = self.last_status.as_ref().map(|s| s.current_tick).unwrap_or(0);
                let _total_ticks = self.last_status.as_ref().map(|s| s.total_ticks).unwrap_or(15360);
                let bpm = self.last_status.as_ref().map(|s| s.bpm).unwrap_or(68.0);

                // Play / Pause
                if is_playing {
                    if ui.button("⏸ Pause").clicked() {
                        self.send_cmd(IpcCommand::Pause);
                    }
                } else {
                    if ui.button("▶ Play").clicked() {
                        self.send_cmd(IpcCommand::Play);
                    }
                }

                // Stop / Rewind
                if ui.button("⏹ Stop").clicked() {
                    self.send_cmd(IpcCommand::Pause);
                    self.send_cmd(IpcCommand::Seek { tick: 0 });
                }

                // Record Button
                if is_recording {
                    if ui.button(egui::RichText::new("⏹ Stop Rec").color(egui::Color32::RED)).clicked() {
                        self.send_cmd(IpcCommand::StopRecording);
                        self.status_msg = "Saved take and added new Audio Track to arrangement!".to_string();
                        // Advance filename for next take: take1 -> take2, etc.
                        let mut counter = 2;
                        while std::path::Path::new(&format!("recordings/vocal_take_{}.wav", counter)).exists() {
                            counter += 1;
                        }
                        self.record_out_filename = format!("recordings/vocal_take_{}.wav", counter);
                    }
                } else {
                    if ui.button(egui::RichText::new("⏺ Record Mic").color(egui::Color32::from_rgb(255, 90, 90))).clicked() {
                        self.send_cmd(IpcCommand::StartRecording {
                            out_path: self.record_out_filename.clone(),
                        });
                        self.send_cmd(IpcCommand::Play);
                        self.status_msg = format!("Recording live take to: {}", self.record_out_filename);
                    }
                }

                ui.separator();

                // Tempo, Time Signature, and Bar/Beat display
                let current_bar = (current_tick / (4 * 960)) + 1;
                let current_beat = ((current_tick % (4 * 960)) / 960) + 1;
                let active_meter = self.last_status.as_ref()
                    .and_then(|s| {
                        let cur_id = s.active_section_id.as_deref();
                        s.sections.iter().find(|sec| cur_id == Some(&sec.id))
                            .map(|sec| format!("{}/{}", sec.time_signature.numerator, sec.time_signature.denominator))
                    })
                    .unwrap_or_else(|| "4/4".to_string());
                ui.monospace(format!("Bar {:02} : {:02} | {} | {:.1} BPM", current_bar, current_beat, active_meter, bpm));

                ui.separator();

                // Live Mic Input Meter
                let mic_peak = self.last_status.as_ref().map(|s| s.mic_peak).unwrap_or(0.0);
                ui.label("Mic:");
                let progress_bar = egui::ProgressBar::new(mic_peak.min(1.0))
                    .desired_width(70.0)
                    .show_percentage();
                ui.add(progress_bar);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.selectable_value(&mut self.active_tab, Tab::Teleprompter, "🎙 Lyrics");
                    ui.selectable_value(&mut self.active_tab, Tab::Mixer, "🎚 Mixer");
                    ui.selectable_value(&mut self.active_tab, Tab::PianoRoll, "🎹 Piano Roll");
                    ui.selectable_value(&mut self.active_tab, Tab::Timeline, "📊 Timeline");
                });
            });
            ui.add_space(4.0);
        });

        // Bottom Status Bar
        egui::TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.small(&self.status_msg);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.small("Native Linux FOSS Audio Workstation | PipeWire ALSA");
                });
            });
        });

        // Project Switcher Modal Dialog
        if self.show_project_dialog {
            egui::Window::new("📁 Project Manager")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.set_width(450.0);
                    ui.heading("Open or Save Project");
                    ui.add_space(8.0);

                    let current_path = self.last_status.as_ref().and_then(|s| s.project_path.clone()).unwrap_or_else(|| "(none)".to_string());
                    ui.label(format!("Active file: {}", current_path));
                    ui.add_space(6.0);

                    ui.label("Project File Path (.json):");
                    ui.text_edit_singleline(&mut self.project_path_input);

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("📂 Open / Switch").clicked() {
                            self.send_cmd(IpcCommand::LoadProject {
                                path: self.project_path_input.clone(),
                            });
                            self.status_msg = format!("Loading project: {}", self.project_path_input);
                            self.show_project_dialog = false;
                        }

                        if ui.button("💾 Save Project").clicked() {
                            self.send_cmd(IpcCommand::SaveProject {
                                path: self.project_path_input.clone(),
                            });
                            self.status_msg = format!("Saved project to: {}", self.project_path_input);
                            self.show_project_dialog = false;
                        }

                        if ui.button("Close").clicked() {
                            self.show_project_dialog = false;
                        }
                    });

                    ui.add_space(8.0);
                    ui.separator();
                    ui.heading("✨ Create New Project");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Title:");
                        ui.text_edit_singleline(&mut self.new_project_title);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Tempo:");
                        ui.add(egui::Slider::new(&mut self.new_project_bpm, 40.0..=220.0).text("BPM"));
                    });
                    ui.add_space(6.0);
                    if ui.button("➕ Create Fresh Project").clicked() {
                        self.send_cmd(IpcCommand::NewProject {
                            title: self.new_project_title.clone(),
                            bpm: self.new_project_bpm,
                        });
                        self.status_msg = format!("Created new project: '{}' @ {:.1} BPM", self.new_project_title, self.new_project_bpm);
                        self.show_project_dialog = false;
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.label("Quick Switch Known Songs:");
                    if ui.button("• The White Flame (Verse 1)").clicked() {
                        self.project_path_input = "doc/songs/the_white_flame.json".to_string();
                    }
                });
        }

        // Central Workspace Area
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                Tab::Timeline => self.render_timeline_view(ui),
                Tab::PianoRoll => self.render_piano_roll_view(ui),
                Tab::Mixer => self.render_mixer_view(ui),
                Tab::Teleprompter => self.render_teleprompter_view(ui),
            }
        });

        // Live Plugin / Synth Fiddling & Parameter Inspector Modal
        if let Some(ref track_id) = self.edit_track_id.clone() {
            let track_opt = self.last_status.as_ref().and_then(|s| {
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
                                let current_buf = self.track_name_edits.entry(track.id.clone()).or_insert_with(|| track.name.clone());
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
                                self.send_cmd(IpcCommand::RenameTrack {
                                    track_id: track.id.clone(),
                                    new_name: new_name_str.clone(),
                                });
                                self.status_msg = format!("Renamed track to '{}'", new_name_str);
                            }
                        });
                        ui.small(format!("Track ID: {} | Live DSP Control", track.id));
                        ui.separator();

                        // Dynamic Plugin / Instrument Switcher
                        ui.horizontal(|ui| {
                            ui.label("Active Instrument:");
                            let current_plug = track.current_plugin.as_deref().unwrap_or("None");

                            egui::ComboBox::from_id_salt(format!("plugin_sel_{}", track.id))
                                .selected_text(current_plug)
                                .show_ui(ui, |ui| {
                                    let available = self.last_status.as_ref()
                                        .map(|s| s.available_plugins.clone())
                                        .unwrap_or_default();

                                    if !available.is_empty() {
                                        ui.label("--- Available Plugins (CLAP / LV2) ---");
                                        for plug in &available {
                                            let display = format!("{} ({})", plug.name, plug.plugin_format);
                                            let is_selected = current_plug == display;
                                            if ui.selectable_label(is_selected, &display).clicked() {
                                                self.send_cmd(IpcCommand::SwitchTrackPlugin {
                                                    track_id: track.id.clone(),
                                                    plugin_uri_or_type: plug.uri_or_path.clone(),
                                                    plugin_name: display,
                                                });
                                            }
                                        }
                                    } else {
                                        ui.label("(No system LV2/CLAP plugins found)");
                                    }

                                    ui.separator();
                                    if ui.button("🔄 Rescan Plugins").clicked() {
                                        self.send_cmd(IpcCommand::RescanPlugins);
                                        self.status_msg = "Rescanning system LV2 and CLAP plugins...".to_string();
                                    }
                                });

                            if ui.button("🔄").on_hover_text("Rescan system for newly installed LV2 and CLAP plugins").clicked() {
                                self.send_cmd(IpcCommand::RescanPlugins);
                                self.status_msg = "Rescanning system LV2 and CLAP plugins...".to_string();
                            }
                        });
                        ui.separator();

                        if track.params.is_empty() {
                            ui.label("No tweakable DSP parameters exposed for this track (e.g. raw audio clip).");
                        } else {
                            ui.horizontal(|ui| {
                                ui.label(format!("Live Parameters ({})", track.params.len()));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if !self.param_search_filter.is_empty() && ui.small_button("Clear").clicked() {
                                        self.param_search_filter.clear();
                                    }
                                    ui.add(egui::TextEdit::singleline(&mut self.param_search_filter).hint_text("🔍 Search parameters..."));
                                });
                            });
                            ui.add_space(4.0);

                            let filter_lower = self.param_search_filter.trim().to_lowercase();
                            let filtered_params: Vec<_> = track
                                .params
                                .iter()
                                .filter(|p| {
                                    if filter_lower.is_empty() {
                                        true
                                    } else {
                                        p.name.to_lowercase().contains(&filter_lower)
                                    }
                                })
                                .collect();

                            egui::ScrollArea::vertical()
                                .max_height(250.0)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    if filtered_params.is_empty() {
                                        ui.label("No parameters match the search filter.");
                                    } else {
                                        for param in filtered_params {
                                            let mut val = param.value;
                                            ui.horizontal(|ui| {
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
                                                ui.set_min_width(120.0);
                                                ui.label(label);
                                                if ui.add(egui::Slider::new(&mut val, param.min..=param.max)).changed() {
                                                    self.send_cmd(IpcCommand::SetTrackParam {
                                                        track_id: track.id.clone(),
                                                        param_name: param.name.clone(),
                                                        value: val,
                                                    });
                                                }
                                            });
                                        }
                                    }
                                });

                            ui.add_space(8.0);
                            ui.separator();
                            ui.label("💾 Preset Management:");
                            if track.current_plugin.as_deref().map(|p| p.to_lowercase().contains("jamsynth")).unwrap_or(false) {
                                ui.horizontal(|ui| {
                                    ui.label("Quick Presets:");
                                    if ui.button("808 Sub").clicked() {
                                        self.send_cmd(IpcCommand::LoadTrackPreset {
                                            track_id: track.id.clone(),
                                            preset_name: "808_sub.json".to_string(),
                                        });
                                        self.status_msg = "Loaded 808 Sub preset".to_string();
                                    }
                                    if ui.button("Dark Reese").clicked() {
                                        self.send_cmd(IpcCommand::LoadTrackPreset {
                                            track_id: track.id.clone(),
                                            preset_name: "dark_reese.json".to_string(),
                                        });
                                        self.status_msg = "Loaded Dark Reese preset".to_string();
                                    }
                                    if ui.button("Cold Pad").clicked() {
                                        self.send_cmd(IpcCommand::LoadTrackPreset {
                                            track_id: track.id.clone(),
                                            preset_name: "cold_gothic_pad.json".to_string(),
                                        });
                                        self.status_msg = "Loaded Cold Pad preset".to_string();
                                    }
                                });
                            }
                            ui.horizontal(|ui| {
                                ui.label("File:");
                                ui.text_edit_singleline(&mut self.preset_save_name);
                            });
                            ui.horizontal(|ui| {
                                if ui.button("💾 Save Preset As...").clicked() {
                                    self.send_cmd(IpcCommand::SaveTrackPreset {
                                        track_id: track.id.clone(),
                                        preset_name: self.preset_save_name.clone(),
                                    });
                                    self.status_msg = format!("Saved preset to presets/{}", self.preset_save_name);
                                }
                                if ui.button("📂 Load Preset").clicked() {
                                    self.send_cmd(IpcCommand::LoadTrackPreset {
                                        track_id: track.id.clone(),
                                        preset_name: self.preset_save_name.clone(),
                                    });
                                    self.status_msg = format!("Loaded preset from {}", self.preset_save_name);
                                }
                            });

                            ui.add_space(8.0);
                            ui.separator();

                            // --- INSERT EFFECTS CHAIN SECTION ---
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("⚡ Insert Effects Chain").strong());

                                egui::ComboBox::from_id_salt(format!("add_insert_sel_{}", track.id))
                                    .selected_text("➕ Add Effect...")
                                    .show_ui(ui, |ui| {
                                        let available = self.last_status.as_ref()
                                            .map(|s| s.available_plugins.clone())
                                            .unwrap_or_default();

                                        if !available.is_empty() {
                                            for plug in &available {
                                                let display = format!("{} ({})", plug.name, plug.plugin_format);
                                                if ui.button(&display).clicked() {
                                                    self.send_cmd(IpcCommand::AddTrackInsert {
                                                        track_id: track.id.clone(),
                                                        plugin_uri_or_path: plug.uri_or_path.clone(),
                                                        plugin_name: display.clone(),
                                                    });
                                                    self.status_msg = format!("Added insert effect '{}'", display);
                                                }
                                            }
                                        } else {
                                            ui.label("(No system plugins available)");
                                        }
                                    });
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
                                                    self.send_cmd(IpcCommand::RemoveTrackInsert {
                                                        track_id: track.id.clone(),
                                                        insert_index: ins_idx,
                                                    });
                                                    self.status_msg = format!("Removed insert effect '{}'", ins.name);
                                                    if self.edit_insert_slot == Some((track.id.clone(), ins_idx)) {
                                                        self.edit_insert_slot = None;
                                                    }
                                                }
                                                let is_effect_active = self.edit_insert_slot == Some((track.id.clone(), ins_idx));
                                                let edit_btn = if is_effect_active {
                                                    egui::Button::new("🎛").fill(egui::Color32::from_rgb(50, 90, 150))
                                                } else {
                                                    egui::Button::new("🎛")
                                                };
                                                if ui.add(edit_btn).on_hover_text("Open Effect Controls in slide-out panel").clicked() {
                                                    if is_effect_active {
                                                        self.edit_insert_slot = None;
                                                    } else {
                                                        self.edit_insert_slot = Some((track.id.clone(), ins_idx));
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
                    self.edit_track_id = None;
                    self.edit_insert_slot = None;
                } else if let Some(parent_rect) = inst_rect {
                    // Child sliding panel stack docked directly to parent instrument panel
                    if let Some((ref tr_id, ins_idx)) = self.edit_insert_slot.clone() {
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
                                            ui.label(format!("Parameters ({})", ins.params.len()));
                                            ui.add_space(4.0);

                                            egui::ScrollArea::vertical()
                                                .max_height(350.0)
                                                .auto_shrink([false, false])
                                                .show(ui, |ui| {
                                                    for p in &ins.params {
                                                        let mut val = p.value;
                                                        ui.horizontal(|ui| {
                                                            ui.set_min_width(120.0);
                                                            ui.label(&p.name);
                                                            if ui.add(egui::Slider::new(&mut val, p.min..=p.max)).changed() {
                                                                self.send_cmd(IpcCommand::SetTrackParam {
                                                                    track_id: tr_id.clone(),
                                                                    param_name: p.name.clone(),
                                                                    value: val,
                                                                });
                                                            }
                                                        });
                                                    }
                                                });
                                        }
                                    },
                                );

                                if !is_effect_open {
                                    self.edit_insert_slot = None;
                                }
                            } else {
                                self.edit_insert_slot = None;
                            }
                        } else {
                            self.edit_insert_slot = None;
                        }
                    }
                }
            } else {
                self.edit_track_id = None;
                self.edit_insert_slot = None;
            }
        } else {
            self.edit_insert_slot = None;
        }
    }
}

impl JamOutApp {
    fn render_timeline_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Arrangement Timeline");
        ui.add_space(8.0);

        let total_ticks = self.last_status.as_ref().map(|s| s.total_ticks).unwrap_or(15360);
        let current_tick = self.last_status.as_ref().map(|s| s.current_tick).unwrap_or(0);
        let active_section_id = self.last_status.as_ref().and_then(|s| s.active_section_id.clone());
        let sections = self.last_status.as_ref().map(|s| s.sections.clone()).unwrap_or_default();

        // Timeline scrubber & active section indicator
        ui.horizontal(|ui| {
            ui.label("Playhead:");
            let mut seek_tick = current_tick;
            if ui.add(egui::Slider::new(&mut seek_tick, 0..=total_ticks).text("Ticks")).changed() {
                self.send_cmd(IpcCommand::Seek { tick: seek_tick });
            }

            if let Some(sec_id) = &active_section_id {
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(255, 215, 0), format!("📍 Section: {}", sec_id));
            }
        });

        ui.add_space(6.0);

        // --- SECTION MARKER LANE ---
        ui.label(egui::RichText::new("Song Sections (Orchestration Scope):").small());
        ui.horizontal(|ui| {
            for sec in &sections {
                let is_current = active_section_id.as_deref() == Some(&sec.id);
                let btn_text = format!("{} (Bar {}-{})", sec.name, sec.start_bar, sec.start_bar + sec.bars - 1);
                let btn = if is_current {
                    egui::Button::new(egui::RichText::new(btn_text).strong().color(egui::Color32::BLACK))
                        .fill(egui::Color32::from_rgb(255, 215, 80))
                } else {
                    egui::Button::new(btn_text)
                        .fill(egui::Color32::from_rgb(50, 60, 75))
                };

                if ui.add(btn).clicked() {
                    // Click section to jump playhead to its downbeat
                    self.send_cmd(IpcCommand::Seek { tick: sec.start_tick() });
                }
            }
        });

        ui.add_space(8.0);

        // --- HUMAN DRIFT & GROOVE TOOLBAR ---
        ui.horizontal(|ui| {
            ui.label("Groove Feel:");
            ui.checkbox(&mut self.freehand_drift, "Freehand Micro-Timing (Shift-bypass snap)");

            ui.separator();
            ui.label("Drift:");
            ui.add(egui::Slider::new(&mut self.drift_ticks, 0..=80).text("±Ticks"));

            ui.separator();
            ui.label("Drag (Late):");
            ui.add(egui::Slider::new(&mut self.drag_bias_ticks, -20..=60).text("Ticks"));

            ui.separator();
            ui.label("Note Length:");
            let durations = [
                (240u64, "1/16"),
                (480u64, "1/8"),
                (960u64, "1/4"),
                (1920u64, "1/2"),
                (3840u64, "1 Bar"),
            ];
            for (dur, label) in durations {
                if ui.selectable_label(self.note_duration_ticks == dur, label).clicked() {
                    self.note_duration_ticks = dur;
                }
            }

            ui.separator();
            if ui.button("🎲 Humanize Selected Section").clicked() {
                let sec_id = active_section_id.clone();
                // Send humanize to all tracks for the current section
                let tracks_clone = self.last_status.as_ref().map(|s| s.tracks.clone()).unwrap_or_default();
                for t in tracks_clone {
                    self.send_cmd(IpcCommand::HumanizeTrack {
                        track_id: t.id.clone(),
                        section_id: sec_id.clone(),
                        timing_drift_ticks: self.drift_ticks,
                        drag_bias_ticks: self.drag_bias_ticks,
                        velocity_jitter: 0.08,
                    });
                }
                self.status_msg = format!("Humanized active section with ±{} drift and +{} drag", self.drift_ticks, self.drag_bias_ticks);
            }
        });

        ui.add_space(8.0);

        // Track Lanes
        let tracks = self.last_status.as_ref().map(|s| s.tracks.clone()).unwrap_or_default();
        let events = self.last_status.as_ref().map(|s| s.events.clone()).unwrap_or_default();

        egui::ScrollArea::vertical().show(ui, |ui| {
            for (idx, track) in tracks.iter().enumerate() {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.set_min_height(48.0);
                        // Strict fixed-width Track Header (220px) so lanes never get offset by label length
                        ui.allocate_ui(egui::vec2(220.0, 42.0), |ui| {
                            ui.set_width(220.0);
                            ui.set_max_width(220.0);
                            ui.vertical(|ui| {
                                let (need_rename, new_name_str) = {
                                    let current_buf = self.track_name_edits.entry(track.id.clone()).or_insert_with(|| track.name.clone());
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
                                    self.send_cmd(IpcCommand::RenameTrack {
                                        track_id: track.id.clone(),
                                        new_name: new_name_str.clone(),
                                    });
                                    self.status_msg = format!("Renamed track to '{}'", new_name_str);
                                }
                                ui.horizontal(|ui| {
                                    ui.small(&format!("ID: {}", track.id));
                                    if ui.small_button("🎹").on_hover_text("Open Piano Roll").clicked() {
                                        self.selected_track_id = track.id.clone();
                                        self.active_tab = Tab::PianoRoll;
                                    }
                                    if ui.small_button("🎛").on_hover_text("Tweak Instruments & DSP Parameters").clicked() {
                                        if self.edit_track_id.as_deref() != Some(&track.id) {
                                            self.edit_insert_slot = None;
                                        }
                                        self.edit_track_id = Some(track.id.clone());
                                    }
                                    if idx > 0 && ui.small_button("▲").on_hover_text("Move track up").clicked() {
                                        self.send_cmd(IpcCommand::ReorderTrack { from: idx, to: idx - 1 });
                                    }
                                    if idx + 1 < tracks.len() && ui.small_button("▼").on_hover_text("Move track down").clicked() {
                                        self.send_cmd(IpcCommand::ReorderTrack { from: idx, to: idx + 1 });
                                    }
                                    if ui.small_button("🗑").on_hover_text("Delete track").clicked() {
                                        self.send_cmd(IpcCommand::DeleteTrack { track_id: track.id.clone() });
                                        self.status_msg = format!("Deleted track '{}'", track.name);
                                    }
                                });
                            });
                        });

                        ui.separator();

                        // Interactive Clip/Note Lane Canvas
                        let lane_width = (ui.available_width() - 40.0).max(100.0);
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(lane_width, 42.0),
                            egui::Sense::click(),
                        );

                        // Click to interactively add/delete note at clicked tick!
                        if response.clicked() {
                            if let Some(pos) = response.interact_pointer_pos() {
                                let pct = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                                let clicked_tick = (pct * total_ticks as f32) as u64;

                                // If Shift is held or Freehand is enabled, place with exact micro-timing resolution!
                                let shift_held = ui.input(|i| i.modifiers.shift);
                                let final_tick = if shift_held || self.freehand_drift {
                                    // Exact pixel/tick resolution + drag bias
                                    (clicked_tick as i64 + self.drag_bias_ticks).max(0) as u64
                                } else {
                                    // Snapped to 16th note grid (240 ticks) + subtle drag bias
                                    let snapped = (clicked_tick / 240) * 240;
                                    (snapped as i64 + self.drag_bias_ticks).max(0) as u64
                                };

                                let note_pitch = if idx == 0 { 50 } else { 62 }; // D3 bass, D4 pad
                                let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

                                // Send granular AddNote command to jamseq
                                self.send_cmd(IpcCommand::AddNote {
                                    note_on_id: now_ms,
                                    note_off_id: now_ms + 1,
                                    track_id: track.id.clone(),
                                    tick: final_tick,
                                    duration_ticks: self.note_duration_ticks,
                                    note: note_pitch,
                                    velocity: 0.9,
                                });
                                let mode_str = if shift_held || self.freehand_drift { "Freehand micro-timed" } else { "Grid-snapped" };
                                self.status_msg = format!("{} note on {} at tick {} (length: {} ticks)", mode_str, track.name, final_tick, self.note_duration_ticks);
                            }
                        } else if response.secondary_clicked() {
                            // Right click deletes note at position
                            if let Some(pos) = response.interact_pointer_pos() {
                                let pct = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                                let clicked_tick = (pct * total_ticks as f32) as u64;
                                let note_pitch = if idx == 0 { 50 } else { 62 };
                                self.send_cmd(IpcCommand::DeleteNoteAt {
                                    track_id: track.id.clone(),
                                    tick: clicked_tick,
                                    note: note_pitch,
                                    });
                                self.status_msg = format!("Deleted note on {} near tick {}", track.name, clicked_tick);
                            }
                        }

                        let painter = ui.painter();
                        let bg_color = if idx == 0 {
                            egui::Color32::from_rgb(30, 45, 65) // Dark Bass
                        } else if idx == 1 {
                            egui::Color32::from_rgb(45, 30, 60) // Dark Pad
                        } else {
                            egui::Color32::from_rgb(35, 55, 45)  // Dark Vocals / Audio
                        };

                        painter.rect_filled(rect, 4.0, bg_color);

                        // Draw subtle grid subdivisions (bars)
                        let num_bars = (total_ticks / (4 * 960)).max(1) as usize;
                        for bar in 1..=num_bars {
                            let bar_pct = (bar as f32 - 1.0) / num_bars as f32;
                            let bar_x = rect.left() + rect.width() * bar_pct;
                            painter.line_segment(
                                [egui::pos2(bar_x, rect.top()), egui::pos2(bar_x, rect.bottom())],
                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 30)),
                            );
                        }

                        // --- DRAW ACTUAL TRUE DURATION NOTE BLOCKS ON TRACK LANE ---
                        let track_events: Vec<&jamseq::ScheduledEvent> = events
                            .iter()
                            .filter(|e| e.track_id == track.id)
                            .collect();

                        for ev in &track_events {
                            if let jamseq::timeline::ScheduledAction::NoteOn { note, velocity: _ } = ev.action {
                                if total_ticks > 0 {
                                    // Calculate true duration by finding the matching NoteOff event
                                    let note_off_tick = track_events
                                        .iter()
                                        .find(|off_ev| {
                                            off_ev.tick > ev.tick
                                                && match off_ev.action {
                                                    jamseq::timeline::ScheduledAction::NoteOff { note: off_n } => off_n == note,
                                                    _ => false,
                                                }
                                        })
                                        .map(|off_ev| off_ev.tick)
                                        .unwrap_or(ev.tick + self.note_duration_ticks);

                                    let duration = note_off_tick.saturating_sub(ev.tick).max(60);

                                    let note_pct = (ev.tick as f32 / total_ticks as f32).clamp(0.0, 1.0);
                                    let note_x = rect.left() + rect.width() * note_pct;
                                    let note_w = (rect.width() * (duration as f32 / total_ticks as f32)).max(4.0);
                                    let note_rect = egui::Rect::from_min_size(
                                        egui::pos2(note_x, rect.top() + 6.0),
                                        egui::vec2(note_w, rect.height() - 12.0),
                                    );
                                    let note_color = if idx == 0 {
                                        egui::Color32::from_rgb(80, 160, 240) // Cyan/Blue 808
                                    } else if idx == 1 {
                                        egui::Color32::from_rgb(180, 120, 240) // Lavender Pad
                                    } else {
                                        egui::Color32::from_rgb(120, 220, 140) // Green
                                    };
                                    painter.rect_filled(note_rect, 2.0, note_color);
                                }
                            }
                        }

                        // Draw playhead cursor across lane
                        if total_ticks > 0 {
                            let playhead_pct = (current_tick as f32 / total_ticks as f32).clamp(0.0, 1.0);
                            let playhead_x = rect.left() + rect.width() * playhead_pct;
                            painter.line_segment(
                                [egui::pos2(playhead_x, rect.top()), egui::pos2(playhead_x, rect.bottom())],
                                egui::Stroke::new(2.5, egui::Color32::WHITE),
                            );
                        }

                        // Mute button
                        let mut muted = track.muted;
                        if ui.toggle_value(&mut muted, "M").clicked() {
                            self.send_cmd(IpcCommand::SetMute {
                                track_id: track.id.clone(),
                                muted,
                            });
                        }
                    });
                });
                ui.add_space(4.0);
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("➕ MIDI Track").clicked() {
                    let count = tracks.len() + 1;
                    let id = format!("midi_{}", count);
                    let name = format!("MIDI Track {}", count);
                    self.send_cmd(IpcCommand::AddTrack {
                        id: id.clone(),
                        name: name.clone(),
                        track_type: "synth".to_string(),
                    });
                    self.track_name_edits.insert(id, name.clone());
                    self.status_msg = format!("Created {}", name);
                }
                if ui.button("➕ Audio Track").clicked() {
                    let count = tracks.len() + 1;
                    let id = format!("audio_{}", count);
                    let name = format!("Audio Track {}", count);
                    self.send_cmd(IpcCommand::AddTrack {
                        id: id.clone(),
                        name: name.clone(),
                        track_type: "audio".to_string(),
                    });
                    self.track_name_edits.insert(id, name.clone());
                    self.status_msg = format!("Created {}", name);
                }
            });
        });
    }

    fn render_mixer_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Console Mixer");
        ui.add_space(8.0);

        let tracks = self.last_status.as_ref().map(|s| s.tracks.clone()).unwrap_or_default();
        let master_vol = self.last_status.as_ref().map(|s| s.master_volume).unwrap_or(0.9);
        let master_peak = self.last_status.as_ref().map(|s| s.master_peak).unwrap_or(0.0);

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
                        self.send_cmd(IpcCommand::SetMasterVolume { volume: m_vol });
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
                                    let current_buf = self.track_name_edits.entry(track.id.clone()).or_insert_with(|| track.name.clone());
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
                                    self.send_cmd(IpcCommand::RenameTrack {
                                        track_id: track.id.clone(),
                                        new_name: new_name_str.clone(),
                                    });
                                    self.status_msg = format!("Renamed track to '{}'", new_name_str);
                                }
                                ui.horizontal(|ui| {
                                    ui.small(&format!("ID: {}", track.id));
                                    if ui.small_button("🎛").on_hover_text("Open DSP / Synth Parameter Controls").clicked() {
                                        if self.edit_track_id.as_deref() != Some(&track.id) {
                                            self.edit_insert_slot = None;
                                        }
                                        self.edit_track_id = Some(track.id.clone());
                                    }
                                    if idx > 0 && ui.small_button("▲").on_hover_text("Move track up").clicked() {
                                        self.send_cmd(IpcCommand::ReorderTrack { from: idx, to: idx - 1 });
                                    }
                                    if idx + 1 < tracks.len() && ui.small_button("▼").on_hover_text("Move track down").clicked() {
                                        self.send_cmd(IpcCommand::ReorderTrack { from: idx, to: idx + 1 });
                                    }
                                    if ui.small_button("🗑").on_hover_text("Delete track").clicked() {
                                        self.send_cmd(IpcCommand::DeleteTrack { track_id: track.id.clone() });
                                        self.status_msg = format!("Deleted track '{}'", track.name);
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
                                    self.send_cmd(IpcCommand::SetMute {
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
                                    self.send_cmd(IpcCommand::SetSolo {
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
                                    self.send_cmd(IpcCommand::SetPan {
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
                                self.send_cmd(IpcCommand::SetVolume {
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

    fn render_teleprompter_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Lyric Teleprompter (Synced Recording Mode)");
        ui.add_space(8.0);

        let current_tick = self.last_status.as_ref().map(|s| s.current_tick).unwrap_or(0);
        let current_bar = (current_tick / (4 * 960)) + 1;

        ui.label("Synchronized vocal recording prompter. Highlights current bar during take:");
        ui.add_space(12.0);

        for line in &self.lyrics {
            let is_active = (current_bar as i32 - line.bar as i32).abs() < 2;
            let text_format = if is_active {
                egui::RichText::new(format!("Bar {:02}:  {}", line.bar, line.text))
                    .size(24.0)
                    .strong()
                    .color(egui::Color32::from_rgb(255, 230, 100))
            } else {
                egui::RichText::new(format!("Bar {:02}:  {}", line.bar, line.text))
                    .size(18.0)
                    .color(egui::Color32::GRAY)
            };

            ui.label(text_format);
            ui.add_space(6.0);
        }

        ui.add_space(20.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Save Take To:");
            ui.text_edit_singleline(&mut self.record_out_filename);
        });
    }

    fn render_piano_roll_view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("🎹 Interactive Piano Roll");
            ui.separator();

            // Track Selector
            let tracks = self.last_status.as_ref().map(|s| s.tracks.clone()).unwrap_or_default();
            ui.label("Editing Track:");
            for t in &tracks {
                let is_sel = self.selected_track_id == t.id;
                if ui.selectable_label(is_sel, &t.name).clicked() {
                    self.selected_track_id = t.id.clone();
                }
            }

            ui.separator();
            ui.label("Length:");
            let durations = [
                (240u64, "1/16"),
                (480u64, "1/8"),
                (960u64, "1/4"),
                (1920u64, "1/2"),
                (3840u64, "1 Bar"),
            ];
            for (dur, label) in durations {
                if ui.selectable_label(self.note_duration_ticks == dur, label).clicked() {
                    self.note_duration_ticks = dur;
                }
            }

            ui.separator();
            ui.label("Pitch:");
            let note_names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
            let octave = (self.piano_roll_note_pitch / 12) as i32 - 1;
            let note_idx = (self.piano_roll_note_pitch % 12) as usize;
            ui.strong(format!("{}{}", note_names[note_idx], octave));
        });

        ui.add_space(6.0);

        let total_ticks = self.last_status.as_ref().map(|s| s.total_ticks).unwrap_or(15360);
        let current_tick = self.last_status.as_ref().map(|s| s.current_tick).unwrap_or(0);
        let events = self.last_status.as_ref().map(|s| s.events.clone()).unwrap_or_default();

        let track_events: Vec<&jamseq::ScheduledEvent> = events
            .iter()
            .filter(|e| e.track_id == self.selected_track_id)
            .collect();

        // 2 Octaves displayed (C2 = 36 to C5 = 72)
        let min_note = 36u8; // C2
        let max_note = 72u8; // C5
        let num_notes = (max_note - min_note + 1) as usize;
        let row_height = 14.0f32;

        egui::ScrollArea::both().show(ui, |ui| {
            ui.horizontal(|ui| {
                // Left Keyboard Column (Piano Keys)
                let keyboard_width = 50.0;
                let (key_rect, _key_resp) = ui.allocate_exact_size(
                    egui::vec2(keyboard_width, num_notes as f32 * row_height),
                    egui::Sense::click(),
                );

                let key_painter = ui.painter();
                let note_names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

                for i in 0..num_notes {
                    let pitch = max_note - i as u8;
                    let y = key_rect.top() + i as f32 * row_height;
                    let r = egui::Rect::from_min_size(egui::pos2(key_rect.left(), y), egui::vec2(keyboard_width, row_height));

                    let is_black = [1, 3, 6, 8, 10].contains(&(pitch % 12));
                    let key_bg = if is_black { egui::Color32::from_rgb(30, 30, 35) } else { egui::Color32::from_rgb(220, 220, 225) };
                    key_painter.rect_filled(r, 1.0, key_bg);

                    let name = note_names[(pitch % 12) as usize];
                    let oct = (pitch / 12) as i32 - 1;
                    let text_color = if is_black { egui::Color32::LIGHT_GRAY } else { egui::Color32::BLACK };
                    key_painter.text(
                        egui::pos2(r.left() + 4.0, r.top() + 1.0),
                        egui::Align2::LEFT_TOP,
                        format!("{}{}", name, oct),
                        egui::FontId::proportional(10.0),
                        text_color,
                    );
                }

                // Main Note Grid Canvas (with click and drag to draw note lengths!)
                let grid_width = (ui.available_width() - 20.0).max(600.0);
                let (grid_rect, grid_resp) = ui.allocate_exact_size(
                    egui::vec2(grid_width, num_notes as f32 * row_height),
                    egui::Sense::click_and_drag(),
                );

                // Mouse down: register start of note drag
                if grid_resp.drag_started_by(egui::PointerButton::Primary) {
                    if let Some(pos) = grid_resp.interact_pointer_pos() {
                        let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.0, 1.0);
                        let clicked_tick = (pct_x * total_ticks as f32) as u64;
                        let shift_held = ui.input(|i| i.modifiers.shift);
                        let final_tick = if shift_held || self.freehand_drift {
                            (clicked_tick as i64 + self.drag_bias_ticks).max(0) as u64
                        } else {
                            let snapped = (clicked_tick / 240) * 240;
                            (snapped as i64 + self.drag_bias_ticks).max(0) as u64
                        };
                        let row_idx = ((pos.y - grid_rect.top()) / row_height).floor() as u8;
                        let clicked_pitch = max_note.saturating_sub(row_idx);
                        self.piano_roll_note_pitch = clicked_pitch;
                        self.drag_start = Some((final_tick, clicked_pitch));
                    }
                }

                // Mouse release: commit note with dragged or selected duration
                if grid_resp.drag_stopped_by(egui::PointerButton::Primary) {
                    if let Some((start_tick, pitch)) = self.drag_start.take() {
                        let final_duration = if let Some(pos) = grid_resp.interact_pointer_pos() {
                            let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.0, 1.0);
                            let current_tick = (pct_x * total_ticks as f32) as u64;
                            if current_tick > start_tick {
                                let shift_held = ui.input(|i| i.modifiers.shift);
                                let drag_len = if shift_held || self.freehand_drift {
                                    current_tick - start_tick
                                } else {
                                    ((current_tick - start_tick + 120) / 240) * 240
                                };
                                drag_len.max(self.note_duration_ticks)
                            } else {
                                self.note_duration_ticks
                            }
                        } else {
                            self.note_duration_ticks
                        };

                        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                        self.send_cmd(IpcCommand::AddNote {
                            note_on_id: now_ms,
                            note_off_id: now_ms + 1,
                            track_id: self.selected_track_id.clone(),
                            tick: start_tick,
                            duration_ticks: final_duration,
                            note: pitch,
                            velocity: 0.9,
                        });
                        self.status_msg = format!("Piano roll: Added note {} at tick {} (length: {} ticks)", pitch, start_tick, final_duration);
                    }
                } else if grid_resp.clicked_by(egui::PointerButton::Primary) && self.drag_start.is_none() {
                    // Normal single click: places note with currently selected duration
                    if let Some(pos) = grid_resp.interact_pointer_pos() {
                        let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.0, 1.0);
                        let clicked_tick = (pct_x * total_ticks as f32) as u64;
                        let shift_held = ui.input(|i| i.modifiers.shift);
                        let final_tick = if shift_held || self.freehand_drift {
                            (clicked_tick as i64 + self.drag_bias_ticks).max(0) as u64
                        } else {
                            let snapped = (clicked_tick / 240) * 240;
                            (snapped as i64 + self.drag_bias_ticks).max(0) as u64
                        };

                        let row_idx = ((pos.y - grid_rect.top()) / row_height).floor() as u8;
                        let clicked_pitch = max_note.saturating_sub(row_idx);
                        self.piano_roll_note_pitch = clicked_pitch;

                        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                        self.send_cmd(IpcCommand::AddNote {
                            note_on_id: now_ms,
                            note_off_id: now_ms + 1,
                            track_id: self.selected_track_id.clone(),
                            tick: final_tick,
                            duration_ticks: self.note_duration_ticks,
                            note: clicked_pitch,
                            velocity: 0.9,
                        });
                        self.status_msg = format!("Piano roll: Added note {} at tick {} (length: {} ticks)", clicked_pitch, final_tick, self.note_duration_ticks);
                    }
                } else if grid_resp.secondary_clicked() {
                    // Right click deletes note
                    if let Some(pos) = grid_resp.interact_pointer_pos() {
                        let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.0, 1.0);
                        let clicked_tick = (pct_x * total_ticks as f32) as u64;
                        let row_idx = ((pos.y - grid_rect.top()) / row_height).floor() as u8;
                        let clicked_pitch = max_note.saturating_sub(row_idx);

                        self.send_cmd(IpcCommand::DeleteNoteAt {
                            track_id: self.selected_track_id.clone(),
                            tick: clicked_tick,
                            note: clicked_pitch,
                        });
                        self.status_msg = format!("Piano roll: Deleted note {} near tick {}", clicked_pitch, clicked_tick);
                    }
                }

                let grid_painter = ui.painter();
                // Grid background rows
                for i in 0..num_notes {
                    let pitch = max_note - i as u8;
                    let y = grid_rect.top() + i as f32 * row_height;
                    let r = egui::Rect::from_min_size(egui::pos2(grid_rect.left(), y), egui::vec2(grid_width, row_height));
                    let is_black = [1, 3, 6, 8, 10].contains(&(pitch % 12));
                    let row_bg = if is_black { egui::Color32::from_rgb(22, 22, 28) } else { egui::Color32::from_rgb(32, 32, 38) };
                    grid_painter.rect_filled(r, 0.0, row_bg);
                }

                // Vertical 16th-note & Bar lines
                let total_16ths = (total_ticks / 240).max(1) as usize;
                for step in 0..=total_16ths {
                    let step_pct = step as f32 / total_16ths as f32;
                    let x = grid_rect.left() + grid_rect.width() * step_pct;
                    let is_bar = step % 16 == 0;
                    let line_color = if is_bar {
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(255, 255, 255, 15)
                    };
                    grid_painter.line_segment(
                        [egui::pos2(x, grid_rect.top()), egui::pos2(x, grid_rect.bottom())],
                        egui::Stroke::new(if is_bar { 1.5 } else { 0.5 }, line_color),
                    );
                }

                // Draw Note Blocks with true duration
                for ev in &track_events {
                    if let jamseq::timeline::ScheduledAction::NoteOn { note, velocity: _ } = ev.action {
                        if note >= min_note && note <= max_note && total_ticks > 0 {
                            // Find matching NoteOff tick
                            let note_off_tick = track_events
                                .iter()
                                .find(|off_ev| {
                                    off_ev.tick > ev.tick
                                        && match off_ev.action {
                                            jamseq::timeline::ScheduledAction::NoteOff { note: off_n } => off_n == note,
                                            _ => false,
                                        }
                                })
                                .map(|off_ev| off_ev.tick)
                                .unwrap_or(ev.tick + self.note_duration_ticks);

                            let duration = note_off_tick.saturating_sub(ev.tick).max(60);

                            let row = (max_note - note) as f32;
                            let y = grid_rect.top() + row * row_height;
                            let note_pct = (ev.tick as f32 / total_ticks as f32).clamp(0.0, 1.0);
                            let note_x = grid_rect.left() + grid_rect.width() * note_pct;
                            let note_w = (grid_rect.width() * (duration as f32 / total_ticks as f32)).max(6.0);
                            let n_rect = egui::Rect::from_min_size(
                                egui::pos2(note_x, y + 1.0),
                                egui::vec2(note_w, row_height - 2.0),
                            );

                            grid_painter.rect_filled(n_rect, 2.0, egui::Color32::from_rgb(90, 190, 255));
                            grid_painter.rect_stroke(n_rect, 2.0, egui::Stroke::new(1.0, egui::Color32::WHITE));
                        }
                    }
                }

                // Draw ghost note preview while dragging to lengthen
                if let Some((start_tick, pitch)) = self.drag_start {
                    if let Some(pos) = grid_resp.interact_pointer_pos() {
                        let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.0, 1.0);
                        let cur_tick = (pct_x * total_ticks as f32) as u64;
                        let preview_len = cur_tick.saturating_sub(start_tick).max(self.note_duration_ticks);
                        if pitch >= min_note && pitch <= max_note {
                            let row = (max_note - pitch) as f32;
                            let y = grid_rect.top() + row * row_height;
                            let note_pct = (start_tick as f32 / total_ticks as f32).clamp(0.0, 1.0);
                            let note_x = grid_rect.left() + grid_rect.width() * note_pct;
                            let note_w = (grid_rect.width() * (preview_len as f32 / total_ticks as f32)).max(6.0);
                            let ghost_rect = egui::Rect::from_min_size(
                                egui::pos2(note_x, y + 1.0),
                                egui::vec2(note_w, row_height - 2.0),
                            );
                            grid_painter.rect_filled(ghost_rect, 2.0, egui::Color32::from_rgba_unmultiplied(255, 230, 80, 180));
                            grid_painter.rect_stroke(ghost_rect, 2.0, egui::Stroke::new(1.5, egui::Color32::YELLOW));
                        }
                    }
                }

                // Playhead
                if total_ticks > 0 {
                    let playhead_pct = (current_tick as f32 / total_ticks as f32).clamp(0.0, 1.0);
                    let playhead_x = grid_rect.left() + grid_rect.width() * playhead_pct;
                    grid_painter.line_segment(
                        [egui::pos2(playhead_x, grid_rect.top()), egui::pos2(playhead_x, grid_rect.bottom())],
                        egui::Stroke::new(2.5, egui::Color32::WHITE),
                    );
                }
            });
        });
    }
}
