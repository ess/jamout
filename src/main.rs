mod search_input;
mod sliding_panel;

use eframe::egui;
use jamseq::ipc::{IpcCommand, IpcResponse, IpcStatusResponse};
use jamseq::timeline::{LoopMode, TempoOverride, TrackBlock};
use search_input::{SearchInput, SearchablePicker};
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

#[derive(Clone, Debug)]
pub struct BlockDragState {
    pub section_id: String,
    pub track_id: String,
    pub initial_offset: u64,
    pub start_pos_x: f32,
    pub is_slip_edit: bool,
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
    pub _drift_ticks: i64,
    pub drag_bias_ticks: i64,
    pub freehand_drift: bool,

    // Project switching UI state
    pub show_project_dialog: bool,
    pub project_path_input: String,
    pub new_project_title: String,
    pub new_project_bpm: f64,

    // Song section management UI state
    pub show_section_dialog: bool,
    pub new_section_name: String,

    // Piano roll state
    pub selected_track_id: String,
    pub piano_roll_note_pitch: u8,
    pub piano_roll_scroll_y: Option<f32>,

    // Track editing state
    pub track_name_edits: HashMap<String, String>,

    // Live plugin / synth editor modal
    pub edit_track_id: Option<String>,
    pub edit_insert_slot: Option<(String, usize)>, // (track_id, insert_index)
    pub preset_save_name: String,
    pub param_search_filter: String,
    pub plugin_search_filter: String,
    pub insert_search_filter: String,

    // Note duration and drag drawing state
    pub note_duration_ticks: u64,
    pub drag_start: Option<(u64, u8)>, // (start_tick, pitch)

    // Matrix Arranger & Block Editing State
    pub selected_block: Option<(String, String)>, // (section_id, track_id)
    pub block_drag: Option<BlockDragState>,
    pub show_tempo_dialog: Option<(String, Option<String>)>, // (section_id, Option<track_id>)
    pub tempo_dialog_mode: usize, // 0 = Inherited, 1 = Absolute BPM, 2 = Ratio
    pub tempo_dialog_bpm: f64,
    pub tempo_dialog_num: u32,
    pub tempo_dialog_den: u32,
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
            _drift_ticks: 20,       // ±20 ticks Gaussian feel
            drag_bias_ticks: 15,   // +15 ticks lazy 90s R&B drag
            freehand_drift: false,
            show_project_dialog: false,
            project_path_input: String::new(),
            new_project_title: "My New Jam".to_string(),
            new_project_bpm: 68.0,
            show_section_dialog: false,
            new_section_name: "Verse 1".to_string(),
            selected_track_id: "bass".to_string(),
            piano_roll_note_pitch: 50, // D3 default
            piano_roll_scroll_y: Some(472.0), // Center on Middle C (C4) initially
            track_name_edits: HashMap::new(),
            edit_track_id: None,
            edit_insert_slot: None,
            preset_save_name: "custom_patch.json".to_string(),
            param_search_filter: String::new(),
            plugin_search_filter: String::new(),
            insert_search_filter: String::new(),
            note_duration_ticks: 480, // 1/8 note default
            drag_start: None,
            selected_block: None,
            block_drag: None,
            show_tempo_dialog: None,
            tempo_dialog_mode: 0,
            tempo_dialog_bpm: 120.0,
            tempo_dialog_num: 3,
            tempo_dialog_den: 2,
        }
    }

    fn send_cmd(&self, cmd: IpcCommand) {
        let _ = self.cmd_sender.send(cmd);
    }

    fn prompt_open_project(&mut self) {
        let dialog = rfd::FileDialog::new()
            .set_title("Open JamBand Project")
            .add_filter("JamBand Project (*.jam.json, *.json)", &["jam.json", "json"]);

        if let Some(path) = dialog.pick_file() {
            let path_str = path.to_string_lossy().to_string();
            self.send_cmd(IpcCommand::LoadProject { path: path_str.clone() });
            self.project_path_input = path_str.clone();
            self.status_msg = format!("Loaded project: {}", path_str);
            self.show_project_dialog = false;
        }
    }

    fn prompt_save_project_as(&mut self) {
        let default_name = self.last_status.as_ref()
            .map(|s| {
                let title = s.project_title.trim();
                if title.is_empty() || title == "Untitled" {
                    "untitled".to_string()
                } else {
                    title.replace(' ', "_").to_lowercase()
                }
            })
            .unwrap_or_else(|| "project".to_string());
        let default_file = format!("{}.jam.json", default_name);

        let dialog = rfd::FileDialog::new()
            .set_title("Save JamBand Project As")
            .set_file_name(&default_file)
            .add_filter("JamBand Project (*.jam.json)", &["jam.json", "json"]);

        if let Some(path) = dialog.save_file() {
            let normalized = normalize_jam_path(&path);
            let path_str = normalized.to_string_lossy().to_string();
            self.send_cmd(IpcCommand::SaveProject { path: path_str.clone() });
            self.project_path_input = path_str.clone();
            self.status_msg = format!("Saved project to: {}", path_str);
            self.show_project_dialog = false;
        }
    }

    fn save_project_quick(&mut self) {
        let existing = self.last_status.as_ref().and_then(|s| s.project_path.clone());
        if let Some(path) = existing {
            self.send_cmd(IpcCommand::SaveProject { path: path.clone() });
            self.status_msg = format!("Saved project: {}", path);
        } else {
            self.prompt_save_project_as();
        }
    }
}

