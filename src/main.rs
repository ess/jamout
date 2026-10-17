mod arranger;
mod inspector;
mod mixer;
mod piano_roll;
mod search_input;
mod sliding_panel;

use eframe::egui;
use jamseq::ipc::{IpcCommand, IpcResponse, IpcStatusResponse};
use jamseq::timeline::{TempoOverride, TimeSignature};
use piano_roll::{OpenPianoRollTarget, PianoRollAction, PianoRollState};
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

#[derive(Clone, Debug, PartialEq)]
pub enum BlockDragKind {
    Slide,
    Slip,
    Resize,
}

#[derive(Clone, Debug)]
pub struct BlockDragState {
    pub section_id: String,
    pub track_id: String,
    pub initial_offset: u64,
    pub initial_length: u64,
    pub start_pos_x: f32,
    pub kind: BlockDragKind,
}

#[derive(Clone, Debug)]
pub struct TimelineBlockDragState {
    pub block_id: String,
    pub track_id: String,
    pub initial_start_tick: u64,
    pub initial_length_ticks: u64,
    pub start_pos_x: f32,
    pub kind: BlockDragKind,
}

pub struct JamOutApp {
    pub _socket_path: String,
    pub connected: bool,
    pub last_status: Option<IpcStatusResponse>,
    pub cmd_sender: Sender<IpcCommand>,
    pub status_receiver: Receiver<IpcStatusResponse>,
    
    // UI state
    pub record_out_filename: String,
    pub status_msg: String,
    pub _status_time: Instant,
    pub active_tab: Tab,

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
    pub piano_roll: PianoRollState,

    // Track editing state
    pub track_name_edits: HashMap<String, String>,

    // Live plugin / synth editor modal
    pub edit_track_id: Option<String>,
    pub edit_insert_slot: Option<(String, usize)>, // (track_id, insert_index)
    pub edit_master_insert_slot: Option<usize>,
    pub preset_save_name: String,
    pub param_search_filter: String,
    pub plugin_search_filter: String,
    pub insert_search_filter: String,
    pub effect_param_search_filter: String,

    // Matrix Arranger & Block Editing State
    pub block_drag: Option<BlockDragState>,
    pub show_tempo_dialog: Option<(String, Option<String>)>, // (section_id, Option<track_id>)
    pub tempo_dialog_mode: usize, // 0 = Inherited, 1 = Absolute BPM, 2 = Ratio
    pub tempo_dialog_bpm: f64,
    pub tempo_dialog_num: u32,
    pub tempo_dialog_den: u32,

    // Cascading Meter & Polymeter State
    pub show_meter_dialog: Option<(Option<String>, Option<String>)>, // (Option<section_id>, Option<track_id>)
    pub meter_dialog_inherit: bool,
    pub meter_dialog_num: u32,
    pub meter_dialog_den: u32,

    // Block-Owned Floating Piano Roll state
    pub open_piano_roll: Option<OpenPianoRollTarget>,
    pub timeline_block_drag: Option<TimelineBlockDragState>,

