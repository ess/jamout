mod piano_roll;
mod search_input;
mod sliding_panel;

use eframe::egui;
use jamseq::ipc::{IpcCommand, IpcResponse, IpcStatusResponse, ParamType, TrackParamStatus};
use jamseq::timeline::{LoopMode, TempoOverride, TimeSignature, TrackArrangerMode, TrackBlock};
use piano_roll::{OpenPianoRollTarget, PianoRollAction, PianoRollState};
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
enum Tab {
    Timeline,
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

fn render_grouped_parameter_list<F>(
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
                                if ui.add(egui::Slider::new(&mut val, min_i..=max_i).step_by(1.0)).changed() {
                                    on_change(&param.name, val as f32);
                                }
                                if let Some(ref disp) = param.display_value {
                                    ui.monospace(disp);
                                }
                            }
                            ParamType::Float => {
                                let mut val = param.value;
                                if ui.add(egui::Slider::new(&mut val, param.min..=param.max)).changed() {
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
                    ui.selectable_value(&mut self.active_tab, Tab::Teleprompter, "🎙 Lyrics");
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

                            egui::ScrollArea::vertical()
                                .max_height(280.0)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    render_grouped_parameter_list(
                                        ui,
                                        &format!("track_{}", track.id),
                                        &track.params,
                                        &self.param_search_filter,
                                        |name, val| {
                                            self.send_cmd(IpcCommand::SetTrackParam {
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
                                            ui.horizontal(|ui| {
                                                ui.label(format!("Parameters ({})", ins.params.len()));
                                                if ins.params.len() > 6 {
                                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                        ui.add(
                                                            SearchInput::new(&mut self.effect_param_search_filter, "🔍 Search parameters...")
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
                                                        &self.effect_param_search_filter,
                                                        |name, val| {
                                                            self.send_cmd(IpcCommand::SetTrackInsertParam {
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

        // Master Bus Insert Effect Parameter Inspector Modal
        if let Some(ins_idx) = self.edit_master_insert_slot {
            let master_ins_opt = self.last_status.as_ref().and_then(|s| {
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
                                            SearchInput::new(&mut self.effect_param_search_filter, "🔍 Search parameters...")
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
                                        &self.effect_param_search_filter,
                                        |name, val| {
                                            self.send_cmd(IpcCommand::SetMasterInsertParam {
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
                    self.edit_master_insert_slot = None;
                }
            } else {
                self.edit_master_insert_slot = None;
            }
        }

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
    fn open_section_properties(&mut self, sec: &jamseq::SongSection) {
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
        let total_ticks = self.last_status.as_ref().map(|s| s.total_ticks).unwrap_or(0);
        let current_tick = self.last_status.as_ref().map(|s| s.current_tick).unwrap_or(0);
        let sections = self.last_status.as_ref().map(|s| s.sections.clone()).unwrap_or_default();
        let tracks = self.last_status.as_ref().map(|s| s.tracks.clone()).unwrap_or_default();
        let project_bpm = self.last_status.as_ref().map(|s| s.bpm).unwrap_or(120.0);
        let project_ts = self.last_status.as_ref().map(|s| s.time_signature).unwrap_or_default();

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

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("➕ Add Section").on_hover_text("Define or append a new song section").clicked() {
                    self.show_section_dialog = true;
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
                                    self.open_section_properties(sec);
                                }
                                if ui.small_button("▶").on_hover_text("Seek playhead to section downbeat").clicked() {
                                    self.send_cmd(IpcCommand::Seek { tick: sec_start_tick });
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
                        self.show_section_dialog = true;
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
                            let current_buf = self.track_name_edits.entry(track.id.clone()).or_insert_with(|| track.name.clone());
                            let resp = ui.add(egui::TextEdit::singleline(current_buf).desired_width(TRACK_HEADER_WIDTH - 20.0));
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

                            let mode_str = match track.arranger_mode {
                                TrackArrangerMode::Section => "Matrix",
                                TrackArrangerMode::Timeline => "Timeline",
                            };
                            if ui.small_button(format!("[{}]", mode_str)).on_hover_text("Switch track arranger mode between Section Matrix and continuous Timeline").clicked() {
                                let new_mode = match track.arranger_mode {
                                    TrackArrangerMode::Section => TrackArrangerMode::Timeline,
                                    TrackArrangerMode::Timeline => TrackArrangerMode::Section,
                                };
                                self.send_cmd(IpcCommand::SetTrackArrangerMode { track_id: track.id.clone(), mode: new_mode });
                            }

                            if ui.small_button("🎹").on_hover_text("Open Piano Roll for this track").clicked() {
                                if track.arranger_mode == TrackArrangerMode::Timeline {
                                    if let Some(tb) = self.last_status.as_ref().and_then(|st| st.timeline_blocks.iter().find(|b| b.track_id == track.id)) {
                                        self.open_piano_roll = Some(OpenPianoRollTarget {
                                            track_id: track.id.clone(),
                                            block_id: tb.id.clone(),
                                            section_id: None,
                                        });
                                        self.selected_track_id = track.id.clone();
                                    } else {
                                        self.status_msg = format!("Track '{}' has no timeline clips yet. Click in the timeline lane to create one.", track.name);
                                    }
                                } else {
                                    if let Some((s_id, blk_id)) = sections.iter().find_map(|s| s.track_blocks.get(&track.id).map(|b| (s.id.clone(), b.id.clone()))) {
                                        self.open_piano_roll = Some(OpenPianoRollTarget {
                                            track_id: track.id.clone(),
                                            block_id: blk_id,
                                            section_id: Some(s_id),
                                        });
                                        self.selected_track_id = track.id.clone();
                                    } else {
                                        self.status_msg = format!("Track '{}' has no blocks yet. Click '+ Add Block' in a section.", track.name);
                                    }
                                }
                            }
                            if ui.small_button("🎛").on_hover_text("Tweak Instruments & DSP").clicked() {
                                self.edit_master_insert_slot = None;
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

                        let track_timeline_blocks: Vec<TrackBlock> = self.last_status.as_ref()
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

                            let is_open_in_piano_roll = self.open_piano_roll.as_ref().map(|t| t.section_id.is_none() && t.block_id == block.id && t.track_id == block.track_id).unwrap_or(false);
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
                            let is_resizing_this = self.timeline_block_drag.as_ref().map(|d| d.block_id == block.id && d.kind == BlockDragKind::Resize).unwrap_or(false);
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
                                self.selected_track_id = track.id.clone();
                                self.open_piano_roll = Some(OpenPianoRollTarget {
                                    track_id: block.track_id.clone(),
                                    block_id: block.id.clone(),
                                    section_id: None,
                                });
                            }
                            if del_resp.clicked() {
                                self.send_cmd(IpcCommand::RemoveBlock {
                                    track_id: block.track_id.clone(),
                                    block_id: block.id.clone(),
                                    section_id: None,
                                });
                                if self.open_piano_roll.as_ref().map(|t| t.section_id.is_none() && t.block_id == block.id).unwrap_or(false) {
                                    self.open_piano_roll = None;
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
                                    self.timeline_block_drag = Some(TimelineBlockDragState {
                                        block_id: hit_block.id.clone(),
                                        track_id: track.id.clone(),
                                        initial_start_tick: hit_block.start_offset_ticks,
                                        initial_length_ticks: hit_block.base_length_ticks,
                                        start_pos_x: pos.x,
                                        kind,
                                    });
                                    self.selected_track_id = track.id.clone();
                                }
                            }
                        }

                        if lane_resp.dragged() {
                            if let Some(ref drag) = self.timeline_block_drag.clone() {
                                if drag.track_id == track.id {
                                    if let Some(pos) = pointer_pos {
                                        let delta_px = pos.x - drag.start_pos_x;
                                        let ticks_per_px = total_timeline_ticks as f32 / lane_rect.width();
                                        let delta_ticks = (delta_px * ticks_per_px) as i64;
                                        let shift_held = ui.input(|i| i.modifiers.shift);
                                        let snap_quantum = 240i64; // 1/16th snap
                                        let snapped_delta = if shift_held || self.freehand_drift {
                                            delta_ticks
                                        } else {
                                            ((delta_ticks + snap_quantum / 2) / snap_quantum) * snap_quantum
                                        };

                                        if let Some(mut block) = track_timeline_blocks.iter().find(|b| b.id == drag.block_id).cloned() {
                                            match drag.kind {
                                                BlockDragKind::Resize => {
                                                    let new_len = (drag.initial_length_ticks as i64 + snapped_delta).max(240) as u64;
                                                    block.base_length_ticks = new_len;
                                                    self.send_cmd(IpcCommand::UpdateBlock { block });
                                                }
                                                BlockDragKind::Slip => {
                                                    for ev in &mut block.events {
                                                        ev.tick = (ev.tick as i64 + snapped_delta).max(0) as u64;
                                                    }
                                                    self.send_cmd(IpcCommand::UpdateBlock { block });
                                                }
                                                BlockDragKind::Slide => {
                                                    let new_start = (drag.initial_start_tick as i64 + snapped_delta).max(0) as u64;
                                                    block.start_offset_ticks = new_start;
                                                    self.send_cmd(IpcCommand::UpdateBlock { block });
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        if lane_resp.drag_stopped() {
                            self.timeline_block_drag = None;
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
                                    self.selected_track_id = track.id.clone();
                                    self.open_piano_roll = Some(OpenPianoRollTarget {
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
                                    self.send_cmd(IpcCommand::AddBlock {
                                        track_id: track.id.clone(),
                                        block_id: Some(block_id.clone()),
                                        section_id: None,
                                        start_tick: Some(new_start),
                                        length_ticks: Some(3840),
                                    });
                                    self.selected_track_id = track.id.clone();
                                    self.open_piano_roll = Some(OpenPianoRollTarget {
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
                                let is_open_in_piano_roll = self.open_piano_roll.as_ref().map(|t| t.section_id.as_deref() == Some(&sec.id) && t.track_id == track.id).unwrap_or(false);

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
                                let is_resizing_this = self.block_drag.as_ref().map(|d| d.section_id == sec.id && d.track_id == track.id && d.kind == BlockDragKind::Resize).unwrap_or(false);
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
                                    self.selected_track_id = track.id.clone();
                                    self.open_piano_roll = Some(OpenPianoRollTarget {
                                        track_id: track.id.clone(),
                                        block_id: block.id.clone(),
                                        section_id: Some(sec.id.clone()),
                                    });
                                }
                                if del_resp.clicked() {
                                    self.send_cmd(IpcCommand::RemoveBlock {
                                        track_id: track.id.clone(),
                                        block_id: block.id.clone(),
                                        section_id: Some(sec.id.clone()),
                                    });
                                    if self.open_piano_roll.as_ref().map(|t| t.section_id.as_deref() == Some(&sec.id) && t.track_id == track.id).unwrap_or(false) {
                                        self.open_piano_roll = None;
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
                                        self.selected_track_id = track.id.clone();
                                        self.open_piano_roll = Some(OpenPianoRollTarget {
                                            track_id: track.id.clone(),
                                            block_id: block.id.clone(),
                                            section_id: Some(sec.id.clone()),
                                        });
                                    } else if cell_resp.clicked() {
                                        self.selected_track_id = track.id.clone();
                                    }

                                    if cell_resp.drag_started() {
                                        self.selected_track_id = track.id.clone();
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
                                        self.block_drag = Some(BlockDragState {
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
                                    if let Some(ref drag) = self.block_drag.clone() {
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
                                                        let snapped_delta = if shift_held || self.freehand_drift {
                                                            delta_ticks
                                                        } else {
                                                            ((delta_ticks + tp_beat as i64 / 2) / tp_beat as i64) * tp_beat as i64
                                                        };
                                                        let new_length = (drag.initial_length as i64 + snapped_delta).max(tp_beat as i64) as u64;
                                                        let mut updated = block.clone();
                                                        updated.base_length_ticks = new_length;
                                                        self.send_cmd(IpcCommand::UpdateBlock {
                                                            block: updated,
                                                        });
                                                    }
                                                    BlockDragKind::Slip => {
                                                        let snapped_delta = if shift_held || self.freehand_drift {
                                                            delta_ticks
                                                        } else {
                                                            (delta_ticks / 240) * 240
                                                        };
                                                        let mut updated = block.clone();
                                                        for ev in &mut updated.events {
                                                            ev.tick = (ev.tick as i64 + snapped_delta).max(0) as u64;
                                                        }
                                                        self.send_cmd(IpcCommand::UpdateBlock {
                                                            block: updated,
                                                        });
                                                    }
                                                    BlockDragKind::Slide => {
                                                        let snapped_delta = if shift_held || self.freehand_drift {
                                                            delta_ticks
                                                        } else {
                                                            (delta_ticks / 240) * 240
                                                        };
                                                        let new_offset = (drag.initial_offset as i64 + snapped_delta).max(0) as u64;
                                                        let mut updated = block.clone();
                                                        updated.start_offset_ticks = new_offset;
                                                        self.send_cmd(IpcCommand::UpdateBlock {
                                                            block: updated,
                                                        });
                                                    }
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
                                let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(cell_rect));
                                child_ui.centered_and_justified(|ui| {
                                    if ui.small_button("➕ Add Block").on_hover_text("Add a track block / pattern to this section").clicked() {
                                        let block_id = format!("{}_{}", sec.id, track.id);
                                        self.send_cmd(IpcCommand::AddBlock {
                                            track_id: track.id.clone(),
                                            block_id: Some(block_id.clone()),
                                            section_id: Some(sec.id.clone()),
                                            start_tick: None,
                                            length_ticks: Some(sec.length_ticks),
                                        });
                                        self.selected_track_id = track.id.clone();
                                        self.open_piano_roll = Some(OpenPianoRollTarget {
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

    fn render_mixer_view(&mut self, ui: &mut egui::Ui) {
        ui.heading("Console Mixer");
        ui.add_space(8.0);

        let tracks = self.last_status.as_ref().map(|s| s.tracks.clone()).unwrap_or_default();
        let master_vol = self.last_status.as_ref().map(|s| s.master_volume).unwrap_or(0.9);
        let master_peak = self.last_status.as_ref().map(|s| s.master_peak).unwrap_or(0.0);
        let master_inserts = self.last_status.as_ref().map(|s| s.master_inserts.clone()).unwrap_or_default();

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

            // Master Inserts Section
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("⚡ Master Inserts:").strong());

                let available = self.last_status.as_ref()
                    .map(|s| s.available_plugins.clone())
                    .unwrap_or_default();

                let chosen_effect = SearchablePicker::show(
                    ui,
                    "add_master_insert_sel".to_string(),
                    "➕ Add Master Effect...",
                    &mut self.insert_search_filter,
                    &available,
                    |plug| format!("{} ({})", plug.name, plug.plugin_format),
                );

                if let Some(plug) = chosen_effect {
                    let display = format!("{} ({})", plug.name, plug.plugin_format);
                    self.send_cmd(IpcCommand::AddMasterInsert {
                        plugin_uri_or_path: plug.uri_or_path.clone(),
                        plugin_name: display.clone(),
                    });
                    self.status_msg = format!("Added Master insert effect '{}'", display);
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
                                let is_effect_active = self.edit_master_insert_slot == Some(ins_idx);
                                let edit_btn = if is_effect_active {
                                    egui::Button::new("🎛").fill(egui::Color32::from_rgb(50, 90, 150))
                                } else {
                                    egui::Button::new("🎛")
                                };
                                if ui.add(edit_btn).on_hover_text("Open Master Effect Controls in slide-out panel").clicked() {
                                    if is_effect_active {
                                        self.edit_master_insert_slot = None;
                                    } else {
                                        self.edit_track_id = None;
                                        self.edit_insert_slot = None;
                                        self.edit_master_insert_slot = Some(ins_idx);
                                    }
                                }
                                if ui.small_button("🗑").on_hover_text("Remove this master effect").clicked() {
                                    self.send_cmd(IpcCommand::RemoveMasterInsert {
                                        insert_index: ins_idx,
                                    });
                                    self.status_msg = format!("Removed master insert effect '{}'", ins.name);
                                    if self.edit_master_insert_slot == Some(ins_idx) {
                                        self.edit_master_insert_slot = None;
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
                                        self.edit_master_insert_slot = None;
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
}