fn normalize_jam_path(path: &std::path::Path) -> std::path::PathBuf {
    let s = path.to_string_lossy();
    if s.ends_with(".jam.json") {
        path.to_path_buf()
    } else if s.ends_with(".json") {
        let stem = s.strip_suffix(".json").unwrap();
        std::path::PathBuf::from(format!("{}.jam.json", stem))
    } else {
        std::path::PathBuf::from(format!("{}.jam.json", s))
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

        // Global Keyboard Shortcuts for Project Management
        let ctrl_or_cmd = ctx.input(|i| i.modifiers.command || i.modifiers.ctrl);
        if ctrl_or_cmd {
            if ctx.input(|i| i.key_pressed(egui::Key::O)) {
                self.prompt_open_project();
            } else if ctx.input(|i| i.modifiers.shift && i.key_pressed(egui::Key::S)) {
                self.prompt_save_project_as();
            } else if ctx.input(|i| !i.modifiers.shift && i.key_pressed(egui::Key::S)) {
                self.save_project_quick();
            }
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

                // Project Switcher Button & Quick Open / Save Buttons
                let proj_title = self.last_status.as_ref().map(|s| s.project_title.as_str()).unwrap_or("No Project");
                if ui.button(format!("📁 Project: {}", proj_title)).on_hover_text("Open Project Manager").clicked() {
                    self.show_project_dialog = !self.show_project_dialog;
                }
                if ui.small_button("📂").on_hover_text("Open Project File (Ctrl+O)").clicked() {
                    self.prompt_open_project();
                }
                if ui.small_button("💾").on_hover_text("Save Project (Ctrl+S)").clicked() {
                    self.save_project_quick();
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
                    ui.set_width(460.0);
                    ui.heading("Project Manager");
                    ui.add_space(8.0);

                    let current_path = self.last_status.as_ref()
                        .and_then(|s| s.project_path.clone())
                        .unwrap_or_else(|| "(Unsaved In-Memory Project)".to_string());
                    let current_title = self.last_status.as_ref()
                        .map(|s| s.project_title.clone())
                        .unwrap_or_else(|| "Untitled".to_string());
                    let current_bpm = self.last_status.as_ref()
                        .map(|s| s.bpm)
                        .unwrap_or(120.0);

                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Active Project:").strong());
                            ui.label(format!("{} ({:.1} BPM)", current_title, current_bpm));
                        });
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("File Path:").strong());
                            ui.monospace(&current_path);
                        });
                    });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("📂 Open Project... (Ctrl+O)").clicked() {
                            self.prompt_open_project();
                        }

                        if ui.button("💾 Save (Ctrl+S)").clicked() {
                            self.save_project_quick();
                        }

                        if ui.button("💾 Save As... (Ctrl+Shift+S)").clicked() {
                            self.prompt_save_project_as();
                        }

                        if ui.button("Close").clicked() {
                            self.show_project_dialog = false;
                        }
                    });

                    ui.add_space(12.0);
                    ui.separator();
                    ui.heading("✨ Create New Project");
                    ui.small("Starts a fresh in-memory project. You will be prompted to choose a save file when ready.");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Title:");
                        ui.text_edit_singleline(&mut self.new_project_title);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Base Tempo:");
                        ui.add(egui::Slider::new(&mut self.new_project_bpm, 40.0..=220.0).text("BPM"));
                    });
                    ui.add_space(6.0);
                    if ui.button("➕ Create Clean Project").clicked() {
                        let title = if self.new_project_title.trim().is_empty() {
                            "Untitled".to_string()
                        } else {
                            self.new_project_title.trim().to_string()
                        };
                        self.send_cmd(IpcCommand::NewProject {
                            title: title.clone(),
                            bpm: self.new_project_bpm,
                        });
                        self.status_msg = format!("Created fresh project: '{}' @ {:.1} BPM", title, self.new_project_bpm);
                        self.show_project_dialog = false;
                    }
                });
        }

        // Song Section Manager Modal Dialog
        if self.show_section_dialog {
            egui::Window::new("🎼 Add Song Section")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.heading("Add Section");
                    ui.small("Sections define song structure. Length is content-driven and automatically calculated from track blocks.");
                    ui.add_space(8.0);

                    ui.label("Presets:");
                    ui.horizontal_wrapped(|ui| {
                        let presets = [
                            "Intro", "Verse 1", "Verse 2", "Chorus 1", "Chorus 2", "Bridge", "Solo", "Outro", "Breakdown"
                        ];
                        for name in presets {
                            if ui.button(name).clicked() {
                                self.new_section_name = name.to_string();
                            }
                        }
                    });

                    ui.add_space(6.0);
                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut self.new_section_name);
                    });

                    ui.add_space(8.0);
                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui.button("➕ Add Section").clicked() {
                            let name = self.new_section_name.trim();
                            if !name.is_empty() {
                                let id = name.to_lowercase().replace(' ', "_");
                                self.send_cmd(IpcCommand::AddSection {
                                    id: id.clone(),
                                    name: name.to_string(),
                                    start_tick: 0,
                                    length_ticks: 0,
                                });
                                self.status_msg = format!("Added section '{}'", name);
                                self.show_section_dialog = false;
                            }
                        }

                        if ui.button("Close").clicked() {
                            self.show_section_dialog = false;
                        }
                    });
                });
        }

        // Tempo & Metric Modulation Modal
        if let Some((sec_id, track_id_opt)) = self.show_tempo_dialog.clone() {
            let is_block = track_id_opt.is_some();
            let title = if let Some(ref tid) = track_id_opt {
                format!("⏱ Track Block Tempo ({})", tid)
            } else {
                format!("⏱ Section Tempo ({})", sec_id)
            };

            let project_bpm = self.last_status.as_ref().map(|s| s.bpm).unwrap_or(120.0);
            let parent_bpm = if is_block {
                self.last_status
                    .as_ref()
                    .and_then(|s| s.sections.iter().find(|sec| sec.id == sec_id))
                    .map(|sec| sec.effective_bpm(project_bpm))
                    .unwrap_or(project_bpm)
            } else {
                project_bpm
            };

            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.label(egui::RichText::new("Hierarchical Tempo Inheritance").strong());
                    ui.small(format!("Parent Tempo: {:.1} BPM", parent_bpm));
                    ui.add_space(8.0);

                    ui.radio_value(&mut self.tempo_dialog_mode, 0, "Inherit from parent");
                    ui.radio_value(&mut self.tempo_dialog_mode, 1, "Absolute BPM");
                    if self.tempo_dialog_mode == 1 {
                        ui.indent("abs_bpm_indent", |ui| {
                            ui.horizontal(|ui| {
                                ui.label("BPM:");
                                ui.add(egui::DragValue::new(&mut self.tempo_dialog_bpm).speed(0.5).range(20.0..=300.0));
                            });
                        });
                    }

                    ui.radio_value(&mut self.tempo_dialog_mode, 2, "Ratio (Metric Modulation)");
                    if self.tempo_dialog_mode == 2 {
                        ui.indent("ratio_bpm_indent", |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Ratio:");
                                ui.add(egui::DragValue::new(&mut self.tempo_dialog_num).speed(1).range(1..=16));
                                ui.label(":");
                                ui.add(egui::DragValue::new(&mut self.tempo_dialog_den).speed(1).range(1..=16));
                            });
                            let eff = if self.tempo_dialog_den > 0 {
                                parent_bpm * (self.tempo_dialog_num as f64 / self.tempo_dialog_den as f64)
                            } else {
                                parent_bpm
                            };
                            ui.label(
                                egui::RichText::new(format!(
                                    "Resulting Tempo: {}:{} ({:.1} BPM)",
                                    self.tempo_dialog_num, self.tempo_dialog_den, eff
                                ))
                                .italics()
                                .color(egui::Color32::from_rgb(255, 180, 50)),
                            );

                            ui.horizontal_wrapped(|ui| {
                                if ui.small_button("3:2 (1.5x)").clicked() { self.tempo_dialog_num = 3; self.tempo_dialog_den = 2; }
                                if ui.small_button("4:3 (1.33x)").clicked() { self.tempo_dialog_num = 4; self.tempo_dialog_den = 3; }
                                if ui.small_button("1:2 (0.5x)").clicked() { self.tempo_dialog_num = 1; self.tempo_dialog_den = 2; }
                                if ui.small_button("2:1 (2.0x)").clicked() { self.tempo_dialog_num = 2; self.tempo_dialog_den = 1; }
                            });
                        });
                    }

                    ui.add_space(8.0);
                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui.button("Apply Tempo").clicked() {
                            let override_val = match self.tempo_dialog_mode {
                                1 => Some(TempoOverride::AbsoluteBpm(self.tempo_dialog_bpm)),
                                2 => Some(TempoOverride::Ratio(self.tempo_dialog_num, self.tempo_dialog_den)),
                                _ => None,
                            };

                            if let Some(ref tid) = track_id_opt {
                                self.send_cmd(IpcCommand::SetBlockTempo {
                                    section_id: sec_id.clone(),
                                    track_id: tid.clone(),
                                    tempo: override_val,
                                });
                                self.status_msg = format!("Updated tempo for block '{}' in section '{}'", tid, sec_id);
                            } else {
                                self.send_cmd(IpcCommand::SetSectionTempo {
                                    section_id: sec_id.clone(),
                                    tempo: override_val,
                                });
                                self.status_msg = format!("Updated tempo for section '{}'", sec_id);
                            }
                            self.show_tempo_dialog = None;
                        }

                        if ui.button("Cancel").clicked() {
                            self.show_tempo_dialog = None;
                        }
                    });
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

                            let available = self.last_status.as_ref()
                                .map(|s| s.available_plugins.clone())
                                .unwrap_or_default();

                            let chosen_plug = SearchablePicker::show(
                                ui,
                                format!("plugin_sel_{}", track.id),
                                current_plug,
                                &mut self.plugin_search_filter,
                                &available,
                                |plug| format!("{} ({})", plug.name, plug.plugin_format),
                            );

                            if let Some(plug) = chosen_plug {
                                let display = format!("{} ({})", plug.name, plug.plugin_format);
                                self.send_cmd(IpcCommand::SwitchTrackPlugin {
                                    track_id: track.id.clone(),
                                    plugin_uri_or_type: plug.uri_or_path.clone(),
                                    plugin_name: display,
                                });
                            }

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
                                    ui.add(
                                        SearchInput::new(&mut self.param_search_filter, "🔍 Search parameters...")
                                            .id_salt("param_search_input")
                                            .desired_width(180.0)
                                    );
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

                                let available = self.last_status.as_ref()
                                    .map(|s| s.available_plugins.clone())
                                    .unwrap_or_default();

                                let chosen_effect = SearchablePicker::show(
                                    ui,
                                    format!("add_insert_sel_{}", track.id),
                                    "➕ Add Effect...",
                                    &mut self.insert_search_filter,
                                    &available,
                                    |plug| format!("{} ({})", plug.name, plug.plugin_format),
                                );

                                if let Some(plug) = chosen_effect {
                                    let display = format!("{} ({})", plug.name, plug.plugin_format);
                                    self.send_cmd(IpcCommand::AddTrackInsert {
                                        track_id: track.id.clone(),
                                        plugin_uri_or_path: plug.uri_or_path.clone(),
                                        plugin_name: display.clone(),
                                    });
                                    self.status_msg = format!("Added insert effect '{}'", display);
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
        let total_ticks = self.last_status.as_ref().map(|s| s.total_ticks).unwrap_or(0);
        let current_tick = self.last_status.as_ref().map(|s| s.current_tick).unwrap_or(0);
        let active_section_id = self.last_status.as_ref().and_then(|s| s.active_section_id.clone());
        let sections = self.last_status.as_ref().map(|s| s.sections.clone()).unwrap_or_default();
        let tracks = self.last_status.as_ref().map(|s| s.tracks.clone()).unwrap_or_default();
        let project_bpm = self.last_status.as_ref().map(|s| s.bpm).unwrap_or(120.0);

        // --- 0. KEYBOARD SHORTCUTS FOR SELECTED BLOCK NUDGING ---
        if let Some((ref sec_id, ref track_id)) = self.selected_block.clone() {
            let alt_held = ui.input(|i| i.modifiers.alt);
            if alt_held {
                let nudge_left = ui.input(|i| i.key_pressed(egui::Key::ArrowLeft));
                let nudge_right = ui.input(|i| i.key_pressed(egui::Key::ArrowRight));
                if nudge_left || nudge_right {
                    if let Some(sec) = sections.iter().find(|s| s.id == *sec_id) {
                        if let Some(block) = sec.track_blocks.get(track_id) {
                            let mut updated = block.clone();
                            if nudge_left {
                                updated.start_offset_ticks = updated.start_offset_ticks.saturating_sub(240);
                            } else {
                                updated.start_offset_ticks += 240;
                            }
                            self.send_cmd(IpcCommand::SetTrackBlock {
                                section_id: sec_id.clone(),
                                block: updated,
                            });
                        }
                    }
                }
            }
        }

        // --- 1. TOP HEADER & TRANSPORT TIMELINE ---
        ui.horizontal(|ui| {
            ui.heading("Arrangement Matrix Arranger");
            ui.separator();
            ui.label(egui::RichText::new(format!("Base Tempo: {:.1} BPM", project_bpm)).strong());

            ui.separator();
            ui.label("Playhead:");
            let mut seek_tick = current_tick;
            if ui.add(egui::Slider::new(&mut seek_tick, 0..=total_ticks.max(1)).text("Ticks")).changed() {
                self.send_cmd(IpcCommand::Seek { tick: seek_tick });
            }

            if let Some(sec_id) = &active_section_id {
                ui.separator();
                ui.colored_label(egui::Color32::from_rgb(255, 215, 0), format!("📍 Active Section: {}", sec_id));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("➕ Add Section").on_hover_text("Define or append a new song section").clicked() {
                    self.show_section_dialog = true;
                }
            });
        });

        ui.add_space(6.0);

        // --- 2. SELECTED BLOCK INSPECTOR & NUDGE TOOLBAR ---
        if let Some((ref sec_id, ref track_id)) = self.selected_block.clone() {
            if let Some(sec) = sections.iter().find(|s| s.id == *sec_id) {
                if let Some(block) = sec.track_blocks.get(track_id) {
                    ui.group(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("🔍 Selected Block: {} / {}", sec.name, track_id)).strong());

                            ui.separator();
                            ui.label("Start Offset:");
                            let mut offset = block.start_offset_ticks;
                            let bar = (offset / 3840) + 1;
                            let beat = ((offset % 3840) as f64 / 960.0) + 1.0;
                            ui.small(format!("(Bar {}.{:.1})", bar, beat));
                            if ui.add(egui::DragValue::new(&mut offset).speed(240).range(0..=1000000)).changed() {
                                let mut updated = block.clone();
                                updated.start_offset_ticks = offset;
                                self.send_cmd(IpcCommand::SetTrackBlock { section_id: sec.id.clone(), block: updated });
                            }

                            // Nudge buttons
                            if ui.small_button("◀ -240t").on_hover_text("Nudge earlier 1/16 note (Alt+Left)").clicked() {
                                let mut updated = block.clone();
                                updated.start_offset_ticks = updated.start_offset_ticks.saturating_sub(240);
                                self.send_cmd(IpcCommand::SetTrackBlock { section_id: sec.id.clone(), block: updated });
                            }
                            if ui.small_button("+240t ▶").on_hover_text("Nudge later 1/16 note (Alt+Right)").clicked() {
                                let mut updated = block.clone();
                                updated.start_offset_ticks += 240;
                                self.send_cmd(IpcCommand::SetTrackBlock { section_id: sec.id.clone(), block: updated });
                            }

                            ui.separator();
                            ui.label("Base Length:");
                            let mut base_len = block.base_length_ticks;
                            if ui.add(egui::DragValue::new(&mut base_len).speed(240).range(240..=1000000)).changed() {
                                let mut updated = block.clone();
                                updated.base_length_ticks = base_len;
                                self.send_cmd(IpcCommand::SetTrackBlock { section_id: sec.id.clone(), block: updated });
                            }

                            ui.separator();
                            ui.label("Loop Mode:");
                            let mode_label = match block.loop_mode {
                                LoopMode::OneShot => "One-Shot (1x)",
                                LoopMode::LoopToSection => "Loop To Section",
                                LoopMode::FixedLoops(n) => if n == 2 { "Loop 2x" } else { "Loop 4x" },
                            };
                            egui::ComboBox::from_id_salt("block_loop_combo")
                                .selected_text(mode_label)
                                .show_ui(ui, |ui| {
                                    if ui.selectable_label(block.loop_mode == LoopMode::OneShot, "One-Shot (1x)").clicked() {
                                        let mut updated = block.clone();
                                        updated.loop_mode = LoopMode::OneShot;
                                        self.send_cmd(IpcCommand::SetTrackBlock { section_id: sec.id.clone(), block: updated });
                                    }
                                    if ui.selectable_label(block.loop_mode == LoopMode::LoopToSection, "Loop To Section").clicked() {
                                        let mut updated = block.clone();
                                        updated.loop_mode = LoopMode::LoopToSection;
                                        self.send_cmd(IpcCommand::SetTrackBlock { section_id: sec.id.clone(), block: updated });
                                    }
                                    if ui.selectable_label(block.loop_mode == LoopMode::FixedLoops(2), "Loop 2x").clicked() {
                                        let mut updated = block.clone();
                                        updated.loop_mode = LoopMode::FixedLoops(2);
                                        self.send_cmd(IpcCommand::SetTrackBlock { section_id: sec.id.clone(), block: updated });
                                    }
                                    if ui.selectable_label(block.loop_mode == LoopMode::FixedLoops(4), "Loop 4x").clicked() {
                                        let mut updated = block.clone();
                                        updated.loop_mode = LoopMode::FixedLoops(4);
                                        self.send_cmd(IpcCommand::SetTrackBlock { section_id: sec.id.clone(), block: updated });
                                    }
                                });

                            ui.separator();
                            if ui.button("⏱ Block Tempo").clicked() {
                                self.show_tempo_dialog = Some((sec.id.clone(), Some(track_id.clone())));
                            }

                            if ui.button("🎹 Piano Roll").clicked() {
                                self.selected_track_id = track_id.clone();
                                self.active_tab = Tab::PianoRoll;
                            }

                            if ui.small_button("🗑").on_hover_text("Delete track block").clicked() {
                                self.send_cmd(IpcCommand::RemoveTrackBlock {
                                    section_id: sec.id.clone(),
                                    track_id: track_id.clone(),
                                });
                                self.selected_block = None;
                            }
                        });
                    });
                    ui.add_space(6.0);
                }
            }
        }

        // --- 3. MATRIX ARRANGER GRID (Sections Columns x Tracks Rows) ---
        egui::ScrollArea::both().show(ui, |ui| {
            // Header Row: Track Column Header + Each Section Column Header
            ui.horizontal(|ui| {
                // Top-Left Corner Cell (Track Column Header)
                ui.allocate_ui(egui::vec2(220.0, 52.0), |ui| {
                    ui.set_width(220.0);
                    ui.set_max_width(220.0);
                    ui.vertical(|ui| {
                        ui.strong("TRACKS");
                        ui.horizontal(|ui| {
                            if ui.small_button("➕ MIDI").clicked() {
                                let count = tracks.len() + 1;
                                let id = format!("track_{}", count);
                                let name = format!("MIDI Track {}", count);
                                self.send_cmd(IpcCommand::AddTrack { id: id.clone(), name: name.clone(), track_type: "synth".to_string() });
                                self.track_name_edits.insert(id, name);
                            }
                            if ui.small_button("➕ Audio").clicked() {
                                let count = tracks.len() + 1;
                                let id = format!("audio_{}", count);
                                let name = format!("Audio Track {}", count);
                                self.send_cmd(IpcCommand::AddTrack { id: id.clone(), name: name.clone(), track_type: "audio".to_string() });
                                self.track_name_edits.insert(id, name);
                            }
                        });
                    });
                });

                ui.separator();

                // Section Column Headers
                if sections.is_empty() {
                    ui.label("(No sections in project. Click '➕ Add Section' to create your first arrangement section.)");
                }

                let mut accum_ticks = 0u64;
                for (_s_idx, sec) in sections.iter().enumerate() {
                    let sec_len = sec.length_ticks();
                    let sec_start_tick = accum_ticks;
                    accum_ticks += sec_len;

                    let col_width = if sec_len == 0 {
                        160.0
                    } else {
                        ((sec_len as f32 / 3840.0) * 160.0).clamp(180.0, 720.0)
                    };

                    ui.allocate_ui(egui::vec2(col_width, 52.0), |ui| {
                        ui.set_width(col_width);
                        ui.vertical(|ui| {
                            // Section Name and Energy
                            ui.horizontal(|ui| {
                                let is_active = active_section_id.as_deref() == Some(&sec.id);
                                let sec_btn = if is_active {
                                    egui::Button::new(egui::RichText::new(format!("▶ {}", sec.name)).strong().color(egui::Color32::BLACK))
                                        .fill(egui::Color32::from_rgb(255, 215, 80))
                                } else {
                                    egui::Button::new(egui::RichText::new(&sec.name).strong())
                                        .fill(egui::Color32::from_rgb(45, 55, 70))
                                };

                                if ui.add(sec_btn).on_hover_text("Click to seek playhead to section downbeat").clicked() {
                                    self.send_cmd(IpcCommand::Seek { tick: sec_start_tick });
                                }

                                if ui.small_button("🗑").on_hover_text("Delete section").clicked() {
                                    self.send_cmd(IpcCommand::RemoveSection { section_id: sec.id.clone() });
                                }
                            });

                            // Metric Modulation & Length Sub-row
                            ui.horizontal(|ui| {
                                // Metric Modulation Tempo Pill
                                let eff_bpm = sec.effective_bpm(project_bpm);
                                let tempo_text = match sec.tempo {
                                    Some(TempoOverride::AbsoluteBpm(b)) => format!("{:.1} BPM", b),
                                    Some(TempoOverride::Ratio(n, d)) => format!("{}:{} ({:.1} BPM)", n, d, eff_bpm),
                                    None => format!("{:.1} BPM [Inherit]", eff_bpm),
                                };

                                let pill_color = match sec.tempo {
                                    Some(TempoOverride::Ratio(_, _)) => egui::Color32::from_rgb(180, 100, 30),
                                    Some(TempoOverride::AbsoluteBpm(_)) => egui::Color32::from_rgb(30, 90, 160),
                                    None => egui::Color32::from_rgb(60, 65, 75),
                                };

                                if ui.add(egui::Button::new(egui::RichText::new(tempo_text).small()).fill(pill_color))
                                    .on_hover_text("Click to configure section tempo or metric modulation")
                                    .clicked()
                                {
                                    self.show_tempo_dialog = Some((sec.id.clone(), None));
                                }

                                // Derived Length Display
                                if sec_len == 0 {
                                    ui.small(egui::RichText::new("(Empty - 0t)").color(egui::Color32::GRAY));
                                } else {
                                    let bars = sec_len as f64 / 3840.0;
                                    ui.small(format!("{:.1}b ({}t)", bars, sec_len));
                                }
                            });
                        });
                    });

                    ui.separator();
                }

                // Placeholder button column to append section
                ui.allocate_ui(egui::vec2(120.0, 52.0), |ui| {
                    ui.set_width(120.0);
                    if ui.button("➕ New Section").clicked() {
                        self.show_section_dialog = true;
                    }
                });
            });

            ui.separator();

            // Track Rows: Each Track intersects all Sections
            for (t_idx, track) in tracks.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.set_min_height(64.0);

                    // Track Control Header (Leftmost 220px)
                    ui.allocate_ui(egui::vec2(220.0, 64.0), |ui| {
                        ui.set_width(220.0);
                        ui.set_max_width(220.0);
                        ui.vertical(|ui| {
                            let (need_rename, new_name_str) = {
                                let current_buf = self.track_name_edits.entry(track.id.clone()).or_insert_with(|| track.name.clone());
                                let resp = ui.add(egui::TextEdit::singleline(current_buf).desired_width(210.0));
                                if resp.lost_focus() && *current_buf != track.name && !current_buf.trim().is_empty() {
                                    (true, current_buf.trim().to_string())
                                } else {
                                    (false, String::new())
                                }
                            };
                            if need_rename {
                                self.send_cmd(IpcCommand::RenameTrack { track_id: track.id.clone(), new_name: new_name_str.clone() });
                                self.status_msg = format!("Renamed track to '{}'", new_name_str);
                            }

                            ui.horizontal(|ui| {
                                ui.small(if track.current_bpm > 0.0 { format!("{:.1} BPM", track.current_bpm) } else { format!("{:.1} BPM", project_bpm) });

                                if ui.small_button("🎹").on_hover_text("Open Piano Roll").clicked() {
                                    self.selected_track_id = track.id.clone();
                                    self.active_tab = Tab::PianoRoll;
                                }
                                if ui.small_button("🎛").on_hover_text("Tweak Instruments & DSP").clicked() {
                                    self.edit_track_id = Some(track.id.clone());
                                }

                                let mut muted = track.muted;
                                if ui.toggle_value(&mut muted, "M").clicked() {
                                    self.send_cmd(IpcCommand::SetMute { track_id: track.id.clone(), muted });
                                }

                                let mut solo = track.solo;
                                if ui.toggle_value(&mut solo, "S").clicked() {
                                    self.send_cmd(IpcCommand::SetSolo { track_id: track.id.clone(), solo });
                                }

                                if ui.small_button("🗑").on_hover_text("Delete track").clicked() {
                                    self.send_cmd(IpcCommand::DeleteTrack { track_id: track.id.clone() });
                                }
                            });
                        });
                    });

                    ui.separator();

                    // Section Cells for this Track
                    let mut cell_accum_ticks = 0u64;
                    for (_s_idx, sec) in sections.iter().enumerate() {
                        let sec_len = sec.length_ticks();
                        let sec_start_tick = cell_accum_ticks;
                        cell_accum_ticks += sec_len;

                        let col_width = if sec_len == 0 {
                            160.0
                        } else {
                            ((sec_len as f32 / 3840.0) * 160.0).clamp(180.0, 720.0)
                        };

                        let (cell_rect, cell_resp) = ui.allocate_exact_size(
                            egui::vec2(col_width, 64.0),
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
                            let is_selected = self.selected_block.as_ref() == Some(&(sec.id.clone(), track.id.clone()));

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

                            let stroke = if is_selected {
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 215, 0))
                            } else {
                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60))
                            };

                            let painter = ui.painter();
                            painter.rect(block_rect, 4.0, card_color, stroke);

                            // Block title & offset readout
                            let title_text = if block.start_offset_ticks > 0 {
                                format!("{} (+{}t)", block.id, block.start_offset_ticks)
                            } else {
                                block.id.clone()
                            };
                            painter.text(
                                egui::pos2(block_rect.left() + 6.0, block_rect.top() + 4.0),
                                egui::Align2::LEFT_TOP,
                                title_text,
                                egui::FontId::proportional(11.0),
                                egui::Color32::WHITE,
                            );

                            // Loop Mode & Metric Modulation Badges
                            let mode_pill = match block.loop_mode {
                                LoopMode::OneShot => "1x",
                                LoopMode::LoopToSection => "Loop",
                                LoopMode::FixedLoops(n) => if n == 2 { "2x" } else { "4x" },
                            };
                            painter.text(
                                egui::pos2(block_rect.right() - 6.0, block_rect.top() + 4.0),
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

                            // --- INTERACTIVE DRAGGING: ITEM SLIDING & SLIP EDITING ---
                            if cell_resp.double_clicked() {
                                self.selected_block = Some((sec.id.clone(), track.id.clone()));
                                self.selected_track_id = track.id.clone();
                                self.active_tab = Tab::PianoRoll;
                            } else if cell_resp.clicked() {
                                self.selected_block = Some((sec.id.clone(), track.id.clone()));
                                self.selected_track_id = track.id.clone();
                            }

                            if cell_resp.drag_started() {
                                self.selected_block = Some((sec.id.clone(), track.id.clone()));
                                self.selected_track_id = track.id.clone();
                                let is_slip = ui.input(|i| i.modifiers.alt);
                                let start_pos = cell_resp.interact_pointer_pos().map(|p| p.x).unwrap_or(0.0);
                                self.block_drag = Some(BlockDragState {
                                    section_id: sec.id.clone(),
                                    track_id: track.id.clone(),
                                    initial_offset: block.start_offset_ticks,
                                    start_pos_x: start_pos,
                                    is_slip_edit: is_slip,
                                });
                            }

                            if cell_resp.dragged() {
                                if let Some(ref drag) = self.block_drag.clone() {
                                    if drag.section_id == sec.id && drag.track_id == track.id {
                                        if let Some(pos) = cell_resp.interact_pointer_pos() {
                                            let delta_px = pos.x - drag.start_pos_x;
                                            let ticks_per_px = if sec_len > 0 { sec_len as f32 / cell_rect.width() } else { 3840.0 / cell_rect.width() };
                                            let delta_ticks = (delta_px * ticks_per_px) as i64;

                                            let shift_held = ui.input(|i| i.modifiers.shift);
                                            let snapped_delta = if shift_held || self.freehand_drift {
                                                delta_ticks
                                            } else {
                                                (delta_ticks / 240) * 240
                                            };

                                            if drag.is_slip_edit {
                                                // Slip Editing: Shift internal event timestamps
                                                let mut updated = block.clone();
                                                for ev in &mut updated.events {
                                                    ev.tick = (ev.tick as i64 + snapped_delta).max(0) as u64;
                                                }
                                                self.send_cmd(IpcCommand::SetTrackBlock {
                                                    section_id: sec.id.clone(),
                                                    block: updated,
                                                });
                                            } else {
                                                // Horizontal Item Sliding: Shift start_offset_ticks
                                                let new_offset = (drag.initial_offset as i64 + snapped_delta).max(0) as u64;
                                                let mut updated = block.clone();
                                                updated.start_offset_ticks = new_offset;
                                                self.send_cmd(IpcCommand::SetTrackBlock {
                                                    section_id: sec.id.clone(),
                                                    block: updated,
                                                });
                                            }
                                        }
                                    }
                                }
                            }

                            if cell_resp.drag_stopped() {
                                self.block_drag = None;
                            }
                        } else {
                            // Empty / Rest Cell (Silence for this track in this section)
                            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(cell_rect), |ui| {
                                ui.centered_and_justified(|ui| {
                                    if ui.small_button("➕ Add Block").on_hover_text("Add a track block / pattern to this section").clicked() {
                                        let block_id = format!("{}_{}", sec.id, track.id);
                                        let new_block = TrackBlock::new(&block_id, &track.id, 3840);
                                        self.send_cmd(IpcCommand::SetTrackBlock {
                                            section_id: sec.id.clone(),
                                            block: new_block,
                                        });
                                        self.selected_block = Some((sec.id.clone(), track.id.clone()));
                                        self.selected_track_id = track.id.clone();
                                    }
                                });
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
                });

                ui.add_space(2.0);
            }
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
        let tracks = self.last_status.as_ref().map(|s| s.tracks.clone()).unwrap_or_default();

        let available_blocks: Vec<(String, String, TrackBlock)> = self.last_status.as_ref().map(|st| {
            st.sections.iter().filter_map(|s| {
                s.track_blocks.get(&self.selected_track_id).map(|b| (s.id.clone(), s.name.clone(), b.clone()))
            }).collect()
        }).unwrap_or_default();

        let selected_block_info = if let Some((ref sec_id, ref t_id)) = self.selected_block {
            if t_id == &self.selected_track_id {
                self.last_status.as_ref().and_then(|st| {
                    st.sections.iter()
                        .find(|s| &s.id == sec_id)
                        .and_then(|s| s.track_blocks.get(t_id).map(|b| (s.id.clone(), s.name.clone(), b.clone())))
                })
            } else {
                None
            }
        } else {
            None
        };

        ui.horizontal(|ui| {
            ui.heading("🎹 Interactive Piano Roll");
            ui.separator();

            // Track Selector
            ui.label("Track:");
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

        ui.add_space(3.0);

        // Scope Selector: Full Track vs Specific Section Block
        ui.horizontal(|ui| {
            ui.label("Scope:");
            let is_full = selected_block_info.is_none();
            if ui.selectable_label(is_full, "🌐 Full Track Timeline").on_hover_text("View and edit the full song timeline across all sections").clicked() {
                self.selected_block = None;
            }

            if available_blocks.is_empty() {
                ui.small("(No blocks in this track. Click '➕ Add Block' in the Arranger grid to create a section block.)");
            } else {
                for (sec_id, sec_name, blk) in &available_blocks {
                    let is_this = self.selected_block.as_ref() == Some(&(sec_id.clone(), self.selected_track_id.clone()));
                    let label = format!("🔲 {} ({})", blk.id, sec_name);
                    let resp = ui.selectable_label(is_this, label);
                    if resp.clicked() {
                        self.selected_block = Some((sec_id.clone(), self.selected_track_id.clone()));
                    }
                }
            }

            if let Some((_, ref sec_name, ref blk)) = selected_block_info {
                ui.separator();
                let bars = (blk.base_length_ticks as f32 / 3840.0).max(0.25);
                ui.label(egui::RichText::new(format!("Active: {} (Section: {}, Length: {:.1} bars / {}t)", blk.id, sec_name, bars, blk.base_length_ticks)).color(egui::Color32::from_rgb(255, 215, 0)).strong());
            }
        });

        ui.add_space(6.0);

        let (total_ticks, track_events, is_block_scoped) = if let Some((_, _, ref blk)) = selected_block_info {
            (blk.base_length_ticks.max(240), blk.events.clone(), true)
        } else {
            let total = self.last_status.as_ref().map(|s| s.total_ticks).unwrap_or(15360);
            let events = self.last_status.as_ref().map(|s| s.events.clone()).unwrap_or_default();
            let filtered: Vec<jamseq::timeline::ScheduledEvent> = events
                .into_iter()
                .filter(|e| e.track_id == self.selected_track_id)
                .collect();
            (total, filtered, false)
        };

        let current_tick = self.last_status.as_ref().map(|s| s.current_tick).unwrap_or(0);

        // 8 Full Octaves displayed (C0 = 12 to C8 = 108)
        let min_note = 12u8; // C0
        let max_note = 108u8; // C8
        let num_notes = (max_note - min_note + 1) as usize;
        let row_height = 14.0f32;

        // Octave Jump and Viewport Navigation Toolbar
        ui.horizontal(|ui| {
            ui.label("Jump:");
            if ui.small_button("Sub (C1)").on_hover_text("Jump to C1 (MIDI 24)").clicked() {
                self.piano_roll_scroll_y = Some(((max_note - 24) as f32 * row_height - 180.0).max(0.0));
            }
            if ui.small_button("Bass (C2)").on_hover_text("Jump to C2 (MIDI 36)").clicked() {
                self.piano_roll_scroll_y = Some(((max_note - 36) as f32 * row_height - 180.0).max(0.0));
            }
            if ui.small_button("Tenor (C3)").on_hover_text("Jump to C3 (MIDI 48)").clicked() {
                self.piano_roll_scroll_y = Some(((max_note - 48) as f32 * row_height - 180.0).max(0.0));
            }
            if ui.small_button("Mid (C4)").on_hover_text("Jump to Middle C (MIDI 60)").clicked() {
                self.piano_roll_scroll_y = Some(((max_note - 60) as f32 * row_height - 180.0).max(0.0));
            }
            if ui.small_button("Treble (C5)").on_hover_text("Jump to C5 (MIDI 72)").clicked() {
                self.piano_roll_scroll_y = Some(((max_note - 72) as f32 * row_height - 180.0).max(0.0));
            }
            if ui.small_button("High (C6)").on_hover_text("Jump to C6 (MIDI 84)").clicked() {
                self.piano_roll_scroll_y = Some(((max_note - 84) as f32 * row_height - 180.0).max(0.0));
            }
            if ui.small_button("Air (C7)").on_hover_text("Jump to C7 (MIDI 96)").clicked() {
                self.piano_roll_scroll_y = Some(((max_note - 96) as f32 * row_height - 180.0).max(0.0));
            }

            ui.separator();

            let pitches: Vec<u8> = track_events.iter().filter_map(|e| {
                if let jamseq::timeline::ScheduledAction::NoteOn { note, .. } = e.action {
                    Some(note)
                } else {
                    None
                }
            }).collect();

            let focus_tooltip = if pitches.is_empty() {
                "No notes on this track yet (will center on Middle C)".to_string()
            } else {
                let min_p = pitches.iter().min().unwrap();
                let max_p = pitches.iter().max().unwrap();
                format!("Center viewport on active notes (range: MIDI {} - {})", min_p, max_p)
            };

            if ui.small_button("🎯 Focus Notes").on_hover_text(focus_tooltip).clicked() {
                if !pitches.is_empty() {
                    let avg = pitches.iter().map(|&p| p as f32).sum::<f32>() / pitches.len() as f32;
                    self.piano_roll_scroll_y = Some(((max_note as f32 - avg) * row_height - 180.0).max(0.0));
                } else {
                    self.piano_roll_scroll_y = Some(((max_note - 60) as f32 * row_height - 180.0).max(0.0));
                }
            }

            ui.separator();
            ui.small("Range: C0 (12) - C8 (108)");
        });

        ui.add_space(4.0);

        let mut scroll = egui::ScrollArea::both();
        if let Some(offset_y) = self.piano_roll_scroll_y.take() {
            scroll = scroll.vertical_scroll_offset(offset_y);
        }

        scroll.show(ui, |ui| {
            ui.horizontal(|ui| {
                // Left Keyboard Column (Piano Keys)
                let keyboard_width = 54.0;
                let (key_rect, key_resp) = ui.allocate_exact_size(
                    egui::vec2(keyboard_width, num_notes as f32 * row_height),
                    egui::Sense::click(),
                );

                if key_resp.clicked() {
                    if let Some(pos) = key_resp.interact_pointer_pos() {
                        let row_idx = ((pos.y - key_rect.top()) / row_height).floor() as u8;
                        let clicked_pitch = max_note.saturating_sub(row_idx);
                        self.piano_roll_note_pitch = clicked_pitch;
                    }
                }

                let key_painter = ui.painter();
                let note_names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

                for i in 0..num_notes {
                    let pitch = max_note - i as u8;
                    let y = key_rect.top() + i as f32 * row_height;
                    let r = egui::Rect::from_min_size(egui::pos2(key_rect.left(), y), egui::vec2(keyboard_width, row_height));

                    let is_c = pitch % 12 == 0;
                    let is_black = [1, 3, 6, 8, 10].contains(&(pitch % 12));
                    let is_active_pitch = pitch == self.piano_roll_note_pitch;

                    let key_bg = if is_active_pitch {
                        egui::Color32::from_rgb(65, 130, 220)
                    } else if is_c {
                        egui::Color32::from_rgb(240, 240, 252)
                    } else if is_black {
                        egui::Color32::from_rgb(28, 28, 34)
                    } else {
                        egui::Color32::from_rgb(215, 215, 222)
                    };
                    key_painter.rect_filled(r, 1.0, key_bg);

                    if is_c {
                        key_painter.line_segment(
                            [egui::pos2(r.left(), r.bottom()), egui::pos2(r.right(), r.bottom())],
                            egui::Stroke::new(1.5, egui::Color32::from_rgb(80, 140, 240)),
                        );
                    }

                    let name = note_names[(pitch % 12) as usize];
                    let oct = (pitch / 12) as i32 - 1;
                    let text_color = if is_active_pitch {
                        egui::Color32::WHITE
                    } else if is_black {
                        egui::Color32::LIGHT_GRAY
                    } else if is_c {
                        egui::Color32::from_rgb(15, 45, 130)
                    } else {
                        egui::Color32::BLACK
                    };
                    key_painter.text(
                        egui::pos2(r.left() + 4.0, r.top() + 1.0),
                        egui::Align2::LEFT_TOP,
                        format!("{}{}", name, oct),
                        egui::FontId::proportional(if is_c { 10.5 } else { 9.5 }),
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
                        if is_block_scoped {
                            if let Some((ref s_id, _, ref blk)) = selected_block_info {
                                let mut updated = blk.clone();
                                updated.add_note(now_ms, now_ms + 1, start_tick, final_duration, pitch, 0.9);
                                self.send_cmd(IpcCommand::SetTrackBlock { section_id: s_id.clone(), block: updated });
                                self.status_msg = format!("Block '{}': Added note {} at tick {} (length: {} ticks)", blk.id, pitch, start_tick, final_duration);
                            }
                        } else {
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
                        if is_block_scoped {
                            if let Some((ref s_id, _, ref blk)) = selected_block_info {
                                let mut updated = blk.clone();
                                updated.add_note(now_ms, now_ms + 1, final_tick, self.note_duration_ticks, clicked_pitch, 0.9);
                                self.send_cmd(IpcCommand::SetTrackBlock { section_id: s_id.clone(), block: updated });
                                self.status_msg = format!("Block '{}': Added note {} at tick {} (length: {} ticks)", blk.id, clicked_pitch, final_tick, self.note_duration_ticks);
                            }
                        } else {
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
                    }
                } else if grid_resp.secondary_clicked() {
                    // Right click deletes note
                    if let Some(pos) = grid_resp.interact_pointer_pos() {
                        let pct_x = ((pos.x - grid_rect.left()) / grid_rect.width()).clamp(0.0, 1.0);
                        let clicked_tick = (pct_x * total_ticks as f32) as u64;
                        let row_idx = ((pos.y - grid_rect.top()) / row_height).floor() as u8;
                        let clicked_pitch = max_note.saturating_sub(row_idx);

                        if is_block_scoped {
                            if let Some((ref s_id, _, ref blk)) = selected_block_info {
                                let mut updated = blk.clone();
                                updated.events.retain(|e| {
                                    if (e.tick as i64 - clicked_tick as i64).abs() <= 120 {
                                        match e.action {
                                            jamseq::timeline::ScheduledAction::NoteOn { note, .. } => note != clicked_pitch,
                                            jamseq::timeline::ScheduledAction::NoteOff { note } => note != clicked_pitch,
                                            _ => true,
                                        }
                                    } else {
                                        true
                                    }
                                });
                                self.send_cmd(IpcCommand::SetTrackBlock { section_id: s_id.clone(), block: updated });
                                self.status_msg = format!("Block '{}': Deleted note {} near tick {}", blk.id, clicked_pitch, clicked_tick);
                            }
                        } else {
                            self.send_cmd(IpcCommand::DeleteNoteAt {
                                track_id: self.selected_track_id.clone(),
                                tick: clicked_tick,
                                note: clicked_pitch,
                            });
                            self.status_msg = format!("Piano roll: Deleted note {} near tick {}", clicked_pitch, clicked_tick);
                        }
                    }
                }

                let grid_painter = ui.painter();
                // Grid background rows
                for i in 0..num_notes {
                    let pitch = max_note - i as u8;
                    let y = grid_rect.top() + i as f32 * row_height;
                    let r = egui::Rect::from_min_size(egui::pos2(grid_rect.left(), y), egui::vec2(grid_width, row_height));
                    let is_c = pitch % 12 == 0;
                    let is_black = [1, 3, 6, 8, 10].contains(&(pitch % 12));
                    let row_bg = if is_black { egui::Color32::from_rgb(22, 22, 28) } else { egui::Color32::from_rgb(32, 32, 38) };
                    grid_painter.rect_filled(r, 0.0, row_bg);

                    if is_c {
                        grid_painter.line_segment(
                            [egui::pos2(grid_rect.left(), r.bottom()), egui::pos2(grid_rect.right(), r.bottom())],
                            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(80, 140, 240, 50)),
                        );
                    }
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
                let playhead_tick = if is_block_scoped {
                    if let Some((ref s_id, _, ref blk)) = selected_block_info {
                        let sec_pos = self.last_status.as_ref().and_then(|st| {
                            let mut acc = 0u64;
                            for s in &st.sections {
                                if &s.id == s_id {
                                    return Some((acc, s.length_ticks()));
                                }
                                acc += s.length_ticks();
                            }
                            None
                        });
                        if let Some((sec_start, _sec_len)) = sec_pos {
                            let blk_start = sec_start + blk.start_offset_ticks;
                            let blk_end = blk_start + blk.effective_length_ticks();
                            if current_tick >= blk_start && current_tick < blk_end && blk.base_length_ticks > 0 {
                                Some((current_tick - blk_start) % blk.base_length_ticks)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    Some(current_tick)
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
            });
        });
    }
}