    // Unified Section Properties Modal state
    pub show_section_properties_modal: Option<String>, // section_id
    pub section_modal_name: String,
    pub section_modal_bars: u32,
    pub section_modal_meter_inherit: bool,
    pub section_modal_meter_num: u32,
    pub section_modal_meter_den: u32,
    pub section_modal_tempo_mode: usize, // 0 = Inherited, 1 = Absolute BPM, 2 = Ratio
    pub section_modal_tempo_bpm: f64,
    pub section_modal_tempo_num: u32,
    pub section_modal_tempo_den: u32,
}

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    Timeline,
    Mixer,
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
            _drift_ticks: 20,       // ±20 ticks Gaussian feel
            drag_bias_ticks: 0,    // Default 0 ticks (strict grid alignment)
            freehand_drift: false,
            show_project_dialog: false,
            project_path_input: String::new(),
            new_project_title: "My New Jam".to_string(),
            new_project_bpm: 68.0,
            show_section_dialog: false,
            new_section_name: "Verse 1".to_string(),
            selected_track_id: "bass".to_string(),
            piano_roll: PianoRollState::default(),
            track_name_edits: HashMap::new(),
            edit_track_id: None,
            edit_insert_slot: None,
            edit_master_insert_slot: None,
            preset_save_name: "custom_patch.json".to_string(),
            param_search_filter: String::new(),
            plugin_search_filter: String::new(),
            insert_search_filter: String::new(),
            effect_param_search_filter: String::new(),
            block_drag: None,
            show_tempo_dialog: None,
            tempo_dialog_mode: 0,
            tempo_dialog_bpm: 120.0,
            tempo_dialog_num: 3,
            tempo_dialog_den: 2,
            show_meter_dialog: None,
            meter_dialog_inherit: true,
            meter_dialog_num: 4,
            meter_dialog_den: 4,
            open_piano_roll: None,
            timeline_block_drag: None,
            show_section_properties_modal: None,
            section_modal_name: String::new(),
            section_modal_bars: 4,
            section_modal_meter_inherit: true,
            section_modal_meter_num: 4,
            section_modal_meter_den: 4,
            section_modal_tempo_mode: 0,
            section_modal_tempo_bpm: 120.0,
            section_modal_tempo_num: 3,
            section_modal_tempo_den: 2,
        }
    }

    pub fn send_cmd(&self, cmd: IpcCommand) {
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

    pub fn prompt_import_midi(&mut self, target_track_id: Option<String>, target_section_id: Option<String>) {
        let dialog = rfd::FileDialog::new()
            .set_title("Import Standard MIDI File (.mid)")
            .add_filter("Standard MIDI File (*.mid, *.midi)", &["mid", "midi"]);

        if let Some(path) = dialog.pick_file() {
            let path_str = path.to_string_lossy().to_string();
            self.send_cmd(IpcCommand::ImportMidiFile {
                file_path: path_str.clone(),
                target_track_id,
                target_section_id,
            });
            self.status_msg = format!("Imported MIDI: {}", path_str);
        }
    }

    pub fn prompt_export_midi(&mut self) {
        let default_name = self.last_status.as_ref()
            .map(|s| {
                let title = s.project_title.trim();
                if title.is_empty() || title == "Untitled" {
                    "arrangement".to_string()
                } else {
                    title.replace(' ', "_").to_lowercase()
                }
            })
            .unwrap_or_else(|| "arrangement".to_string());
        let default_file = format!("{}.mid", default_name);

        let dialog = rfd::FileDialog::new()
            .set_title("Export Arrangement as Standard MIDI File (.mid)")
            .set_file_name(&default_file)
            .add_filter("Standard MIDI File (*.mid)", &["mid", "midi"]);

        if let Some(path) = dialog.save_file() {
            let mut normalized = path.clone();
            if normalized.extension().and_then(|e| e.to_str()) != Some("mid") {
                normalized.set_extension("mid");
            }
            let path_str = normalized.to_string_lossy().to_string();
            self.send_cmd(IpcCommand::ExportMidiFile { file_path: path_str.clone() });
            self.status_msg = format!("Exported MIDI to: {}", path_str);
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
                if ui.small_button("📥 MIDI").on_hover_text("Import Standard MIDI File (.mid)...").clicked() {
                    self.prompt_import_midi(None, None);
                }
                if ui.small_button("📤 MIDI").on_hover_text("Export Arrangement to Standard MIDI File (.mid)...").clicked() {
                    self.prompt_export_midi();
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
                let project_ts = self.last_status.as_ref().map(|s| s.time_signature).unwrap_or_default();
                let active_ts = self.last_status.as_ref()
                    .and_then(|s| {
                        let cur_id = s.active_section_id.as_deref();
                        s.sections.iter().find(|sec| cur_id == Some(&sec.id))
                            .and_then(|sec| sec.time_signature)
                    })
                    .unwrap_or(project_ts);

                let ticks_per_bar = active_ts.ticks_per_bar().max(1);
                let ticks_per_beat = active_ts.ticks_per_beat().max(1);
                let current_bar = (current_tick / ticks_per_bar) + 1;
                let current_beat = ((current_tick % ticks_per_bar) / ticks_per_beat) + 1;
                let active_meter_str = format!("{}/{}", active_ts.numerator, active_ts.denominator);

                ui.monospace(format!("Bar {:02} : {:02}", current_bar, current_beat));
                ui.separator();
                if ui.button(egui::RichText::new(format!("📏 {}", active_meter_str)).strong())
                    .on_hover_text("Click to configure master project time signature")
                    .clicked()
                {
                    self.show_meter_dialog = Some((None, None));
                    self.meter_dialog_inherit = false;
                    self.meter_dialog_num = project_ts.numerator;
                    self.meter_dialog_den = project_ts.denominator;
                }
                ui.separator();
                ui.monospace(format!("{:.1} BPM", bpm));

                ui.separator();

                // Live Mic Input Meter
                let mic_peak = self.last_status.as_ref().map(|s| s.mic_peak).unwrap_or(0.0);
                ui.label("Mic:");
                let progress_bar = egui::ProgressBar::new(mic_peak.min(1.0))
                    .desired_width(70.0)
                    .show_percentage();
                ui.add(progress_bar);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.selectable_value(&mut self.active_tab, Tab::Mixer, "🎚 Mixer");
                    ui.selectable_value(&mut self.active_tab, Tab::Timeline, "📊 Arranger");
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

                        if ui.button("📥 Import MIDI...").clicked() {
                            self.prompt_import_midi(None, None);
                        }

                        if ui.button("📤 Export MIDI...").clicked() {
                            self.prompt_export_midi();
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

        // Meter & Polymeter Hierarchy Modal
        if let Some((sec_id_opt, track_id_opt)) = self.show_meter_dialog.clone() {
            let project_ts = self.last_status.as_ref().map(|s| s.time_signature).unwrap_or_default();
            let is_block = track_id_opt.is_some();
            let is_section = sec_id_opt.is_some() && track_id_opt.is_none();
            let is_project = sec_id_opt.is_none();

            let title = if let Some(ref tid) = track_id_opt {
                format!("📏 Track Block Meter / Polymeter ({})", tid)
            } else if let Some(ref sid) = sec_id_opt {
                format!("📏 Section Time Signature ({})", sid)
            } else {
                "📏 Master Project Time Signature".to_string()
            };

            let parent_ts = if let Some(ref sid) = sec_id_opt {
                if is_block {
                    self.last_status.as_ref()
                        .and_then(|s| s.sections.iter().find(|sec| &sec.id == sid))
                        .and_then(|sec| sec.time_signature)
                        .unwrap_or(project_ts)
                } else {
                    project_ts
                }
            } else {
                project_ts
            };

            egui::Window::new(title)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.set_width(380.0);

                    if is_project {
                        ui.label(egui::RichText::new("Default Arrangement Time Signature").strong());
                        ui.small("Controls the global bar and beat grid across all sections unless overridden.");
                    } else if is_section {
                        ui.label(egui::RichText::new("Hierarchical Section Meter").strong());
                        ui.small(format!("Master Project Meter: {}/{}", project_ts.numerator, project_ts.denominator));
                    } else {
                        ui.label(egui::RichText::new("Independent Block Polymeter").strong());
                        ui.colored_label(egui::Color32::from_rgb(220, 150, 255), "⚡ Polymetric phrasing: This block runs independently on its own metric grid.");
                        ui.small(format!("Parent Section Meter: {}/{}", parent_ts.numerator, parent_ts.denominator));
                    }
                    ui.add_space(8.0);

                    if !is_project {
                        let inherit_label = if is_block {
                            format!("Inherit from Section ({}/{})", parent_ts.numerator, parent_ts.denominator)
                        } else {
                            format!("Inherit from Project ({}/{})", project_ts.numerator, project_ts.denominator)
                        };
                        ui.radio_value(&mut self.meter_dialog_inherit, true, inherit_label);
                        ui.radio_value(&mut self.meter_dialog_inherit, false, if is_block { "Override (Polymeter)" } else { "Override Time Signature" });
                    }

                    if is_project || !self.meter_dialog_inherit {
                        ui.indent("meter_custom_indent", |ui| {
                            ui.horizontal(|ui| {
                                ui.label("Meter:");
                                ui.add(egui::DragValue::new(&mut self.meter_dialog_num).speed(1).range(1..=32));
                                ui.label("/");
                                egui::ComboBox::from_id_salt("meter_den_combo")
                                    .selected_text(format!("{}", self.meter_dialog_den))
                                    .show_ui(ui, |ui| {
                                        for den in [1, 2, 4, 8, 16, 32] {
                                            ui.selectable_value(&mut self.meter_dialog_den, den, format!("{}", den));
                                        }
                                    });
                            });

                            let preview_ts = TimeSignature::new(self.meter_dialog_num, self.meter_dialog_den);
                            ui.label(
                                egui::RichText::new(format!(
                                    "Resulting Grid: {}/{} (Bar: {} ticks, Beat: {} ticks)",
                                    preview_ts.numerator, preview_ts.denominator, preview_ts.ticks_per_bar(), preview_ts.ticks_per_beat()
                                ))
                                .italics()
                                .color(egui::Color32::from_rgb(180, 215, 255)),
                            );

                            ui.label("Common Presets:");
                            ui.horizontal_wrapped(|ui| {
                                if ui.small_button("4/4").clicked() { self.meter_dialog_num = 4; self.meter_dialog_den = 4; }
                                if ui.small_button("3/4").clicked() { self.meter_dialog_num = 3; self.meter_dialog_den = 4; }
                                if ui.small_button("2/4").clicked() { self.meter_dialog_num = 2; self.meter_dialog_den = 4; }
                                if ui.small_button("6/8").clicked() { self.meter_dialog_num = 6; self.meter_dialog_den = 8; }
                                if ui.small_button("5/4").clicked() { self.meter_dialog_num = 5; self.meter_dialog_den = 4; }
                                if ui.small_button("7/8").clicked() { self.meter_dialog_num = 7; self.meter_dialog_den = 8; }
                                if ui.small_button("9/8").clicked() { self.meter_dialog_num = 9; self.meter_dialog_den = 8; }
                                if ui.small_button("12/8").clicked() { self.meter_dialog_num = 12; self.meter_dialog_den = 8; }
                            });
                        });
                    }

                    ui.add_space(8.0);
                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui.button("Apply Meter").clicked() {
                            let override_ts = if is_project || !self.meter_dialog_inherit {
                                Some(TimeSignature::new(self.meter_dialog_num, self.meter_dialog_den))
                            } else {
                                None
                            };

                            if let Some(ref tid) = track_id_opt {
                                let sid = sec_id_opt.clone().unwrap_or_default();
                                self.send_cmd(IpcCommand::SetBlockTimeSignature {
                                    section_id: sid.clone(),
                                    track_id: tid.clone(),
                                    time_signature: override_ts,
                                });
                                self.status_msg = format!("Updated meter for block '{}' in section '{}'", tid, sid);
                            } else if let Some(ref sid) = sec_id_opt {
                                self.send_cmd(IpcCommand::SetSectionTimeSignature {
                                    section_id: sid.clone(),
                                    time_signature: override_ts,
                                });
                                self.status_msg = format!("Updated meter for section '{}'", sid);
                            } else {
                                if let Some(ts) = override_ts {
                                    self.send_cmd(IpcCommand::SetProjectTimeSignature {
                                        numerator: ts.numerator,
                                        denominator: ts.denominator,
                                    });
                                    self.status_msg = format!("Updated project time signature to {}/{}", ts.numerator, ts.denominator);
                                }
                            }
                            self.show_meter_dialog = None;
                        }

                        if ui.button("Cancel").clicked() {
                            self.show_meter_dialog = None;
                        }
                    });
                });
        }

        // Central Workspace Area
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.active_tab {
                Tab::Timeline => self.render_timeline_view(ui),
                Tab::Mixer => self.render_mixer_view(ui),
            }
        });

        // Live Plugin / Synth Fiddling & Parameter Inspector Modals
        inspector::render_inspector(self, ctx);

        // Floating Piano Roll Window for open block
        if let Some(target) = self.open_piano_roll.clone() {
            let block_exists = self.last_status.as_ref().map(|st| {
                if let Some(ref sec_id) = target.section_id {
                    st.sections.iter().find(|s| &s.id == sec_id)
                        .and_then(|s| s.track_blocks.get(&target.track_id))
                        .map(|b| b.id == target.block_id)
                        .unwrap_or(false)
                } else {
                    st.timeline_blocks.iter().any(|b| b.id == target.block_id && b.track_id == target.track_id)
                }
            }).unwrap_or(false);

            if !block_exists {
                self.open_piano_roll = None;
            } else {
                let mut is_open = true;
                let track_name = self.last_status.as_ref()
                    .and_then(|st| st.tracks.iter().find(|t| t.id == target.track_id))
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| target.track_id.clone());
                let win_title = if let Some(ref sec_id) = target.section_id {
                    format!("🎹 Piano Roll: Track '{}' in Section '{}'", track_name, sec_id)
                } else {
                    format!("🎹 Piano Roll: Clip '{}' ({})", target.block_id, track_name)
                };
                egui::Window::new(win_title)
                    .open(&mut is_open)
                    .default_size([900.0, 520.0])
                    .min_size([600.0, 350.0])
                    .resizable(true)
                    .collapsible(true)
                    .show(ctx, |ui| {
                        self.piano_roll.freehand_drift = self.freehand_drift;
                        self.piano_roll.drag_bias_ticks = self.drag_bias_ticks;
                        let mut actions = Vec::new();
                        self.piano_roll.render(
                            ui,
                            &target,
                            self.last_status.as_ref(),
                            &self.cmd_sender,
                            |act| actions.push(act),
                        );
                        for act in actions {
                            match act {
                                PianoRollAction::OpenMeterDialog { section_id, track_id } => {
                                    self.show_meter_dialog = Some((section_id.clone(), track_id.clone()));
                                    if let (Some(sec_id), Some(trk_id)) = (section_id.as_ref(), track_id.as_ref()) {
                                        let block_opt = self.last_status.as_ref().and_then(|st| {
                                            st.sections.iter().find(|s| &s.id == sec_id)
                                                .and_then(|s| s.track_blocks.get(trk_id))
                                        });
                                        let sec_ts = self.last_status.as_ref().and_then(|st| {
                                            st.sections.iter().find(|s| &s.id == sec_id)
                                                .map(|s| s.effective_time_signature(st.time_signature))
                                        }).unwrap_or(TimeSignature::default());

                                        if let Some(block) = block_opt {
                                            if let Some(ts) = block.time_signature {
                                                self.meter_dialog_inherit = false;
                                                self.meter_dialog_num = ts.numerator;
                                                self.meter_dialog_den = ts.denominator;
                                            } else {
                                                self.meter_dialog_inherit = true;
                                                self.meter_dialog_num = sec_ts.numerator;
                                                self.meter_dialog_den = sec_ts.denominator;
                                            }
                                        }
                                    }
                                }
                                PianoRollAction::OpenTempoDialog { section_id, track_id } => {
                                    self.show_tempo_dialog = Some((section_id.clone(), track_id.clone()));
                                    if let Some(trk_id) = track_id.as_ref() {
                                        let block_opt = self.last_status.as_ref().and_then(|st| {
                                            st.sections.iter().find(|s| &s.id == &section_id)
                                                .and_then(|s| s.track_blocks.get(trk_id))
                                        });
                                        let sec_bpm = self.last_status.as_ref().and_then(|st| {
                                            st.sections.iter().find(|s| &s.id == &section_id)
                                                .map(|s| s.effective_bpm(st.bpm))
                                        }).unwrap_or(120.0);
                                        let eff_block_bpm = block_opt.map(|b| b.effective_bpm(sec_bpm)).unwrap_or(sec_bpm);

                                        if let Some(block) = block_opt {
                                            match block.tempo {
                                                Some(TempoOverride::AbsoluteBpm(b)) => {
                                                    self.tempo_dialog_mode = 1;
                                                    self.tempo_dialog_bpm = b;
                                                }
                                                Some(TempoOverride::Ratio(n, d)) => {
                                                    self.tempo_dialog_mode = 2;
                                                    self.tempo_dialog_bpm = eff_block_bpm;
                                                    self.tempo_dialog_num = n;
                                                    self.tempo_dialog_den = d;
                                                }
                                                None => {
                                                    self.tempo_dialog_mode = 0;
                                                    self.tempo_dialog_bpm = sec_bpm;
                                                }
                                            }
                                        }
                                    }
                                }
                                PianoRollAction::StatusMsg(msg) => {
                                    self.status_msg = msg;
                                    self._status_time = Instant::now();
                                }
                            }
                        }
                    });
                if !is_open {
                    self.open_piano_roll = None;
                }
            }
        }

        // Unified Section Properties Modal
        if let Some(sec_id) = self.show_section_properties_modal.clone() {
            self.render_section_properties_modal(ctx, &sec_id);
        }
    }
}

pub const TRACK_HEADER_WIDTH: f32 = 240.0;
pub const SECTION_HEADER_HEIGHT: f32 = 56.0;
pub const TRACK_ROW_HEIGHT: f32 = 64.0;
pub const MIN_SECTION_COL_WIDTH: f32 = 220.0;
pub const TICKS_PER_STANDARD_BAR: f32 = 3840.0;

pub fn compute_section_col_width(sec_len_ticks: u64, zoom: f32) -> f32 {
    let proportional = (sec_len_ticks as f32 / TICKS_PER_STANDARD_BAR) * 160.0 * zoom;
    proportional.max(MIN_SECTION_COL_WIDTH)
}

impl JamOutApp {
    pub fn open_section_properties(&mut self, sec: &jamseq::SongSection) {
        let project_bpm = self.last_status.as_ref().map(|s| s.bpm).unwrap_or(120.0);
        let project_ts = self.last_status.as_ref().map(|s| s.time_signature).unwrap_or_default();

        self.section_modal_name = sec.name.clone();
        let tpb = sec.time_signature.unwrap_or(project_ts).ticks_per_bar().max(1);
        self.section_modal_bars = if sec.bars > 0 {
            sec.bars
        } else if sec.length_ticks() > 0 {
            (sec.length_ticks() as f64 / tpb as f64).ceil() as u32
        } else {
            4
        };

        if let Some(ts) = sec.time_signature {
            self.section_modal_meter_inherit = false;
            self.section_modal_meter_num = ts.numerator;
            self.section_modal_meter_den = ts.denominator;
        } else {
            self.section_modal_meter_inherit = true;
            self.section_modal_meter_num = project_ts.numerator;
            self.section_modal_meter_den = project_ts.denominator;
        }

        match sec.tempo {
            Some(TempoOverride::AbsoluteBpm(b)) => {
                self.section_modal_tempo_mode = 1;
                self.section_modal_tempo_bpm = b;
                self.section_modal_tempo_num = 1;
                self.section_modal_tempo_den = 1;
            }
            Some(TempoOverride::Ratio(n, d)) => {
                self.section_modal_tempo_mode = 2;
                self.section_modal_tempo_bpm = sec.effective_bpm(project_bpm);
                self.section_modal_tempo_num = n;
                self.section_modal_tempo_den = d;
            }
            None => {
                self.section_modal_tempo_mode = 0;
                self.section_modal_tempo_bpm = project_bpm;
                self.section_modal_tempo_num = 1;
                self.section_modal_tempo_den = 1;
            }
        }

        self.show_section_properties_modal = Some(sec.id.clone());
    }

    fn render_section_properties_modal(&mut self, ctx: &egui::Context, sec_id: &str) {
        egui::Window::new(format!("⚙ Section Properties: {}", self.section_modal_name))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("Section Properties");
                ui.add_space(8.0);

                // Section Name
                ui.horizontal(|ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut self.section_modal_name);
                });

                // Bars
                ui.horizontal(|ui| {
                    ui.label("Base Length (Bars):");
                    ui.add(egui::DragValue::new(&mut self.section_modal_bars).speed(1).range(1..=128));
                });
                ui.small("Sets nominal bar length when empty. If blocks inside exceed this, section expands automatically.");

                ui.add_space(8.0);
                ui.separator();

                // Time Signature
                ui.label(egui::RichText::new("Time Signature").strong());
                let project_ts = self.last_status.as_ref().map(|s| s.time_signature).unwrap_or_default();
                let inherit_ts_label = format!("Inherit from Project ({}/{})", project_ts.numerator, project_ts.denominator);
                ui.radio_value(&mut self.section_modal_meter_inherit, true, inherit_ts_label);
                ui.radio_value(&mut self.section_modal_meter_inherit, false, "Custom Section Meter");

                if !self.section_modal_meter_inherit {
                    ui.indent("sec_meter_indent", |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Meter:");
                            ui.add(egui::DragValue::new(&mut self.section_modal_meter_num).speed(1).range(1..=32));
                            ui.label("/");
                            egui::ComboBox::from_id_salt("sec_prop_meter_den")
                                .selected_text(format!("{}", self.section_modal_meter_den))
                                .show_ui(ui, |ui| {
                                    for den in [1, 2, 4, 8, 16, 32] {
                                        ui.selectable_value(&mut self.section_modal_meter_den, den, format!("{}", den));
                                    }
                                });
                        });
                        ui.horizontal_wrapped(|ui| {
                            if ui.small_button("4/4").clicked() { self.section_modal_meter_num = 4; self.section_modal_meter_den = 4; }
                            if ui.small_button("3/4").clicked() { self.section_modal_meter_num = 3; self.section_modal_meter_den = 4; }
                            if ui.small_button("6/8").clicked() { self.section_modal_meter_num = 6; self.section_modal_meter_den = 8; }
                            if ui.small_button("7/8").clicked() { self.section_modal_meter_num = 7; self.section_modal_meter_den = 8; }
                            if ui.small_button("5/4").clicked() { self.section_modal_meter_num = 5; self.section_modal_meter_den = 4; }
                        });
                    });
                }

                ui.add_space(8.0);
                ui.separator();

                // Tempo & Metric Modulation
                ui.label(egui::RichText::new("Tempo & Metric Modulation").strong());
                let project_bpm = self.last_status.as_ref().map(|s| s.bpm).unwrap_or(120.0);
                let inherit_bpm_label = format!("Inherit from Project ({:.1} BPM)", project_bpm);
                ui.radio_value(&mut self.section_modal_tempo_mode, 0, inherit_bpm_label);
                ui.radio_value(&mut self.section_modal_tempo_mode, 1, "Absolute BPM");
                if self.section_modal_tempo_mode == 1 {
                    ui.indent("sec_abs_bpm_indent", |ui| {
                        ui.horizontal(|ui| {
                            ui.label("BPM:");
                            ui.add(egui::DragValue::new(&mut self.section_modal_tempo_bpm).speed(0.5).range(20.0..=300.0));
                        });
                    });
                }

                ui.radio_value(&mut self.section_modal_tempo_mode, 2, "Ratio (Metric Modulation)");
                if self.section_modal_tempo_mode == 2 {
                    ui.indent("sec_ratio_bpm_indent", |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Ratio:");
                            ui.add(egui::DragValue::new(&mut self.section_modal_tempo_num).speed(1).range(1..=16));
                            ui.label(":");
                            ui.add(egui::DragValue::new(&mut self.section_modal_tempo_den).speed(1).range(1..=16));
                        });
                        let eff = if self.section_modal_tempo_den > 0 {
                            project_bpm * (self.section_modal_tempo_num as f64 / self.section_modal_tempo_den as f64)
                        } else {
                            project_bpm
                        };
                        ui.label(
                            egui::RichText::new(format!(
                                "Resulting Tempo: {}:{} ({:.1} BPM)",
                                self.section_modal_tempo_num, self.section_modal_tempo_den, eff
                            ))
                            .italics()
                            .color(egui::Color32::from_rgb(255, 180, 50)),
                        );
                        ui.horizontal_wrapped(|ui| {
                            if ui.small_button("3:2 (1.5x)").clicked() { self.section_modal_tempo_num = 3; self.section_modal_tempo_den = 2; }
                            if ui.small_button("4:3 (1.33x)").clicked() { self.section_modal_tempo_num = 4; self.section_modal_tempo_den = 3; }
                            if ui.small_button("1:2 (0.5x)").clicked() { self.section_modal_tempo_num = 1; self.section_modal_tempo_den = 2; }
                            if ui.small_button("2:1 (2.0x)").clicked() { self.section_modal_tempo_num = 2; self.section_modal_tempo_den = 1; }
                        });
                    });
                }

                ui.add_space(10.0);
                ui.separator();

                // Action Buttons
                ui.horizontal(|ui| {
                    if ui.button("Apply Changes").clicked() {
                        if let Some(st) = &self.last_status {
                            if let Some(sec) = st.sections.iter().find(|s| s.id == sec_id) {
                                let mut updated = sec.clone();
                                if !self.section_modal_name.trim().is_empty() {
                                    updated.name = self.section_modal_name.trim().to_string();
                                }
                                updated.bars = self.section_modal_bars;
                                let ts = if self.section_modal_meter_inherit {
                                    None
                                } else {
                                    Some(TimeSignature::new(self.section_modal_meter_num, self.section_modal_meter_den))
                                };
                                let eff_ts = ts.unwrap_or(project_ts);
                                updated.time_signature = ts;
                                updated.length_ticks = self.section_modal_bars as u64 * eff_ts.ticks_per_bar();
                                updated.tempo = match self.section_modal_tempo_mode {
                                    1 => Some(TempoOverride::AbsoluteBpm(self.section_modal_tempo_bpm)),
                                    2 => Some(TempoOverride::Ratio(self.section_modal_tempo_num, self.section_modal_tempo_den)),
                                    _ => None,
                                };

                                self.send_cmd(IpcCommand::UpdateSection { section: updated });
                                self.status_msg = format!("Updated section '{}' properties", self.section_modal_name);
                            }
                        }
                        self.show_section_properties_modal = None;
                    }

                    if ui.button("Cancel").clicked() {
                        self.show_section_properties_modal = None;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("🗑 Delete Section").clicked() {
                            self.send_cmd(IpcCommand::RemoveSection { section_id: sec_id.to_string() });
                            self.status_msg = format!("Removed section '{}'", sec_id);
                            self.show_section_properties_modal = None;
                        }
                    });
                });
            });
    }

    fn render_timeline_view(&mut self, ui: &mut egui::Ui) {
        arranger::render_arranger(self, ui);
    }



    fn render_mixer_view(&mut self, ui: &mut egui::Ui) {
        mixer::render_mixer(self, ui);
    }
}
