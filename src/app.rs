//! The main window.

use crate::camera::{CamCmd, CamMsg, CameraWorker, DemoSource, DeviceInfo};
use crate::chart::{draw_chart, History};
use crate::record::{
    open_folder, render_rgb, save_matrix_csv, save_png, save_raw_single, timestamp_name, CsvLogger, CsvStyle, Mark,
    load_matrix_csv, load_raw_series, temps_to_raw, FrameStore, RawReader, RawWriter, VideoRecorder,
};
use crate::overlay::{annotate, font_px, Anchor, Annotations, ColorScale, Label};
use crate::sim::DemoScene;
use crate::stream::{json_num, json_str, Command, StreamServer};
use crate::thermal::*;
use eframe::egui::{
    self, Align, Align2, Color32, ColorImage, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Vec2,
};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(255, 138, 31);
const BG: Color32 = Color32::from_rgb(21, 23, 28);
const PANEL: Color32 = Color32::from_rgb(28, 31, 38);
const CARD: Color32 = Color32::from_rgb(36, 40, 49);
const MUTED: Color32 = Color32::from_rgb(150, 156, 170);
const REC: Color32 = Color32::from_rgb(230, 50, 50);
const HOT: Color32 = Color32::from_rgb(255, 84, 64);
const COLD: Color32 = Color32::from_rgb(70, 160, 255);

const TOOL_COLORS: [Color32; 8] = [
    Color32::from_rgb(0, 230, 160),
    Color32::from_rgb(255, 220, 60),
    Color32::from_rgb(90, 200, 255),
    Color32::from_rgb(255, 120, 220),
    Color32::from_rgb(170, 255, 90),
    Color32::from_rgb(255, 170, 90),
    Color32::from_rgb(180, 150, 255),
    Color32::from_rgb(240, 240, 240),
];

const KEY_MAX: u64 = 1;
const KEY_MIN: u64 = 2;
const KEY_AVG: u64 = 3;
const KEY_CENTER: u64 = 4;
const KEY_TOOL: u64 = 1000;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Measure,
    Image,
    Emission,
    Capture,
}

#[derive(PartialEq, Clone, Copy)]
enum ToolMode {
    Select,
    Spot,
    Area,
    Line,
}

#[derive(PartialEq, Clone, Copy)]
enum Metric {
    Max,
    Avg,
    Min,
}

impl Metric {
    const ALL: [Metric; 3] = [Metric::Max, Metric::Avg, Metric::Min];

    fn short(&self) -> &'static str {
        match self {
            Metric::Max => "Max",
            Metric::Avg => "Ø",
            Metric::Min => "Min",
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Metric::Max => "Max",
            Metric::Avg => "Mittel",
            Metric::Min => "Min",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Spot { x: f32, y: f32 },
    Area { x0: f32, y0: f32, x1: f32, y1: f32 },
    Line { x0: f32, y0: f32, x1: f32, y1: f32 },
}

#[derive(Clone, Copy, Default)]
struct Reading {
    value: f32,
    min: f32,
    max: f32,
    avg: f32,
    max_pos: (usize, usize),
    min_pos: (usize, usize),
}

struct Tool {
    id: u32,
    name: String,
    shape: Shape,
    color: Color32,
    /// which values of a line/area are shown and drawn (Max, Mittel, Min)
    metrics: [bool; 3],
    in_chart: bool,
    hi_on: bool,
    hi: f32,
    lo_on: bool,
    lo: f32,
    in_alarm: bool,
    reading: Reading,
}

impl Tool {
    fn kind_name(&self) -> &'static str {
        match self.shape {
            Shape::Spot { .. } => "Punkt",
            Shape::Area { .. } => "Bereich",
            Shape::Line { .. } => "Linie",
        }
    }
}

struct Playback {
    store: FrameStore,
    index: usize,
    playing: bool,
    speed: f32,
    pos_ms: f64,
    name: String,
}

enum Source {
    None,
    Demo(DemoSource),
    #[allow(dead_code)]
    Camera(String),
    Playback(Playback),
}

struct CsvImport {
    paths: Vec<PathBuf>,
    settings: EmissionSettings,
}

struct Drag {
    tool_id: u32,
    start: (f32, f32),
    orig: Shape,
    creating: bool,
}

struct Interval {
    active: bool,
    every_s: f32,
    max_count: u32,
    count: u32,
    next_at: Instant,
    folder: PathBuf,
}

#[derive(PartialEq, Clone, Copy)]
enum RangeMode {
    Auto,
    Manual,
}

pub struct ThermoApp {
    worker: CameraWorker,
    devices: Vec<DeviceInfo>,
    probing: bool,
    selected_src: usize,
    source: Source,
    auto_connect: bool,
    status: String,
    frame_times: VecDeque<Instant>,

    frame: Option<ThermalFrame>,
    ema: Option<Vec<f32>>,
    new_frame: bool,
    temps: Vec<f32>,
    stats: FrameStats,
    colors: Vec<Color32>,
    texture: Option<TextureHandle>,
    frozen: bool,

    palette: Palette,
    pal_lut: Vec<Color32>,
    pal_for: Palette,
    range_mode: RangeMode,
    man_lo: f32,
    man_hi: f32,
    cur_lo: f32,
    cur_hi: f32,
    smooth_view: bool,
    noise_filter: u8,
    rotation: u8,
    flip_h: bool,
    flip_v: bool,
    show_max: bool,
    show_min: bool,
    show_center: bool,
    iso_on: bool,
    iso_above: f32,
    iso_color: Color32,
    unit: TempUnit,

    emission: EmissionSettings,
    lut: TempLut,

    tools: Vec<Tool>,
    next_tool_id: u32,
    mode: ToolMode,
    drag: Option<Drag>,
    selected_tool: Option<u32>,

    history: History,
    t0: Instant,
    t_offset: f64,
    sample_every: f64,
    last_sample: f64,
    chart_window: f64,
    chart_paused: bool,
    chart_now: f64,

    out_dir: PathBuf,
    out_dir_text: String,
    csv_german: bool,
    photo_scale: u32,
    photo_raw: bool,
    photo_csv: bool,
    photo_marks: bool,
    cap_labels: bool,
    cap_scale: bool,
    cap_info: bool,
    interval: Interval,
    video: Option<VideoRecorder>,
    video_csv: Option<(CsvLogger, Vec<u32>)>,
    video_scale: u32,
    video_with_csv: bool,
    raw_rec: Option<RawWriter>,
    logger: Option<(CsvLogger, Vec<u32>)>,
    log_every: f32,
    last_log: Option<Instant>,

    events: VecDeque<String>,
    tab: Tab,
    show_analysis: bool,
    analysis_region: Option<u32>,
    analysis_threshold: f32,

    csv_import: Option<CsvImport>,
    stream: Option<StreamServer>,
    stream_port: u16,
    stream_network: bool,
    stream_fps: f32,
    stream_scale: u32,
    stream_last: Option<Instant>,
    remote_on: bool,
    remote_pw: String,
    egui_ctx: egui::Context,
    file_origin: Option<chrono::DateTime<chrono::Local>>,
    curve_for: Option<String>,

    screenshot: Option<PathBuf>,
    frames_drawn: u32,
    shot_requested: bool,
    shot_done: Arc<AtomicBool>,
}

fn default_out_dir() -> PathBuf {
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Documents").join("Wärmebild-Studio")
}

impl ThermoApp {
    pub fn new(cc: &eframe::CreationContext<'_>, screenshot: Option<PathBuf>) -> Self {
        let ctx = cc.egui_ctx.clone();
        setup_style(&ctx);
        let worker = CameraWorker::start(ctx.clone());
        // WBS_LIVE=1 together with --screenshot: search and connect the real camera first (manual test).
        let live_test = std::env::var_os("WBS_LIVE").is_some();
        let screenshot_mode = screenshot.is_some() && !live_test;
        if !screenshot_mode {
            worker.send(CamCmd::Probe);
        }
        let emission = EmissionSettings::default();
        let out_dir = default_out_dir();
        let mut app = Self {
            worker,
            devices: Vec::new(),
            probing: !screenshot_mode,
            selected_src: 0,
            source: Source::Demo(DemoSource::start(ctx.clone())),
            auto_connect: true,
            status: if screenshot_mode { "Demo-Modus (ohne Kamera)".into() } else { "Demo-Modus – suche Wärmebildkamera …".into() },
            frame_times: VecDeque::new(),
            frame: None,
            ema: None,
            new_frame: false,
            temps: Vec::new(),
            stats: FrameStats::default(),
            colors: Vec::new(),
            texture: None,
            frozen: false,
            palette: Palette::Iron,
            pal_lut: Palette::Iron.lut(),
            pal_for: Palette::Iron,
            range_mode: RangeMode::Auto,
            man_lo: 15.0,
            man_hi: 60.0,
            cur_lo: 15.0,
            cur_hi: 60.0,
            smooth_view: true,
            noise_filter: 1,
            rotation: 0,
            flip_h: false,
            flip_v: false,
            show_max: true,
            show_min: true,
            show_center: true,
            iso_on: false,
            iso_above: 50.0,
            iso_color: Color32::from_rgb(0, 255, 120),
            unit: TempUnit::Celsius,
            lut: TempLut::build(&emission),
            emission,
            tools: Vec::new(),
            next_tool_id: 1,
            mode: ToolMode::Select,
            drag: None,
            selected_tool: None,
            history: History::new(),
            t0: Instant::now(),
            t_offset: 0.0,
            sample_every: 0.2,
            last_sample: -1.0,
            chart_window: 120.0,
            chart_paused: false,
            chart_now: 0.0,
            out_dir_text: out_dir.display().to_string(),
            out_dir,
            csv_german: true,
            photo_scale: 4,
            photo_raw: true,
            photo_csv: false,
            photo_marks: true,
            cap_labels: true,
            cap_scale: true,
            cap_info: true,
            interval: Interval {
                active: false,
                every_s: 10.0,
                max_count: 0,
                count: 0,
                next_at: Instant::now(),
                folder: PathBuf::new(),
            },
            video: None,
            video_csv: None,
            video_scale: 3,
            video_with_csv: true,
            raw_rec: None,
            logger: None,
            log_every: 1.0,
            last_log: None,
            events: VecDeque::new(),
            tab: Tab::Measure,
            show_analysis: false,
            analysis_region: None,
            analysis_threshold: 40.0,
            csv_import: None,
            stream: None,
            stream_port: 8080,
            stream_network: true,
            stream_fps: 10.0,
            stream_scale: 2,
            stream_last: None,
            remote_on: false,
            remote_pw: String::new(),
            egui_ctx: ctx.clone(),
            file_origin: None,
            curve_for: None,
            screenshot,
            frames_drawn: 0,
            shot_requested: false,
            shot_done: Arc::new(AtomicBool::new(false)),
        };
        app.history.ensure(KEY_MAX, "Max", HOT);
        app.history.ensure(KEY_MIN, "Min", COLD);
        app.history.ensure(KEY_AVG, "Mittel", Color32::from_gray(170));
        app.history.ensure(KEY_CENTER, "Mitte", Color32::WHITE);
        if let Some(s) = app.history.series.iter_mut().find(|s| s.key == KEY_AVG) {
            s.visible = false;
        }
        app.add_demo_tools();
        app.prefill_demo_history(90.0);
        app.log("Programm gestartet");
        // WBS_STREAM=<port>: start the network stream on this PC only (manual test)
        if let Some(port) = std::env::var("WBS_STREAM").ok().and_then(|v| v.parse().ok()) {
            if let Ok(pw) = std::env::var("WBS_REMOTE_PW") {
                app.remote_on = true;
                app.remote_pw = pw;
            }
            app.stream_port = port;
            app.stream_network = false;
            app.toggle_stream();
        }
        app
    }

    // ------------------------------------------------------------ helpers

    fn now_s(&self) -> f64 {
        self.t0.elapsed().as_secs_f64() + self.t_offset
    }

    fn log(&mut self, msg: impl Into<String>) {
        let line = format!("{}  {}", chrono::Local::now().format("%H:%M:%S"), msg.into());
        self.events.push_front(line);
        self.events.truncate(300);
    }

    fn csv_style(&self) -> CsvStyle {
        CsvStyle { german: self.csv_german }
    }

    fn ensure_out_dir(&mut self) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&self.out_dir).map_err(|e| format!("Ordner „{}“: {e}", self.out_dir.display()))?;
        Ok(self.out_dir.clone())
    }

    fn frame_size(&self) -> (usize, usize) {
        self.frame.as_ref().map(|f| (f.w, f.h)).unwrap_or((SENSOR_W, SENSOR_H))
    }

    fn add_tool(&mut self, shape: Shape) -> u32 {
        let id = self.next_tool_id;
        self.next_tool_id += 1;
        let prefix = match shape {
            Shape::Spot { .. } => "P",
            Shape::Area { .. } => "B",
            Shape::Line { .. } => "L",
        };
        let n = self
            .tools
            .iter()
            .filter(|t| std::mem::discriminant(&t.shape) == std::mem::discriminant(&shape))
            .count()
            + 1;
        let color = TOOL_COLORS[(id as usize - 1) % TOOL_COLORS.len()];
        self.tools.push(Tool {
            id,
            name: format!("{prefix}{n}"),
            shape,
            color,
            metrics: [true, false, false],
            in_chart: true,
            hi_on: false,
            hi: 60.0,
            lo_on: false,
            lo: 5.0,
            in_alarm: false,
            reading: Reading::default(),
        });
        let _ = (&prefix, n, color); // series are created on the first sample
        id
    }

    fn remove_tool(&mut self, id: u32) {
        if let Some(t) = self.tools.iter().find(|t| t.id == id) {
            let name = t.name.clone();
            self.log(format!("Messwerkzeug {name} entfernt"));
        }
        self.tools.retain(|t| t.id != id);
        for k in tool_keys(id) {
            self.history.remove(k);
        }
        if self.selected_tool == Some(id) {
            self.selected_tool = None;
        }
    }

    fn add_demo_tools(&mut self) {
        let p1 = self.add_tool(Shape::Spot { x: 150.0, y: 112.0 });
        self.add_tool(Shape::Spot { x: 218.0, y: 124.0 });
        self.add_tool(Shape::Area { x0: 12.0, y0: 106.0, x1: 76.0, y1: 148.0 });
        let l1 = self.add_tool(Shape::Line { x0: 96.0, y0: 128.0, x1: 246.0, y1: 128.0 });
        if let Some(t) = self.tools.iter_mut().find(|t| t.id == l1) {
            t.metrics = [true, true, true];
        }
        if let Some(t) = self.tools.iter_mut().find(|t| t.id == p1) {
            t.hi_on = true;
            t.hi = 60.0;
        }
    }

    /// Fill the chart with simulated history so the demo shows a curve right away.
    fn prefill_demo_history(&mut self, secs: f64) {
        let mut scene = DemoScene::new();
        let mut t = 0.0;
        while t < secs {
            let f = scene.frame(t);
            let temps: Vec<f32> = f.raw.iter().map(|r| self.lut.c(*r)).collect();
            let stats = frame_stats(&temps, f.w, f.h);
            let mut vals = vec![(KEY_MAX, stats.max), (KEY_MIN, stats.min), (KEY_AVG, stats.mean), (KEY_CENTER, stats.center)];
            for tool in &self.tools {
                let r = measure(&tool.shape, &temps, f.w, f.h);
                for (k, name, col, v, vis) in tool_series(tool, &r) {
                    self.history.ensure(k, &name, col);
                    if let Some(s) = self.history.series.iter_mut().find(|s| s.key == k) {
                        s.visible = vis;
                    }
                    vals.push((k, v));
                }
            }
            self.history.push(t, &vals);
            t += 0.5;
        }
        self.t_offset = secs;
        self.last_sample = secs;
    }

    // ------------------------------------------------------------ sources

    fn stop_source(&mut self) {
        if matches!(self.source, Source::Camera(_)) {
            self.worker.send(CamCmd::Close);
        }
        self.source = Source::None;
        self.ema = None;
    }

    fn start_demo(&mut self, ctx: &egui::Context) {
        self.stop_source();
        self.source = Source::Demo(DemoSource::start(ctx.clone()));
        self.status = "Demo-Modus (ohne Kamera)".into();
        self.log("Demo-Modus gestartet");
    }

    fn connect_device(&mut self, d: DeviceInfo) {
        self.stop_source();
        self.status = format!("Verbinde mit „{}“ …", d.name);
        self.worker.send(CamCmd::Open(d));
    }

    fn open_playback(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .set_title("Wärmebild-Dateien öffnen (mehrere = Serie)")
            .add_filter("Wärmebild-Dateien (*.tcraw, *.csv)", &["tcraw", "csv"])
            .add_filter("Rohdaten (*.tcraw)", &["tcraw"])
            .add_filter("Temperatur-Tabelle (*.csv)", &["csv"])
            .set_directory(&self.out_dir)
            .pick_files()
        else {
            return;
        };
        self.open_paths(paths);
    }

    /// Opens .tcraw files (one recording, one photo or a whole series) or CSV tables.
    /// A folder stands for all .tcraw files in it.
    fn open_paths(&mut self, paths: Vec<PathBuf>) {
        let is_ext = |p: &PathBuf, e: &str| p.extension().map(|x| x.to_string_lossy().eq_ignore_ascii_case(e)).unwrap_or(false);
        let mut files = Vec::new();
        for p in paths {
            if p.is_dir() {
                if let Ok(rd) = std::fs::read_dir(&p) {
                    files.extend(rd.flatten().map(|e| e.path()).filter(|q| is_ext(q, "tcraw")));
                }
            } else {
                files.push(p);
            }
        }
        let raws: Vec<PathBuf> = files.iter().filter(|p| is_ext(p, "tcraw")).cloned().collect();
        let csvs: Vec<PathBuf> = files.iter().filter(|p| is_ext(p, "csv")).cloned().collect();
        if raws.is_empty() && csvs.is_empty() {
            self.log("Keine .tcraw- oder .csv-Datei gewählt");
            return;
        }
        if !raws.is_empty() {
            if !csvs.is_empty() {
                self.log("Rohdaten und CSV gemischt – es werden nur die Rohdaten geöffnet");
            }
            let label = if raws.len() == 1 {
                raws[0].file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
            } else {
                format!("Serie aus {} Dateien", raws.len())
            };
            let store = if raws.len() == 1 { RawReader::open(&raws[0]).map(FrameStore::File) } else { load_raw_series(&raws) };
            match store {
                Ok(store) => {
                    // series: first photo was taken when it was saved; a recording ends when its file was last written
                    let mut sorted = raws.clone();
                    sorted.sort();
                    let back = if raws.len() == 1 { store.duration_ms() } else { 0.0 };
                    self.file_origin = file_time(&sorted[0]).map(|t| t - chrono::Duration::milliseconds(back.round() as i64));
                    self.start_file_source(store, label)
                }
                Err(e) => self.log(format!("Öffnen fehlgeschlagen: {e}")),
            }
        } else {
            // temperatures need the settings they were calculated with → ask first
            self.csv_import = Some(CsvImport { paths: csvs, settings: self.emission });
        }
    }

    fn finish_csv_import(&mut self, imp: CsvImport) {
        let lut = TempLut::build(&imp.settings);
        let mut paths = imp.paths;
        paths.sort();
        let mut frames: Vec<(f64, ThermalFrame)> = Vec::new();
        let mut names = Vec::new();
        for p in &paths {
            let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            match load_matrix_csv(p) {
                Ok((w, h, temps)) => {
                    if let Some((_, f0)) = frames.first() {
                        if (f0.w, f0.h) != (w, h) {
                            self.log(format!("{name}: andere Bildgröße – übersprungen"));
                            continue;
                        }
                    }
                    frames.push((frames.len() as f64 * 1000.0, ThermalFrame { w, h, raw: temps_to_raw(&temps, &lut.table) }));
                    names.push(name);
                }
                Err(e) => self.log(format!("{name}: {e}")),
            }
        }
        if frames.is_empty() {
            return;
        }
        let label = if names.len() == 1 { names[0].clone() } else { format!("{} CSV-Tabellen", names.len()) };
        // show the table with the settings it was saved with; from here on they can be changed
        self.emission = imp.settings;
        self.file_origin = paths.first().and_then(|p| file_time(p));
        self.start_file_source(FrameStore::Memory { frames, names }, label);
    }

    fn start_file_source(&mut self, mut store: FrameStore, name: String) {
        self.stop_source();
        self.log(format!("Geöffnet: {name} ({} Bilder)", store.frames()));
        if let Ok((_, f)) = store.read(0) {
            let was = self.frozen;
            self.frozen = false;
            self.incoming_from_file(f);
            self.frozen = was;
        }
        self.status = format!("Auswertung: {name}");
        self.history.clear();
        self.curve_for = None;
        let multi = store.frames() > 1;
        self.source = Source::Playback(Playback { store, index: 0, playing: false, speed: 1.0, pos_ms: 0.0, name });
        if multi {
            self.chart_window = 0.0;
            self.compute_curve();
        }
    }

    /// Shows picture `i` of the opened file.
    fn seek_playback(&mut self, i: usize) {
        let mut nf = None;
        if let Source::Playback(pb) = &mut self.source {
            if let Ok((t, f)) = pb.store.read(i) {
                pb.index = i;
                pb.pos_ms = t;
                nf = Some(f);
            }
        }
        if let Some(f) = nf {
            let was = self.frozen;
            self.frozen = false;
            self.incoming_from_file(f);
            self.frozen = was;
        }
    }

    /// Temperature curve over the whole opened recording or series: Max, Min, Mittel,
    /// Mitte and all measuring tools, evaluated with the current emission settings.
    fn compute_curve(&mut self) {
        let Source::Playback(pb) = &mut self.source else { return };
        let n = pb.store.frames();
        let step = (n / 5000).max(1);
        let lut = TempLut::build(&self.emission);
        let mut hist = std::mem::replace(&mut self.history, History::new());
        hist.clear();
        for t in &self.tools {
            for (k, name, col, _, _) in tool_series(t, &t.reading) {
                hist.ensure(k, &name, col);
            }
        }
        let mut temps = Vec::new();
        let mut i = 0;
        while i < n {
            if let Ok((t_ms, f)) = pb.store.read(i) {
                temps.clear();
                temps.extend(f.raw.iter().map(|r| lut.c(*r)));
                let st = frame_stats(&temps, f.w, f.h);
                let mut vals = vec![(KEY_MAX, st.max), (KEY_MIN, st.min), (KEY_AVG, st.mean), (KEY_CENTER, st.center)];
                for t in &self.tools {
                    let r = measure(&t.shape, &temps, f.w, f.h);
                    vals.extend(tool_series(t, &r).into_iter().map(|(k, _, _, v, _)| (k, v)));
                }
                hist.push(t_ms / 1000.0, &vals);
            }
            i += step;
        }
        for t in &self.tools {
            for (k, _, _, _, vis) in tool_series(t, &t.reading) {
                if let Some(s) = hist.series.iter_mut().find(|s| s.key == k) {
                    s.visible = vis;
                }
            }
        }
        self.chart_now = hist.times.last().copied().unwrap_or(0.0);
        self.history = hist;
        self.curve_for = Some(self.curve_key());
    }

    /// What the computed curve depends on; when it changes, the curve is recalculated.
    fn curve_key(&self) -> String {
        let mut k = format!("{:?}", self.emission);
        for t in &self.tools {
            let sh = match t.shape {
                Shape::Spot { x, y } => format!("{x},{y}"),
                Shape::Area { x0, y0, x1, y1 } | Shape::Line { x0, y0, x1, y1 } => format!("{x0},{y0},{x1},{y1}"),
            };
            k.push_str(&format!("|{}:{}", t.id, sh));
        }
        k
    }

    fn poll_sources(&mut self, ctx: &egui::Context) {
        // camera worker messages
        while let Ok(msg) = self.worker.rx.try_recv() {
            match msg {
                CamMsg::Devices(list) => {
                    self.probing = false;
                    let n_th = list.iter().filter(|d| d.thermal).count();
                    self.log(format!("{} Kamera(s) gefunden, davon {} Wärmebildkamera(s)", list.len(), n_th));
                    self.devices = list;
                    if let Some((i, d)) = self.devices.iter().enumerate().find(|(_, d)| d.thermal) {
                        self.selected_src = i + 1;
                        if self.auto_connect && matches!(self.source, Source::Demo(_)) {
                            let d = d.clone();
                            self.connect_device(d);
                        }
                    } else if matches!(self.source, Source::Demo(_)) {
                        self.status = "Keine Wärmebildkamera gefunden – Demo-Modus".into();
                    }
                }
                CamMsg::Opened(name) => {
                    self.log(format!("Kamera verbunden: {name}"));
                    self.status = format!("Live: {name}");
                    self.source = Source::Camera(name);
                    self.history.clear();
                }
                CamMsg::Frame(f) => {
                    if matches!(self.source, Source::Camera(_)) {
                        self.incoming(f);
                    }
                }
                CamMsg::Closed => {
                    if matches!(self.source, Source::Camera(_)) {
                        self.source = Source::None;
                        self.status = "Kamera getrennt".into();
                    }
                }
                CamMsg::Error(e) => {
                    self.status = e.clone();
                    self.log(e);
                }
            }
        }
        // demo frames
        let mut demo_frames = Vec::new();
        if let Source::Demo(d) = &self.source {
            while let Ok(f) = d.rx.try_recv() {
                demo_frames.push(f);
            }
        }
        for f in demo_frames {
            self.incoming(f);
        }
        // playback
        let dt = ctx.input(|i| i.stable_dt).min(0.2) as f64;
        let mut next: Option<ThermalFrame> = None;
        if let Source::Playback(p) = &mut self.source {
            if p.playing {
                p.pos_ms += dt * 1000.0 * p.speed as f64;
                let idx = p.store.index_at(p.pos_ms);
                if idx != p.index {
                    p.index = idx;
                    if let Ok((_, f)) = p.store.read(idx) {
                        next = Some(f);
                    }
                }
                if p.pos_ms >= p.store.duration_ms() {
                    p.playing = false;
                }
                ctx.request_repaint();
            }
        }
        if let Some(f) = next {
            self.incoming_from_file(f);
        }
    }

    /// A frame from an opened file: it was saved already rotated and filtered,
    /// so it is shown exactly as stored (no turning, no noise filter, no re-recording).
    fn incoming_from_file(&mut self, f: ThermalFrame) {
        if self.frozen {
            return;
        }
        self.ema = None;
        if let Some(old) = &self.frame {
            if old.w != f.w || old.h != f.h {
                let (w, h) = (f.w as f32, f.h as f32);
                for t in &mut self.tools {
                    clamp_shape(&mut t.shape, w, h);
                }
            }
        }
        self.frame = Some(f);
        self.new_frame = true;
    }

    /// A new frame from the camera or the demo.
    fn incoming(&mut self, raw: ThermalFrame) {
        if self.frozen {
            return;
        }
        let now = Instant::now();
        self.frame_times.push_back(now);
        while let Some(f) = self.frame_times.front() {
            if now.duration_since(*f) > Duration::from_secs(1) {
                self.frame_times.pop_front();
            } else {
                break;
            }
        }
        let f = raw.transformed(self.rotation, self.flip_h, self.flip_v);
        let mut stop_raw = false;
        if let Some(r) = &mut self.raw_rec {
            if !r.push(&f) {
                stop_raw = true;
            }
        }
        if stop_raw {
            self.stop_raw_recording("Bildgröße geändert");
        }
        // temporal noise filter on the raw values
        let alpha = match self.noise_filter {
            0 => 1.0,
            1 => 0.5,
            2 => 0.25,
            _ => 0.12,
        };
        let filtered = if alpha >= 1.0 {
            self.ema = None;
            f
        } else {
            let reset = self.ema.as_ref().map(|e| e.len() != f.raw.len()).unwrap_or(true);
            if reset {
                self.ema = Some(f.raw.iter().map(|v| *v as f32).collect());
            }
            let ema = self.ema.as_mut().unwrap();
            for (e, v) in ema.iter_mut().zip(&f.raw) {
                *e += (*v as f32 - *e) * alpha;
            }
            ThermalFrame { w: f.w, h: f.h, raw: ema.iter().map(|e| e.round() as u16).collect() }
        };
        if let Some(old) = &self.frame {
            if old.w != filtered.w || old.h != filtered.h {
                let (w, h) = (filtered.w as f32, filtered.h as f32);
                for t in &mut self.tools {
                    clamp_shape(&mut t.shape, w, h);
                }
            }
        }
        self.frame = Some(filtered);
        self.new_frame = true;
    }

    // ------------------------------------------------------------ processing

    fn process(&mut self, ctx: &egui::Context) {
        if self.lut.settings != self.emission {
            self.lut = TempLut::build(&self.emission);
        }
        if self.pal_for != self.palette {
            self.pal_lut = self.palette.lut();
            self.pal_for = self.palette;
        }
        let Some(f) = &self.frame else { return };
        let (w, h) = (f.w, f.h);
        self.temps.clear();
        self.temps.extend(f.raw.iter().map(|r| self.lut.c(*r)));
        self.stats = frame_stats(&self.temps, w, h);
        for t in &mut self.tools {
            t.reading = measure(&t.shape, &self.temps, w, h);
        }
        // colour range
        match self.range_mode {
            RangeMode::Auto => {
                let (mut lo, mut hi) = (self.stats.min, self.stats.max);
                if hi - lo < 2.0 {
                    let m = (hi + lo) / 2.0;
                    lo = m - 1.0;
                    hi = m + 1.0;
                }
                if self.new_frame {
                    let k = 0.35;
                    self.cur_lo += (lo - self.cur_lo) * k;
                    self.cur_hi += (hi - self.cur_hi) * k;
                    if (self.cur_lo - lo).abs() > 30.0 || (self.cur_hi - hi).abs() > 30.0 {
                        self.cur_lo = lo;
                        self.cur_hi = hi;
                    }
                }
            }
            RangeMode::Manual => {
                self.cur_lo = self.man_lo.min(self.man_hi - 0.5);
                self.cur_hi = self.man_hi.max(self.man_lo + 0.5);
            }
        }
        let (lo, hi) = (self.cur_lo, self.cur_hi);
        let scale = 255.0 / (hi - lo).max(0.01);
        self.colors.clear();
        for &t in &self.temps {
            let idx = ((t - lo) * scale).clamp(0.0, 255.0) as usize;
            let mut c = self.pal_lut[idx];
            if self.iso_on && t >= self.iso_above {
                c = self.iso_color;
            }
            self.colors.push(c);
        }
        let img = ColorImage::new([w, h], self.colors.clone());
        let opts = if self.smooth_view { TextureOptions::LINEAR } else { TextureOptions::NEAREST };
        match &mut self.texture {
            Some(tex) => tex.set(img, opts),
            None => self.texture = Some(ctx.load_texture("waermebild", img, opts)),
        }

        if self.new_frame {
            self.new_frame = false;
            self.check_alarms();
            self.sample_history();
            self.write_logs();
            self.capture_tick();
        }
    }

    fn check_alarms(&mut self) {
        let mut msgs = Vec::new();
        for t in &mut self.tools {
            let v = t.reading.value;
            let alarm = v.is_finite() && ((t.hi_on && v > t.hi) || (t.lo_on && v < t.lo));
            if alarm && !t.in_alarm {
                let limit = if t.hi_on && v > t.hi { format!("über {}", fmt_t(t.hi, self.unit)) } else { format!("unter {}", fmt_t(t.lo, self.unit)) };
                msgs.push(format!("⚠ Alarm {}: {} ({})", t.name, fmt_t(v, self.unit), limit));
            } else if !alarm && t.in_alarm {
                msgs.push(format!("Alarm {} beendet ({})", t.name, fmt_t(v, self.unit)));
            }
            t.in_alarm = alarm;
        }
        for m in msgs {
            self.log(m);
        }
    }

    fn sample_history(&mut self) {
        // opened files get their curve from compute_curve(), not from wall-clock time
        if let Source::Playback(pb) = &self.source {
            if pb.store.frames() > 1 {
                return;
            }
        }
        let now = self.now_s();
        if now - self.last_sample < self.sample_every {
            return;
        }
        self.last_sample = now;
        let mut vals = vec![
            (KEY_MAX, self.stats.max),
            (KEY_MIN, self.stats.min),
            (KEY_AVG, self.stats.mean),
            (KEY_CENTER, self.stats.center),
        ];
        for t in &self.tools {
            for (k, name, col, v, vis) in tool_series(t, &t.reading) {
                self.history.ensure(k, &name, col);
                if let Some(s) = self.history.series.iter_mut().find(|s| s.key == k) {
                    s.visible = vis;
                }
                vals.push((k, v));
            }
        }
        self.history.push(now, &vals);
    }

    fn csv_header(&self, ids: &[u32]) -> Vec<String> {
        let u = self.unit.label();
        let mut h = vec![
            "Zeit".to_string(),
            "Millisekunden".to_string(),
            format!("Max [{u}]"),
            format!("Min [{u}]"),
            format!("Mittel [{u}]"),
            format!("Mitte [{u}]"),
        ];
        for id in ids {
            if let Some(t) = self.tools.iter().find(|t| t.id == *id) {
                match t.shape {
                    Shape::Spot { .. } => h.push(format!("{} [{u}]", t.name)),
                    _ => {
                        h.push(format!("{} Max [{u}]", t.name));
                        h.push(format!("{} Mittel [{u}]", t.name));
                        h.push(format!("{} Min [{u}]", t.name));
                    }
                }
            }
        }
        h.push("Emissionsgrad".to_string());
        h
    }

    fn csv_row(&self, ids: &[u32], started: Instant) -> Vec<String> {
        let st = self.csv_style();
        let u = self.unit;
        let n = |c: f32| st.num(u.from_c(c), 2);
        let mut r = vec![
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            format!("{}", started.elapsed().as_millis()),
            n(self.stats.max),
            n(self.stats.min),
            n(self.stats.mean),
            n(self.stats.center),
        ];
        for id in ids {
            if let Some(t) = self.tools.iter().find(|t| t.id == *id) {
                match t.shape {
                    Shape::Spot { .. } => r.push(n(t.reading.value)),
                    _ => {
                        r.push(n(t.reading.max));
                        r.push(n(t.reading.avg));
                        r.push(n(t.reading.min));
                    }
                }
            } else {
                r.push(String::new());
            }
        }
        r.push(st.num(self.emission.emissivity, 2));
        r
    }

    fn write_logs(&mut self) {
        let st = self.csv_style();
        let due = match self.last_log {
            None => true,
            Some(t) => t.elapsed().as_secs_f32() >= self.log_every,
        };
        if due {
            let row = self.logger.as_ref().map(|(lg, ids)| self.csv_row(ids, lg.started));
            if let (Some(row), Some((lg, _))) = (row, &mut self.logger) {
                lg.row(&row, &st);
                self.last_log = Some(Instant::now());
            }
        }
        let row = self.video_csv.as_ref().map(|(lg, ids)| self.csv_row(ids, lg.started));
        if let (Some(row), Some((lg, _))) = (row, &mut self.video_csv) {
            lg.row(&row, &st);
        }
    }

    fn marks(&self) -> Vec<Mark> {
        let mut m = Vec::new();
        for t in &self.tools {
            match t.shape {
                Shape::Spot { x, y } => m.push(Mark::Cross { x, y, color: t.color }),
                Shape::Area { x0, y0, x1, y1 } => m.push(Mark::Rect { x0, y0, x1, y1, color: t.color }),
                Shape::Line { x0, y0, x1, y1 } => m.push(Mark::Line { x0, y0, x1, y1, color: t.color }),
            }
        }
        if self.show_center {
            let (w, h) = self.frame_size();
            m.push(Mark::Cross { x: (w / 2) as f32, y: (h / 2) as f32, color: Color32::WHITE });
        }
        if self.show_max {
            m.push(Mark::Cross { x: self.stats.max_pos.0 as f32, y: self.stats.max_pos.1 as f32, color: HOT });
        }
        if self.show_min {
            m.push(Mark::Cross { x: self.stats.min_pos.0 as f32, y: self.stats.min_pos.1 as f32, color: COLD });
        }
        m
    }

    /// Picture for saving: colours, marks and (if switched on) value labels,
    /// colour scale with temperatures and an info strip with date and time.
    fn render_capture(&self, scale: u32, with_marks: bool) -> image::RgbImage {
        let (w, h) = self.frame_size();
        let s = scale.max(1);
        let sf = s as f32;
        let marks = if with_marks { self.marks() } else { Vec::new() };
        let base = render_rgb(&self.colors, w, h, s, &marks, None);
        let px = font_px(s);
        let u = self.unit;
        let at = |x: f32, y: f32| ((x + 0.5) * sf, (y + 0.5) * sf);
        let mut labels = Vec::new();
        if with_marks && self.cap_labels {
            for t in &self.tools {
                let text = tool_label(t, u);
                let (x, y, anchor) = match t.shape {
                    Shape::Spot { x, y } => {
                        let (a, b) = at(x, y);
                        (a, b, Anchor::Point)
                    }
                    Shape::Area { x0, y0, x1, y1 } => (x0.min(x1) * sf, y0.min(y1) * sf, Anchor::Corner),
                    Shape::Line { x0, y0, .. } => {
                        let (a, b) = at(x0, y0);
                        (a, b, Anchor::Point)
                    }
                };
                labels.push(Label { x, y, anchor, text, color: t.color });
            }
            if self.show_center {
                let (a, b) = at((w / 2) as f32, (h / 2) as f32);
                labels.push(Label { x: a, y: b, anchor: Anchor::Point, text: format!("Mitte {}", fmt_t(self.stats.center, u)), color: Color32::WHITE });
            }
            if self.show_max {
                let (a, b) = at(self.stats.max_pos.0 as f32, self.stats.max_pos.1 as f32);
                labels.push(Label { x: a, y: b, anchor: Anchor::Point, text: format!("Max {}", fmt_t(self.stats.max, u)), color: HOT });
            }
            if self.show_min {
                let (a, b) = at(self.stats.min_pos.0 as f32, self.stats.min_pos.1 as f32);
                labels.push(Label { x: a, y: b, anchor: Anchor::Point, text: format!("Min {}", fmt_t(self.stats.min, u)), color: COLD });
            }
        }
        let scale_info = self.cap_scale.then(|| ColorScale {
            lut: &self.pal_lut,
            lo: u.from_c(self.cur_lo),
            hi: u.from_c(self.cur_hi),
            unit: u.label(),
        });
        let info = self.cap_info.then(|| {
            let when = chrono::Local::now().format("%d.%m.%Y  %H:%M:%S").to_string();
            let mut right = format!("ε {}", fmt_num(self.emission.emissivity, 2));
            if self.iso_on {
                right = format!("Isotherme ab {}   {}", fmt_t(self.iso_above, u), right);
            }
            let st = format!(
                "Max {}   Min {}   Mitte {}   Ø {}",
                fmt_t(self.stats.max, u),
                fmt_t(self.stats.min, u),
                fmt_t(self.stats.center, u),
                fmt_t(self.stats.mean, u)
            );
            [when, right, st]
        });
        annotate(base, px, &Annotations { labels, scale: scale_info, info })
    }

    // ------------------------------------------------------------ network stream

    fn toggle_stream(&mut self) {
        if self.stream.take().is_some() {
            self.log("Netzwerk-Stream beendet");
            return;
        }
        match StreamServer::start(self.stream_port, self.stream_network, self.egui_ctx.clone()) {
            Ok(s) => {
                s.set_password(self.remote_password());
                let url = s.urls().first().cloned().unwrap_or_default();
                self.log(format!("Netzwerk-Stream gestartet: {url}"));
                self.stream = Some(s);
                self.stream_last = None;
            }
            Err(e) => self.log(format!("Netzwerk-Stream fehlgeschlagen: {e}")),
        }
    }

    /// Current values for the web page (/werte.json).
    fn stream_json(&self) -> String {
        let u = self.unit;
        let n = |c: f32| json_num(u.from_c(c));
        let mut tools = Vec::new();
        for t in &self.tools {
            let vals: Vec<String> = match t.shape {
                Shape::Spot { .. } => vec![format!("\"Wert\":{}", n(t.reading.value))],
                _ => Metric::ALL
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| t.metrics[*i])
                    .map(|(_, m)| format!("{}:{}", json_str(m.short()), n(metric_value(*m, &t.reading))))
                    .collect(),
            };
            let c = t.color;
            tools.push(format!(
                "{{\"name\":{},\"art\":{},\"farbe\":\"#{:02x}{:02x}{:02x}\",\"alarm\":{},\"werte\":{{{}}}}}",
                json_str(&t.name),
                json_str(t.kind_name()),
                c.r(),
                c.g(),
                c.b(),
                t.in_alarm,
                vals.join(",")
            ));
        }
        let quelle = match &self.source {
            Source::Camera(name) => format!("Live: {name}"),
            Source::Demo(_) => "Demo".to_string(),
            Source::Playback(pb) => format!("Datei: {}", pb.name),
            Source::None => "keine Quelle".to_string(),
        };
        format!(
            "{{\"zeit\":{},\"einheit\":{},\"max\":{},\"min\":{},\"mitte\":{},\"mittel\":{},\"emission\":{},\"quelle\":{},\"werkzeuge\":[{}]}}",
            json_str(&chrono::Local::now().format("%d.%m.%Y %H:%M:%S").to_string()),
            json_str(u.label()),
            n(self.stats.max),
            n(self.stats.min),
            n(self.stats.center),
            n(self.stats.mean),
            json_num(self.emission.emissivity),
            json_str(&quelle),
            tools.join(",")
        )
    }

    /// Settings and tools for the control page (/api/zustand).
    fn remote_state_json(&self) -> String {
        let u = self.unit;
        let (w, h) = self.frame_size();
        let b = |v: bool| if v { "true" } else { "false" };
        let f = |v: f32| json_num(v);
        let pal_idx = Palette::ALL.iter().position(|p| *p == self.palette).unwrap_or(0);
        let paletten: Vec<String> = Palette::ALL.iter().map(|p| json_str(p.name())).collect();
        let materialien: Vec<String> = MATERIALS.iter().map(|(n, e)| format!("[{},{}]", json_str(n), f(*e))).collect();
        let einheit = match u {
            TempUnit::Celsius => "C",
            TempUnit::Fahrenheit => "F",
            TempUnit::Kelvin => "K",
        };
        let e = &self.emission;
        let einstellungen = format!(
            "{{\"palette\":\"{}\",\"bereich\":\"{}\",\"von\":{},\"bis\":{},\"iso\":{},\"iso_ab\":{},\"weich\":{},\"rauschen\":\"{}\",\"drehung\":\"{}\",\"spiegel_h\":{},\"spiegel_v\":{},\"max\":{},\"min\":{},\"mitte\":{},\"standbild\":{},\"einheit\":\"{}\",\"e\":{},\"refl\":{},\"luft\":{},\"abstand\":{},\"feuchte\":{},\"offset\":{},\"foto_groesse\":\"{}\",\"intervall_alle\":{},\"intervall_anzahl\":{}}}",
            pal_idx,
            if self.range_mode == RangeMode::Auto { "auto" } else { "fest" },
            f(self.man_lo),
            f(self.man_hi),
            b(self.iso_on),
            f(self.iso_above),
            b(self.smooth_view),
            self.noise_filter,
            self.rotation,
            b(self.flip_h),
            b(self.flip_v),
            b(self.show_max),
            b(self.show_min),
            b(self.show_center),
            b(self.frozen),
            einheit,
            f(e.emissivity),
            f(e.reflected_c),
            f(e.ambient_c),
            f(e.distance_m),
            json_num((e.humidity * 100.0).round()),
            f(e.offset_c),
            self.photo_scale,
            f(self.interval.every_s),
            self.interval.max_count
        );
        let mut tools = Vec::new();
        for t in &self.tools {
            let details = match t.shape {
                Shape::Spot { x, y } => format!("Position {} / {}", x as i32, y as i32),
                _ => format!(
                    "Max {} · Ø {} · Min {}",
                    fmt_t(t.reading.max, u),
                    fmt_t(t.reading.avg, u),
                    fmt_t(t.reading.min, u)
                ),
            };
            let c = t.color;
            tools.push(format!(
                "{{\"id\":{},\"name\":{},\"art\":{},\"farbe\":\"#{:02x}{:02x}{:02x}\",\"metriken\":[{},{},{}],\"verlauf\":{},\"hi_an\":{},\"hi\":{},\"lo_an\":{},\"lo\":{},\"wert\":{},\"details\":{},\"alarm\":{}}}",
                t.id,
                json_str(&t.name),
                json_str(t.kind_name()),
                c.r(),
                c.g(),
                c.b(),
                b(t.metrics[0]),
                b(t.metrics[1]),
                b(t.metrics[2]),
                b(t.in_chart),
                b(t.hi_on),
                f(t.hi),
                b(t.lo_on),
                f(t.lo),
                json_str(&fmt_t(tool_value(t), u)),
                json_str(&details),
                b(t.in_alarm)
            ));
        }
        let ereignisse: Vec<String> = self.events.iter().take(12).map(|e| json_str(e)).collect();
        let quelle = match &self.source {
            Source::Camera(name) => format!("Live: {name}"),
            Source::Demo(_) => "Demo".to_string(),
            Source::Playback(pb) => format!("Datei: {}", pb.name),
            Source::None => "keine Quelle".to_string(),
        };
        format!(
            "{{\"zeit\":{},\"quelle\":{},\"breite\":{},\"hoehe\":{},\"skalierung\":{},\"einheit\":{},\"werte\":{{\"max\":{},\"min\":{},\"mitte\":{},\"mittel\":{}}},\"paletten\":[{}],\"materialien\":[{}],\"einstellungen\":{},\"werkzeuge\":[{}],\"aufnahme\":{{\"video\":{},\"intervall\":{},\"intervall_zaehler\":{},\"rohdaten\":{},\"protokoll\":{}}},\"ereignisse\":[{}]}}",
            json_str(&chrono::Local::now().format("%d.%m.%Y %H:%M:%S").to_string()),
            json_str(&quelle),
            w,
            h,
            self.stream_scale.max(1),
            json_str(u.label()),
            json_num(u.from_c(self.stats.max)),
            json_num(u.from_c(self.stats.min)),
            json_num(u.from_c(self.stats.center)),
            json_num(u.from_c(self.stats.mean)),
            paletten.join(","),
            materialien.join(","),
            einstellungen,
            tools.join(","),
            b(self.video.is_some()),
            b(self.interval.active),
            self.interval.count,
            b(self.raw_rec.is_some()),
            b(self.logger.is_some()),
            ereignisse.join(",")
        )
    }

    /// Carries out a command from the control page.
    fn apply_command(&mut self, c: Command) {
        let (w, h) = self.frame_size();
        match c.action.as_str() {
            "set" => {
                let key = c.get("key").unwrap_or("").to_string();
                let num = c.num("value");
                let on = c.flag("value").unwrap_or(false);
                let val = c.get("value").unwrap_or("");
                match (key.as_str(), num) {
                    ("palette", Some(v)) => {
                        if let Some(p) = Palette::ALL.get(v as usize) {
                            self.palette = *p;
                        }
                    }
                    ("bereich", _) => self.range_mode = if val == "fest" { RangeMode::Manual } else { RangeMode::Auto },
                    ("von", Some(v)) => self.man_lo = v.clamp(-50.0, 600.0),
                    ("bis", Some(v)) => self.man_hi = v.clamp(-50.0, 600.0),
                    ("iso", _) => self.iso_on = on,
                    ("iso_ab", Some(v)) => self.iso_above = v.clamp(-50.0, 600.0),
                    ("weich", _) => self.smooth_view = on,
                    ("rauschen", Some(v)) => self.noise_filter = (v as u8).min(3),
                    ("drehung", Some(v)) => self.rotation = (v as u8) % 4,
                    ("spiegel_h", _) => self.flip_h = on,
                    ("spiegel_v", _) => self.flip_v = on,
                    ("max", _) => self.show_max = on,
                    ("min", _) => self.show_min = on,
                    ("mitte", _) => self.show_center = on,
                    ("einheit", _) => {
                        self.unit = match val {
                            "F" => TempUnit::Fahrenheit,
                            "K" => TempUnit::Kelvin,
                            _ => TempUnit::Celsius,
                        }
                    }
                    ("e", Some(v)) => self.emission.emissivity = v.clamp(0.01, 1.0),
                    ("refl", Some(v)) => self.emission.reflected_c = v.clamp(-50.0, 600.0),
                    ("luft", Some(v)) => self.emission.ambient_c = v.clamp(-50.0, 100.0),
                    ("abstand", Some(v)) => self.emission.distance_m = v.clamp(0.0, 100.0),
                    ("feuchte", Some(v)) => self.emission.humidity = (v / 100.0).clamp(0.0, 1.0),
                    ("offset", Some(v)) => self.emission.offset_c = v.clamp(-20.0, 20.0),
                    ("foto_groesse", Some(v)) => {
                        if [1, 2, 4, 6].contains(&(v as u32)) {
                            self.photo_scale = v as u32;
                        }
                    }
                    ("intervall_alle", Some(v)) => self.interval.every_s = v.clamp(0.5, 86400.0),
                    ("intervall_anzahl", Some(v)) => self.interval.max_count = v.clamp(0.0, 100000.0) as u32,
                    _ => {}
                }
            }
            "bereich_uebernehmen" => {
                self.man_lo = (self.stats.min * 2.0).floor() / 2.0;
                self.man_hi = (self.stats.max * 2.0).ceil() / 2.0;
                self.range_mode = RangeMode::Manual;
            }
            "emission_zuruecksetzen" => self.emission = EmissionSettings::default(),
            "standbild" => self.frozen = !self.frozen,
            "neu" => {
                let (Some(x0), Some(y0)) = (c.num("x0"), c.num("y0")) else { return };
                let cx = |v: f32| v.clamp(0.0, w as f32 - 1.0);
                let cy = |v: f32| v.clamp(0.0, h as f32 - 1.0);
                let (x1, y1) = (c.num("x1").unwrap_or(x0), c.num("y1").unwrap_or(y0));
                let shape = match c.get("art") {
                    Some("punkt") => Shape::Spot { x: cx(x0), y: cy(y0) },
                    Some("bereich") => Shape::Area { x0: cx(x0.min(x1)), y0: cy(y0.min(y1)), x1: cx(x0.max(x1)), y1: cy(y0.max(y1)) },
                    Some("linie") => Shape::Line { x0: cx(x0), y0: cy(y0), x1: cx(x1), y1: cy(y1) },
                    _ => return,
                };
                let id = self.add_tool(shape);
                self.selected_tool = Some(id);
                self.log("Messwerkzeug über die Fernsteuerung gesetzt");
            }
            "werkzeug" => {
                let Some(id) = c.num("id").map(|v| v as u32) else { return };
                let field = c.get("feld").unwrap_or("").to_string();
                let num = c.num("wert");
                let on = c.flag("wert").unwrap_or(false);
                let text = c.get("wert").unwrap_or("").to_string();
                if let Some(t) = self.tools.iter_mut().find(|t| t.id == id) {
                    match (field.as_str(), num) {
                        ("name", _) => {
                            let n: String = text.trim().chars().take(12).collect();
                            if !n.is_empty() {
                                t.name = n;
                            }
                        }
                        ("metrik0", _) => t.metrics[0] = on,
                        ("metrik1", _) => t.metrics[1] = on,
                        ("metrik2", _) => t.metrics[2] = on,
                        ("verlauf", _) => t.in_chart = on,
                        ("hi_an", _) => t.hi_on = on,
                        ("hi", Some(v)) => t.hi = v.clamp(-50.0, 600.0),
                        ("lo_an", _) => t.lo_on = on,
                        ("lo", Some(v)) => t.lo = v.clamp(-50.0, 600.0),
                        _ => {}
                    }
                }
            }
            "loeschen" => {
                if let Some(id) = c.num("id") {
                    self.remove_tool(id as u32);
                }
            }
            "alle_loeschen" => {
                let ids: Vec<u32> = self.tools.iter().map(|t| t.id).collect();
                for id in ids {
                    self.remove_tool(id);
                }
            }
            "foto" => self.take_photo(),
            "intervall" => self.toggle_interval(),
            "video" => self.toggle_video(),
            "rohdaten" => self.toggle_raw(),
            "protokoll" => self.toggle_logger(),
            _ => {}
        }
    }

    /// Sends the current picture (only when someone watches) and values, at most `stream_fps` times a second.
    /// The control password, if remote control is switched on and the password is long enough.
    fn remote_password(&self) -> Option<String> {
        (self.remote_on && self.remote_pw.chars().count() >= 4).then(|| self.remote_pw.clone())
    }

    fn stream_tick(&mut self) {
        let Some(srv) = &self.stream else { return };
        srv.set_password(self.remote_password());
        let cmds = srv.take_commands();
        let had_cmds = !cmds.is_empty();
        for c in cmds {
            self.apply_command(c);
        }
        if had_cmds {
            self.stream_last = None; // show the effect right away
        }
        let Some(srv) = &self.stream else { return };
        if self.frame.is_none() {
            return;
        }
        let every = Duration::from_secs_f32(1.0 / self.stream_fps.max(0.5));
        if self.stream_last.map(|t| t.elapsed() < every).unwrap_or(false) {
            return;
        }
        self.stream_last = Some(Instant::now());
        if srv.sessions() > 0 {
            srv.publish_state(self.remote_state_json());
        }
        let Some(srv) = &self.stream else { return };
        let json = self.stream_json();
        if srv.viewers() == 0 {
            srv.publish_values(json);
            return;
        }
        let img = self.render_capture(self.stream_scale, true);
        let mut jpg = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 85);
        if img.write_with_encoder(enc).is_ok() {
            if let Some(srv) = &self.stream {
                srv.publish(jpg, json);
            }
        }
    }

    fn capture_tick(&mut self) {
        // video
        if self.video.is_some() {
            let img = self.render_capture(self.video_scale, true);
            let mut stop = false;
            if let Some(v) = &mut self.video {
                if (img.width(), img.height()) == v.size {
                    v.push(img);
                } else {
                    stop = true;
                }
            }
            if stop {
                self.stop_video();
                self.log("Video beendet: Bildgröße geändert");
            }
        }
        // interval photos
        if self.interval.active && Instant::now() >= self.interval.next_at {
            self.interval.next_at = Instant::now() + Duration::from_secs_f32(self.interval.every_s.max(0.5));
            self.interval.count += 1;
            let dir = self.interval.folder.clone();
            let base = format!("bild_{:05}_{}", self.interval.count, chrono::Local::now().format("%H-%M-%S"));
            if let Err(e) = self.save_photo_to(&dir, &base) {
                self.log(format!("Intervall-Foto fehlgeschlagen: {e}"));
                self.interval.active = false;
            }
            if self.interval.max_count > 0 && self.interval.count >= self.interval.max_count {
                self.interval.active = false;
                self.log(format!("Intervall-Serie fertig: {} Fotos", self.interval.count));
            }
        }
    }

    // ------------------------------------------------------------ saving

    fn save_photo_to(&mut self, dir: &std::path::Path, base: &str) -> Result<String, String> {
        let Some(f) = self.frame.clone() else { return Err("noch kein Bild".into()) };
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let img = self.render_capture(self.photo_scale, self.photo_marks);
        let png = dir.join(format!("{base}.png"));
        save_png(&img, &png)?;
        if self.photo_raw {
            save_raw_single(&f, dir.join(format!("{base}.tcraw")))?;
        }
        if self.photo_csv {
            save_matrix_csv(&self.temps, f.w, f.h, &self.csv_style(), &dir.join(format!("{base}_temperaturen.csv")))?;
        }
        Ok(png.display().to_string())
    }

    fn take_photo(&mut self) {
        match self.ensure_out_dir() {
            Ok(dir) => {
                let base = timestamp_name("foto");
                match self.save_photo_to(&dir, &base) {
                    Ok(p) => self.log(format!("Foto gespeichert: {p}")),
                    Err(e) => self.log(format!("Foto fehlgeschlagen: {e}")),
                }
            }
            Err(e) => self.log(e),
        }
    }

    fn toggle_interval(&mut self) {
        if self.interval.active {
            self.interval.active = false;
            self.log(format!("Intervall-Fotos gestoppt ({} Fotos)", self.interval.count));
            return;
        }
        match self.ensure_out_dir() {
            Ok(dir) => {
                let folder = dir.join(timestamp_name("intervall"));
                self.interval.folder = folder.clone();
                self.interval.count = 0;
                self.interval.active = true;
                self.interval.next_at = Instant::now();
                self.log(format!("Intervall-Fotos alle {} s → {}", fmt_num(self.interval.every_s, 1), folder.display()));
            }
            Err(e) => self.log(e),
        }
    }

    fn toggle_video(&mut self) {
        if self.video.is_some() {
            self.stop_video();
            return;
        }
        let dir = match self.ensure_out_dir() {
            Ok(d) => d,
            Err(e) => return self.log(e),
        };
        let size = self.render_capture(self.video_scale, true).dimensions();
        let base = timestamp_name("video");
        let path = dir.join(format!("{base}.avi"));
        match VideoRecorder::start(path.clone(), size.0, size.1) {
            Ok(v) => {
                self.video = Some(v);
                self.log(format!("Video-Aufnahme gestartet: {}", path.display()));
                if self.video_with_csv {
                    let ids: Vec<u32> = self.tools.iter().map(|t| t.id).collect();
                    let header = self.csv_header(&ids);
                    match CsvLogger::create(dir.join(format!("{base}_messwerte.csv")), &header, &self.csv_style()) {
                        Ok(l) => self.video_csv = Some((l, ids)),
                        Err(e) => self.log(format!("Messwert-Datei: {e}")),
                    }
                }
            }
            Err(e) => self.log(format!("Video fehlgeschlagen: {e}")),
        }
    }

    fn stop_video(&mut self) {
        if let Some(v) = self.video.take() {
            let path = v.path.clone();
            let frames = v.frames;
            match v.finish() {
                Ok(()) => self.log(format!("Video gespeichert ({frames} Bilder): {}", path.display())),
                Err(e) => self.log(format!("Video-Fehler: {e}")),
            }
        }
        if let Some((l, _)) = self.video_csv.take() {
            l.finish();
        }
    }

    fn toggle_raw(&mut self) {
        if self.raw_rec.is_some() {
            self.stop_raw_recording("gestoppt");
            return;
        }
        let dir = match self.ensure_out_dir() {
            Ok(d) => d,
            Err(e) => return self.log(e),
        };
        let (w, h) = self.frame_size();
        let path = dir.join(format!("{}.tcraw", timestamp_name("rohdaten")));
        match RawWriter::create(path.clone(), w, h) {
            Ok(r) => {
                self.raw_rec = Some(r);
                self.log(format!("Rohdaten-Aufzeichnung gestartet: {}", path.display()));
            }
            Err(e) => self.log(format!("Rohdaten-Aufzeichnung fehlgeschlagen: {e}")),
        }
    }

    fn stop_raw_recording(&mut self, why: &str) {
        if let Some(r) = self.raw_rec.take() {
            let msg = format!("Rohdaten-Aufzeichnung {why}: {} Bilder, {}", r.frames, r.path.display());
            r.finish();
            self.log(msg);
        }
    }

    fn toggle_logger(&mut self) {
        if let Some((l, _)) = self.logger.take() {
            let msg = format!("Protokoll beendet: {} Zeilen, {}", l.rows, l.path.display());
            l.finish();
            self.log(msg);
            return;
        }
        let dir = match self.ensure_out_dir() {
            Ok(d) => d,
            Err(e) => return self.log(e),
        };
        let ids: Vec<u32> = self.tools.iter().map(|t| t.id).collect();
        let header = self.csv_header(&ids);
        let path = dir.join(format!("{}.csv", timestamp_name("temperaturverlauf")));
        match CsvLogger::create(path.clone(), &header, &self.csv_style()) {
            Ok(l) => {
                self.logger = Some((l, ids));
                self.last_log = None;
                self.log(format!("Temperatur-Protokoll gestartet: {}", path.display()));
            }
            Err(e) => self.log(format!("Protokoll fehlgeschlagen: {e}")),
        }
    }

    /// Wall-clock time of history time 0: for opened files the recording time,
    /// otherwise the moment the program's time axis started.
    fn history_origin(&self) -> chrono::DateTime<chrono::Local> {
        if let (Source::Playback(pb), Some(o)) = (&self.source, self.file_origin) {
            if pb.store.frames() > 1 {
                return o;
            }
        }
        chrono::Local::now() - chrono::Duration::milliseconds((self.now_s() * 1000.0).round() as i64)
    }

    fn export_history(&mut self) {
        let dir = match self.ensure_out_dir() {
            Ok(d) => d,
            Err(e) => return self.log(e),
        };
        let st = self.csv_style();
        let path = dir.join(format!("{}.csv", timestamp_name("verlauf_export")));
        let mut header = vec!["Zeit".to_string(), "Millisekunden".to_string()];
        for s in &self.history.series {
            header.push(format!("{} [{}]", s.name, self.unit.label()));
        }
        match CsvLogger::create(path.clone(), &header, &st) {
            Ok(mut l) => {
                let t_first = self.history.times.first().copied().unwrap_or(0.0);
                let origin = self.history_origin();
                for i in 0..self.history.times.len() {
                    let t = self.history.times[i];
                    let abs = origin + chrono::Duration::milliseconds((t * 1000.0).round() as i64);
                    let mut row = vec![
                        abs.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
                        format!("{}", ((t - t_first) * 1000.0).round() as i64),
                    ];
                    for s in &self.history.series {
                        row.push(s.values.get(i).map(|v| st.num(self.unit.from_c(*v), 2)).unwrap_or_default());
                    }
                    l.row(&row, &st);
                }
                let n = l.rows;
                l.finish();
                self.log(format!("Verlauf exportiert ({n} Zeilen): {}", path.display()));
            }
            Err(e) => self.log(format!("Export fehlgeschlagen: {e}")),
        }
    }

    // ------------------------------------------------------------ UI: top bar

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            let (r, _) = ui.allocate_exact_size(Vec2::new(26.0, 26.0), Sense::hover());
            let p = ui.painter();
            for i in 0..12 {
                let f = i as f32 / 11.0;
                let c = self.pal_lut[(f * 255.0) as usize];
                p.circle_filled(r.center(), 13.0 * (1.0 - f * 0.85), c);
            }
            ui.label(RichText::new("Wärmebild-Studio").size(19.0).strong().color(Color32::WHITE));
            ui.add_space(14.0);
            ui.separator();
            ui.label(RichText::new("Quelle").color(MUTED));
            let names: Vec<String> = std::iter::once("Demo (ohne Kamera)".to_string())
                .chain(self.devices.iter().map(|d| if d.thermal { format!("🌡 {}", d.name) } else { d.name.clone() }))
                .collect();
            let sel = self.selected_src.min(names.len() - 1);
            let shown = match &self.source {
                Source::Playback(_) => "📂 Datei-Auswertung".to_string(),
                _ => names[sel].clone(),
            };
            egui::ComboBox::from_id_salt("source")
                .width(220.0)
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for (i, n) in names.iter().enumerate() {
                        ui.selectable_value(&mut self.selected_src, i, n);
                    }
                });
            let connected = !matches!(self.source, Source::None);
            if ui.button(if connected { "⏏ Trennen" } else { "🔌 Verbinden" }).clicked() {
                if connected {
                    self.stop_source();
                    self.status = "Getrennt".into();
                    self.log("Quelle getrennt");
                } else if self.selected_src == 0 {
                    self.start_demo(&ctx);
                } else if let Some(d) = self.devices.get(self.selected_src - 1).cloned() {
                    self.connect_device(d);
                }
            }
            let can_probe = !self.probing && !matches!(self.source, Source::Camera(_));
            if ui
                .add_enabled(can_probe, egui::Button::new(if self.probing { "⏳ Suche …" } else { "🔄 Suchen" }))
                .on_hover_text("Nach angeschlossenen Kameras suchen")
                .clicked()
            {
                self.probing = true;
                self.worker.send(CamCmd::Probe);
            }
            if ui
                .button("📂 Öffnen")
                .on_hover_text("Gespeicherte Dateien auswerten: Rohdaten-Aufnahme oder Foto (.tcraw), mehrere Fotos einer Intervall-Serie, Temperatur-Tabelle (.csv). Dateien oder Ordner können auch ins Fenster gezogen werden.")
                .clicked()
            {
                self.open_playback();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(6.0);
                if ui.add(egui::Button::new("📊 Auswertung").selected(self.show_analysis)).clicked() {
                    self.show_analysis = !self.show_analysis;
                }
                ui.separator();
                if rec_button(ui, "📡 Stream", self.stream.is_some(), "Live-Bild und Messwerte im Netzwerk freigeben (Browser, VLC, OBS)").clicked() {
                    self.toggle_stream();
                }
                if rec_button(ui, "📝 Protokoll", self.logger.is_some(), "Temperaturverlauf als CSV mitschreiben").clicked() {
                    self.toggle_logger();
                }
                if rec_button(ui, "💾 Rohdaten", self.raw_rec.is_some(), "Alle Rohdaten aufzeichnen – später mit anderem Emissionsgrad auswertbar").clicked() {
                    self.toggle_raw();
                }
                if rec_button(ui, "⏱ Intervall", self.interval.active, "Fotos in festen Abständen").clicked() {
                    self.toggle_interval();
                }
                let vtxt = if let Some(v) = &self.video { format!("⏹ {}", fmt_dur(v.started.elapsed().as_secs())) } else { "⏺ Video".to_string() };
                if rec_button(ui, &vtxt, self.video.is_some(), "Video (AVI) aufnehmen").clicked() {
                    self.toggle_video();
                }
                if ui
                    .add(egui::Button::new(RichText::new("📷 Foto").strong()).fill(Color32::from_rgb(60, 66, 80)))
                    .on_hover_text("Bild speichern (PNG + Rohdaten)")
                    .clicked()
                {
                    self.take_photo();
                }
            });
        });
    }

    // ------------------------------------------------------------ UI: left tools

    fn tool_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            let items = [
                (ToolMode::Select, "☝", "Auswählen / Verschieben\nRechtsklick löscht ein Werkzeug"),
                (ToolMode::Spot, "+", "Messpunkt setzen (Klick ins Bild)"),
                (ToolMode::Area, "⬜", "Messbereich aufziehen"),
                (ToolMode::Line, "╱", "Messlinie ziehen (Profil)"),
            ];
            for (m, icon, tip) in items {
                let b = egui::Button::new(RichText::new(icon).size(20.0)).min_size(Vec2::new(40.0, 40.0)).selected(self.mode == m);
                if ui.add(b).on_hover_text(tip).clicked() {
                    self.mode = m;
                }
            }
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            let toggles: [(&str, &str, bool, Color32); 3] = [
                ("▲", "Heißesten Punkt anzeigen", self.show_max, HOT),
                ("▼", "Kältesten Punkt anzeigen", self.show_min, COLD),
                ("⊕", "Bildmitte anzeigen", self.show_center, Color32::WHITE),
            ];
            for (i, (icon, tip, on, col)) in toggles.into_iter().enumerate() {
                let b = egui::Button::new(RichText::new(icon).size(17.0).color(if on { col } else { MUTED }))
                    .min_size(Vec2::new(40.0, 34.0))
                    .selected(on);
                if ui.add(b).on_hover_text(tip).clicked() {
                    match i {
                        0 => self.show_max = !self.show_max,
                        1 => self.show_min = !self.show_min,
                        _ => self.show_center = !self.show_center,
                    }
                }
            }
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            let frz = egui::Button::new(RichText::new(if self.frozen { "▶" } else { "⏸" }).size(18.0).color(if self.frozen { ACCENT } else { Color32::from_gray(220) }))
                .min_size(Vec2::new(40.0, 34.0))
                .selected(self.frozen);
            if ui.add(frz).on_hover_text(if self.frozen { "Live-Bild fortsetzen" } else { "Standbild – Bild anhalten zum genauen Auswerten" }).clicked() {
                self.frozen = !self.frozen;
            }
            if ui.add(egui::Button::new(RichText::new("⟳").size(18.0)).min_size(Vec2::new(40.0, 34.0))).on_hover_text("Bild um 90° drehen").clicked() {
                self.rotation = (self.rotation + 1) % 4;
            }
            if ui
                .add(egui::Button::new(RichText::new("⇋").size(18.0)).min_size(Vec2::new(40.0, 34.0)).selected(self.flip_h))
                .on_hover_text("Spiegeln")
                .clicked()
            {
                self.flip_h = !self.flip_h;
            }
            ui.add_space(8.0);
            ui.separator();
            if ui.add(egui::Button::new(RichText::new("🗑").size(17.0)).min_size(Vec2::new(40.0, 34.0))).on_hover_text("Alle Messwerkzeuge löschen").clicked() {
                let ids: Vec<u32> = self.tools.iter().map(|t| t.id).collect();
                for id in ids {
                    self.remove_tool(id);
                }
            }
        });
    }

    // ------------------------------------------------------------ UI: image

    fn image_view(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_rect_before_wrap();
        let bar_w = 78.0;
        let pb_h = if matches!(self.source, Source::Playback(_)) { 40.0 } else { 0.0 };
        let area = Rect::from_min_max(avail.min, Pos2::new(avail.max.x - bar_w, avail.max.y - pb_h));
        let (fw, fh) = self.frame_size();
        let aspect = fw as f32 / fh as f32;
        let mut size = Vec2::new(area.width(), area.width() / aspect);
        if size.y > area.height() {
            size = Vec2::new(area.height() * aspect, area.height());
        }
        let img_rect = Rect::from_center_size(area.center(), size);
        let resp = ui.interact(img_rect, ui.scope_id().with("image"), Sense::click_and_drag());
        let painter = ui.painter().clone();
        painter.rect_filled(area, 6.0, Color32::from_rgb(12, 13, 16));
        if let Some(tex) = &self.texture {
            painter.image(tex.id(), img_rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        } else {
            painter.text(img_rect.center(), Align2::CENTER_CENTER, "Kein Bild – bitte Quelle wählen", FontId::proportional(18.0), MUTED);
        }
        let sx = img_rect.width() / fw as f32;
        let sy = img_rect.height() / fh as f32;
        let to_screen = |x: f32, y: f32| Pos2::new(img_rect.left() + (x + 0.5) * sx, img_rect.top() + (y + 0.5) * sy);
        let to_img = |p: Pos2| ((p.x - img_rect.left()) / sx - 0.5, (p.y - img_rect.top()) / sy - 0.5);
        let clampf = |v: (f32, f32)| (v.0.clamp(0.0, fw as f32 - 1.0), v.1.clamp(0.0, fh as f32 - 1.0));

        // ---- interaction
        let hit = |tools: &Vec<Tool>, p: Pos2| -> Option<u32> {
            let mut best: Option<(f32, u32)> = None;
            for t in tools.iter().rev() {
                let d = match t.shape {
                    Shape::Spot { x, y } => to_screen(x, y).distance(p),
                    Shape::Area { x0, y0, x1, y1 } => {
                        let r = Rect::from_two_pos(to_screen(x0, y0), to_screen(x1, y1)).expand(4.0);
                        if r.contains(p) {
                            0.5
                        } else {
                            f32::MAX
                        }
                    }
                    Shape::Line { x0, y0, x1, y1 } => seg_dist(p, to_screen(x0, y0), to_screen(x1, y1)),
                };
                if d < 12.0 && best.map(|b| d < b.0).unwrap_or(true) {
                    best = Some((d, t.id));
                }
            }
            best.map(|b| b.1)
        };
        let press = ui.input(|i| i.pointer.press_origin());
        if resp.drag_started() {
            if let Some(p0) = press {
                let ip = clampf(to_img(p0));
                let hit_id = hit(&self.tools, p0);
                let creating = matches!(self.mode, ToolMode::Area | ToolMode::Line);
                if creating {
                    if self.tools.len() < 16 {
                        let shape = if self.mode == ToolMode::Area {
                            Shape::Area { x0: ip.0, y0: ip.1, x1: ip.0, y1: ip.1 }
                        } else {
                            Shape::Line { x0: ip.0, y0: ip.1, x1: ip.0, y1: ip.1 }
                        };
                        let id = self.add_tool(shape);
                        self.selected_tool = Some(id);
                        self.drag = Some(Drag { tool_id: id, start: ip, orig: shape, creating: true });
                    }
                } else if let Some(id) = hit_id {
                    if let Some(t) = self.tools.iter().find(|t| t.id == id) {
                        self.selected_tool = Some(id);
                        self.drag = Some(Drag { tool_id: id, start: ip, orig: t.shape, creating: false });
                    }
                }
            }
        }
        if resp.dragged() {
            if let (Some(d), Some(p)) = (&self.drag, resp.interact_pointer_pos()) {
                let ip = clampf(to_img(p));
                let (dx, dy) = (ip.0 - d.start.0, ip.1 - d.start.1);
                let (creating, start, orig, id) = (d.creating, d.start, d.orig, d.tool_id);
                if let Some(t) = self.tools.iter_mut().find(|t| t.id == id) {
                    t.shape = if creating {
                        match orig {
                            Shape::Area { .. } => Shape::Area { x0: start.0, y0: start.1, x1: ip.0, y1: ip.1 },
                            Shape::Line { .. } => Shape::Line { x0: start.0, y0: start.1, x1: ip.0, y1: ip.1 },
                            s => s,
                        }
                    } else {
                        let mut s = match orig {
                            Shape::Spot { x, y } => Shape::Spot { x: x + dx, y: y + dy },
                            Shape::Area { x0, y0, x1, y1 } => Shape::Area { x0: x0 + dx, y0: y0 + dy, x1: x1 + dx, y1: y1 + dy },
                            Shape::Line { x0, y0, x1, y1 } => Shape::Line { x0: x0 + dx, y0: y0 + dy, x1: x1 + dx, y1: y1 + dy },
                        };
                        clamp_shape(&mut s, fw as f32, fh as f32);
                        s
                    };
                }
            }
        }
        if resp.drag_stopped() {
            if let Some(d) = self.drag.take() {
                if d.creating {
                    let small = self.tools.iter().find(|t| t.id == d.tool_id).map(|t| match t.shape {
                        Shape::Area { x0, y0, x1, y1 } => (x1 - x0).abs() < 2.0 || (y1 - y0).abs() < 2.0,
                        Shape::Line { x0, y0, x1, y1 } => (x1 - x0).abs() + (y1 - y0).abs() < 3.0,
                        _ => false,
                    });
                    if small == Some(true) {
                        self.tools.retain(|t| t.id != d.tool_id);
                        for k in tool_keys(d.tool_id) {
                            self.history.remove(k);
                        }
                    } else if let Some(t) = self.tools.iter_mut().find(|t| t.id == d.tool_id) {
                        if let Shape::Area { x0, y0, x1, y1 } = t.shape {
                            t.shape = Shape::Area { x0: x0.min(x1), y0: y0.min(y1), x1: x0.max(x1), y1: y0.max(y1) };
                        }
                        let name = t.name.clone();
                        self.log(format!("Messwerkzeug {name} hinzugefügt"));
                    }
                }
            }
        }
        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let h = hit(&self.tools, p);
                if self.mode == ToolMode::Spot && h.is_none() {
                    if self.tools.len() < 16 {
                        let ip = clampf(to_img(p));
                        let id = self.add_tool(Shape::Spot { x: ip.0.round(), y: ip.1.round() });
                        self.selected_tool = Some(id);
                        let name = self.tools.last().map(|t| t.name.clone()).unwrap_or_default();
                        self.log(format!("Messpunkt {name} gesetzt"));
                    }
                } else {
                    self.selected_tool = h;
                }
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                if let Some(id) = hit(&self.tools, p) {
                    self.remove_tool(id);
                }
            }
        }

        // ---- overlays
        let font = FontId::proportional(13.0);
        let (w, h) = (fw, fh);
        let painter = painter.with_clip_rect(img_rect.expand(60.0));
        let blink_on = (ui.input(|i| i.time) * 3.0) as i64 % 2 == 0;
        for t in &self.tools {
            let sel = self.selected_tool == Some(t.id);
            let col = if t.in_alarm && blink_on { REC } else { t.color };
            let st = Stroke::new(if sel { 2.5 } else { 1.6 }, col);
            let shadow = Stroke::new(if sel { 4.5 } else { 3.5 }, Color32::from_black_alpha(140));
            let label = tool_label(t, self.unit);
            match t.shape {
                Shape::Spot { x, y } => {
                    let c = to_screen(x, y);
                    for s in [shadow, st] {
                        painter.line_segment([c - Vec2::new(10.0, 0.0), c - Vec2::new(3.0, 0.0)], s);
                        painter.line_segment([c + Vec2::new(3.0, 0.0), c + Vec2::new(10.0, 0.0)], s);
                        painter.line_segment([c - Vec2::new(0.0, 10.0), c - Vec2::new(0.0, 3.0)], s);
                        painter.line_segment([c + Vec2::new(0.0, 3.0), c + Vec2::new(0.0, 10.0)], s);
                    }
                    label_box(&painter, c + Vec2::new(12.0, -12.0), &label, col, &font);
                }
                Shape::Area { x0, y0, x1, y1 } => {
                    let r = Rect::from_two_pos(to_screen(x0, y0) - Vec2::new(sx, sy) * 0.5, to_screen(x1, y1) + Vec2::new(sx, sy) * 0.5);
                    painter.rect_stroke(r, 0.0, shadow, StrokeKind::Middle);
                    painter.rect_stroke(r, 0.0, st, StrokeKind::Middle);
                    extreme_marks(&painter, t, col, &to_screen);
                    label_box(&painter, r.left_top() + Vec2::new(0.0, -22.0), &label, col, &font);
                }
                Shape::Line { x0, y0, x1, y1 } => {
                    let (a, b) = (to_screen(x0, y0), to_screen(x1, y1));
                    painter.line_segment([a, b], shadow);
                    painter.line_segment([a, b], st);
                    painter.circle_filled(a, 3.0, col);
                    painter.circle_filled(b, 3.0, col);
                    extreme_marks(&painter, t, col, &to_screen);
                    label_box(&painter, a + Vec2::new(0.0, 8.0), &label, col, &font);
                }
            }
        }
        if self.frame.is_some() {
            if self.show_center {
                let c = to_screen((w / 2) as f32, (h / 2) as f32);
                let s = Stroke::new(1.5, Color32::WHITE);
                painter.circle_stroke(c, 7.0, Stroke::new(3.0, Color32::from_black_alpha(120)));
                painter.circle_stroke(c, 7.0, s);
                painter.line_segment([c - Vec2::new(14.0, 0.0), c + Vec2::new(14.0, 0.0)], s);
                painter.line_segment([c - Vec2::new(0.0, 14.0), c + Vec2::new(0.0, 14.0)], s);
                label_box(&painter, c + Vec2::new(14.0, 6.0), &fmt_t(self.stats.center, self.unit), Color32::WHITE, &font);
            }
            if self.show_max {
                let p = to_screen(self.stats.max_pos.0 as f32, self.stats.max_pos.1 as f32);
                triangle(&painter, p, true, HOT);
                label_box(&painter, p + Vec2::new(10.0, -24.0), &format!("▲ {}", fmt_t(self.stats.max, self.unit)), HOT, &font);
            }
            if self.show_min {
                let p = to_screen(self.stats.min_pos.0 as f32, self.stats.min_pos.1 as f32);
                triangle(&painter, p, false, COLD);
                label_box(&painter, p + Vec2::new(10.0, 6.0), &format!("▼ {}", fmt_t(self.stats.min, self.unit)), COLD, &font);
            }
            // hover readout
            if let Some(p) = resp.hover_pos() {
                let ip = to_img(p);
                let (x, y) = (ip.0.round().clamp(0.0, w as f32 - 1.0) as usize, ip.1.round().clamp(0.0, h as f32 - 1.0) as usize);
                let v = self.temps.get(y * w + x).copied().unwrap_or(f32::NAN);
                let txt = format!("{}  ({}, {})", fmt_t(v, self.unit), x, y);
                label_box(&painter, p + Vec2::new(16.0, 16.0), &txt, Color32::WHITE, &FontId::proportional(14.0));
            }
        }
        // status chips over the image
        let mut chips: Vec<(String, Color32)> = Vec::new();
        if self.frozen {
            chips.push(("⏸ STANDBILD".into(), ACCENT));
        }
        if let Some(v) = &self.video {
            chips.push((format!("● REC {}", fmt_dur(v.started.elapsed().as_secs())), REC));
        }
        if let Some(r) = &self.raw_rec {
            chips.push((format!("● ROH {} Bilder", r.frames), REC));
        }
        if self.interval.active {
            let left = self.interval.next_at.saturating_duration_since(Instant::now()).as_secs_f32();
            chips.push((format!("⏱ Foto {} · nächstes in {} s", self.interval.count, fmt_num(left, 0)), ACCENT));
        }
        if self.logger.is_some() {
            chips.push(("📝 Protokoll läuft".into(), Color32::from_rgb(60, 160, 90)));
        }
        if self.tools.iter().any(|t| t.in_alarm) {
            chips.push(("⚠ ALARM".into(), REC));
        }
        let mut cx = img_rect.left() + 8.0;
        for (txt, col) in chips {
            let g = painter.layout_no_wrap(txt, FontId::proportional(13.0), Color32::WHITE);
            let r = Rect::from_min_size(Pos2::new(cx, img_rect.top() + 8.0), g.size() + Vec2::new(14.0, 8.0));
            painter.rect_filled(r, 10.0, col.gamma_multiply(0.85));
            painter.galley(r.min + Vec2::new(7.0, 4.0), g, Color32::WHITE);
            cx = r.right() + 6.0;
        }
        let mode_hint = match self.mode {
            ToolMode::Select => "Werkzeug ziehen zum Verschieben · Rechtsklick löscht",
            ToolMode::Spot => "Klicken, um einen Messpunkt zu setzen",
            ToolMode::Area => "Ziehen, um einen Messbereich aufzuziehen",
            ToolMode::Line => "Ziehen, um eine Messlinie zu legen",
        };
        label_box(&painter, Pos2::new(img_rect.left() + 8.0, img_rect.bottom() - 26.0), mode_hint, Color32::from_white_alpha(200), &FontId::proportional(12.0));

        // ---- colour scale
        let bar = Rect::from_min_max(
            Pos2::new(avail.max.x - bar_w + 14.0, img_rect.top() + 22.0),
            Pos2::new(avail.max.x - bar_w + 34.0, img_rect.bottom() - 36.0),
        );
        let p = ui.painter();
        let n = 128;
        for i in 0..n {
            let f0 = i as f32 / n as f32;
            let f1 = (i + 1) as f32 / n as f32;
            let r = Rect::from_min_max(
                Pos2::new(bar.left(), bar.bottom() - f1 * bar.height()),
                Pos2::new(bar.right(), bar.bottom() - f0 * bar.height() + 0.5),
            );
            let mut c = self.pal_lut[(f0 * 255.0) as usize];
            if self.iso_on && self.cur_lo + f0 * (self.cur_hi - self.cur_lo) >= self.iso_above {
                c = self.iso_color;
            }
            p.rect_filled(r, 0.0, c);
        }
        p.rect_stroke(bar, 2.0, Stroke::new(1.0, Color32::from_gray(90)), StrokeKind::Outside);
        let small = FontId::proportional(12.0);
        p.text(Pos2::new(bar.center().x, bar.top() - 4.0), Align2::CENTER_BOTTOM, fmt_num(self.unit.from_c(self.cur_hi), 1), small.clone(), Color32::WHITE);
        p.text(Pos2::new(bar.center().x, bar.bottom() + 4.0), Align2::CENTER_TOP, fmt_num(self.unit.from_c(self.cur_lo), 1), small.clone(), Color32::WHITE);
        for k in 1..4 {
            let f = k as f32 / 4.0;
            let y = bar.bottom() - f * bar.height();
            p.line_segment([Pos2::new(bar.right(), y), Pos2::new(bar.right() + 4.0, y)], Stroke::new(1.0, MUTED));
            let v = self.cur_lo + f * (self.cur_hi - self.cur_lo);
            p.text(Pos2::new(bar.right() + 6.0, y), Align2::LEFT_CENTER, fmt_num(self.unit.from_c(v), 1), small.clone(), MUTED);
        }
        p.text(Pos2::new(bar.center().x, bar.bottom() + 20.0), Align2::CENTER_TOP, self.unit.label(), small.clone(), MUTED);
        if self.range_mode == RangeMode::Manual {
            p.text(Pos2::new(bar.center().x, bar.top() - 20.0), Align2::CENTER_BOTTOM, "fest", small, ACCENT);
        }

        // ---- playback controls
        if pb_h > 0.0 {
            let pr = Rect::from_min_max(Pos2::new(avail.left(), avail.bottom() - pb_h + 6.0), avail.max);
            let mut seek: Option<usize> = None;
            ui.scope_builder(egui::UiBuilder::new().max_rect(pr), |ui| {
                ui.horizontal(|ui| {
                    if let Source::Playback(pb) = &mut self.source {
                        if ui.button(if pb.playing { "⏸" } else { "▶" }).clicked() {
                            if !pb.playing && pb.index + 1 >= pb.store.frames() {
                                pb.pos_ms = 0.0;
                                pb.index = 0;
                                seek = Some(0);
                            }
                            pb.playing = !pb.playing;
                        }
                        if ui.button("⏮").on_hover_text("Ein Bild zurück").clicked() && pb.index > 0 {
                            seek = Some(pb.index - 1);
                        }
                        if ui.button("⏭").on_hover_text("Ein Bild vor").clicked() {
                            seek = Some((pb.index + 1).min(pb.store.frames() - 1));
                        }
                        let mut idx = pb.index;
                        let max = pb.store.frames().saturating_sub(1);
                        ui.spacing_mut().slider_width = (ui.available_width() - 360.0).max(100.0);
                        if ui.add(egui::Slider::new(&mut idx, 0..=max).show_value(false)).changed() {
                            seek = Some(idx);
                        }
                        ui.label(format!(
                            "{} / {}  ·  Bild {}/{}",
                            fmt_dur((pb.pos_ms / 1000.0) as u64),
                            fmt_dur((pb.store.duration_ms() / 1000.0) as u64),
                            pb.index + 1,
                            pb.store.frames()
                        ));
                        let sp = |s: f32| format!("{}×", fmt_num(s, if s < 1.0 { 2 } else { 0 }));
                        egui::ComboBox::from_id_salt("speed").width(60.0).selected_text(sp(pb.speed)).show_ui(ui, |ui| {
                            for s in [0.25f32, 0.5, 1.0, 2.0, 4.0, 10.0] {
                                ui.selectable_value(&mut pb.speed, s, sp(s));
                            }
                        });
                        let cur = pb.store.name_of(pb.index);
                        ui.label(RichText::new(if cur.is_empty() { pb.name.as_str() } else { cur }).color(MUTED));
                    }
                });
            });
            if let Some(i) = seek {
                self.seek_playback(i);
            }
        }
    }

    // ------------------------------------------------------------ UI: right panel

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            for (t, name) in [(Tab::Measure, "Messwerte"), (Tab::Image, "Bild"), (Tab::Emission, "Emission"), (Tab::Capture, "Aufnahme")] {
                let sel = self.tab == t;
                let b = egui::Button::new(RichText::new(name).color(if sel { Color32::BLACK } else { Color32::from_gray(210) }).strong())
                    .fill(if sel { ACCENT } else { CARD })
                    .min_size(Vec2::new(74.0, 28.0));
                if ui.add(b).clicked() {
                    self.tab = t;
                }
            }
        });
        ui.add_space(6.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
            Tab::Measure => self.tab_measure(ui),
            Tab::Image => self.tab_image(ui),
            Tab::Emission => self.tab_emission(ui),
            Tab::Capture => self.tab_capture(ui),
        });
    }

    fn tab_measure(&mut self, ui: &mut egui::Ui) {
        let u = self.unit;
        ui.columns(2, |cols| {
            readout(&mut cols[0], "▲ Maximum", &fmt_t(self.stats.max, u), HOT);
            readout(&mut cols[1], "▼ Minimum", &fmt_t(self.stats.min, u), COLD);
        });
        ui.columns(2, |cols| {
            readout(&mut cols[0], "⊕ Bildmitte", &fmt_t(self.stats.center, u), Color32::WHITE);
            readout(&mut cols[1], "Ø Mittelwert", &fmt_t(self.stats.mean, u), Color32::from_gray(200));
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Messwerkzeuge").strong().size(15.0));
            ui.label(RichText::new(format!("({})", self.tools.len())).color(MUTED));
        });
        if self.tools.is_empty() {
            ui.label(RichText::new("Wählen Sie links + (Punkt), ⬜ (Bereich) oder ╱ (Linie) und klicken bzw. ziehen Sie im Bild.").color(MUTED));
        }
        let mut remove: Option<u32> = None;
        let mut select: Option<u32> = None;
        let selected = self.selected_tool;
        for t in &mut self.tools {
            let sel = selected == Some(t.id);
            let fill = if t.in_alarm {
                Color32::from_rgb(80, 28, 28)
            } else if sel {
                Color32::from_rgb(44, 49, 60)
            } else {
                CARD
            };
            egui::Frame::new()
                .fill(fill)
                .corner_radius(6.0)
                .inner_margin(8.0)
                .stroke(Stroke::new(1.0, if sel { t.color } else { Color32::TRANSPARENT }))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (r, resp) = ui.allocate_exact_size(Vec2::new(12.0, 12.0), Sense::click());
                        ui.painter().rect_filled(r, 3.0, t.color);
                        if resp.clicked() {
                            select = Some(t.id);
                        }
                        ui.add(egui::TextEdit::singleline(&mut t.name).desired_width(46.0));
                        ui.label(RichText::new(t.kind_name()).color(MUTED).small());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("🗑").on_hover_text("Löschen").clicked() {
                                remove = Some(t.id);
                            }
                            let v = tool_value(t);
                            ui.label(RichText::new(fmt_t(v, u)).size(18.0).strong().color(if t.in_alarm { REC } else { Color32::WHITE }));
                        });
                    });
                    match t.shape {
                        Shape::Spot { x, y } => {
                            ui.label(RichText::new(format!("Position {} / {}  ·  3×3-Mittel", x as i32, y as i32)).color(MUTED).small());
                        }
                        _ => {
                            ui.label(
                                RichText::new(format!(
                                    "Max {}  ·  Ø {}  ·  Min {}",
                                    fmt_t(t.reading.max, u),
                                    fmt_t(t.reading.avg, u),
                                    fmt_t(t.reading.min, u)
                                ))
                                .small(),
                            );
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Anzeige/Verlauf:").small().color(MUTED));
                                for (i, m) in Metric::ALL.iter().enumerate() {
                                    let on = t.metrics[i];
                                    if ui
                                        .add(egui::Button::new(RichText::new(m.name()).small()).selected(on))
                                        .on_hover_text("Mehrere wählbar – im Bild, im Verlauf und auf Fotos")
                                        .clicked()
                                    {
                                        t.metrics[i] = !on;
                                    }
                                }
                            });
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut t.in_chart, RichText::new("im Verlauf").small());
                        ui.checkbox(&mut t.hi_on, RichText::new("Alarm >").small());
                        ui.add_enabled(t.hi_on, temp_dv(&mut t.hi));
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(92.0);
                        ui.checkbox(&mut t.lo_on, RichText::new("Alarm <").small());
                        ui.add_enabled(t.lo_on, temp_dv(&mut t.lo));
                    });
                });
            ui.add_space(4.0);
        }
        if let Some(id) = select {
            self.selected_tool = Some(id);
        }
        if let Some(id) = remove {
            self.remove_tool(id);
        }
        ui.add_space(8.0);
        ui.label(RichText::new("Ereignisse").strong().size(15.0));
        egui::Frame::new().fill(Color32::from_rgb(20, 22, 27)).corner_radius(6.0).inner_margin(6.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(90.0);
            for e in self.events.iter().take(40) {
                let col = if e.contains('⚠') { REC } else { Color32::from_gray(190) };
                ui.label(RichText::new(e).small().color(col));
            }
        });
    }

    fn tab_image(&mut self, ui: &mut egui::Ui) {
        section(ui, "Farbpalette");
        egui::Grid::new("palettes").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            for (i, p) in Palette::ALL.iter().enumerate() {
                let lut = p.lut();
                let (r, resp) = ui.allocate_exact_size(Vec2::new(140.0, 30.0), Sense::click());
                let painter = ui.painter();
                for k in 0..70 {
                    let f = k as f32 / 70.0;
                    let rr = Rect::from_min_max(
                        Pos2::new(r.left() + f * r.width(), r.top()),
                        Pos2::new(r.left() + (k + 1) as f32 / 70.0 * r.width() + 0.5, r.top() + 14.0),
                    );
                    painter.rect_filled(rr, 0.0, lut[(f * 255.0) as usize]);
                }
                let sel = self.palette == *p;
                painter.text(Pos2::new(r.left(), r.bottom()), Align2::LEFT_BOTTOM, p.name(), FontId::proportional(11.0), if sel { ACCENT } else { Color32::from_gray(200) });
                if sel {
                    painter.rect_stroke(Rect::from_min_max(r.min, Pos2::new(r.right(), r.top() + 14.0)), 2.0, Stroke::new(2.0, ACCENT), StrokeKind::Outside);
                }
                if resp.clicked() {
                    self.palette = *p;
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        section(ui, "Temperaturbereich der Farben");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.range_mode, RangeMode::Auto, "Automatisch");
            ui.selectable_value(&mut self.range_mode, RangeMode::Manual, "Fest");
        });
        ui.add_enabled_ui(self.range_mode == RangeMode::Manual, |ui| {
            ui.horizontal(|ui| {
                ui.label("von");
                ui.add(temp_dv(&mut self.man_lo));
                ui.label("bis");
                ui.add(temp_dv(&mut self.man_hi));
            });
        });
        if ui.button("Aktuellen Bereich übernehmen").on_hover_text("Aktuelle Min/Max-Werte als festen Bereich verwenden").clicked() {
            self.man_lo = (self.stats.min * 2.0).floor() / 2.0;
            self.man_hi = (self.stats.max * 2.0).ceil() / 2.0;
            self.range_mode = RangeMode::Manual;
        }
        section(ui, "Isotherme (Hervorhebung)");
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.iso_on, "Alles über");
            ui.add(temp_dv(&mut self.iso_above));
            ui.color_edit_button_srgba(&mut self.iso_color);
        });
        section(ui, "Darstellung");
        ui.checkbox(&mut self.smooth_view, "Bild weich zeichnen (Interpolation)");
        ui.horizontal(|ui| {
            ui.label("Rauschfilter");
            for (v, n) in [(0u8, "Aus"), (1, "Leicht"), (2, "Mittel"), (3, "Stark")] {
                ui.selectable_value(&mut self.noise_filter, v, n);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Drehung");
            for (v, n) in [(0u8, "0°"), (1, "90°"), (2, "180°"), (3, "270°")] {
                ui.selectable_value(&mut self.rotation, v, n);
            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.flip_h, "Horizontal spiegeln");
            ui.checkbox(&mut self.flip_v, "Vertikal spiegeln");
        });
        ui.horizontal(|ui| {
            ui.label("Einheit");
            ui.selectable_value(&mut self.unit, TempUnit::Celsius, "°C");
            ui.selectable_value(&mut self.unit, TempUnit::Fahrenheit, "°F");
            ui.selectable_value(&mut self.unit, TempUnit::Kelvin, "K");
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_max, "Max-Marke");
            ui.checkbox(&mut self.show_min, "Min-Marke");
            ui.checkbox(&mut self.show_center, "Mitte");
        });
    }

    fn tab_emission(&mut self, ui: &mut egui::Ui) {
        section(ui, "Emissionsgrad ε");
        ui.add(egui::Slider::new(&mut self.emission.emissivity, 0.01..=1.0).fixed_decimals(2).step_by(0.01));
        ui.horizontal(|ui| {
            ui.label("Material:");
            let cur = MATERIALS
                .iter()
                .find(|(_, e)| (*e - self.emission.emissivity).abs() < 0.001)
                .map(|m| m.0)
                .unwrap_or("eigener Wert");
            egui::ComboBox::from_id_salt("material").width(190.0).selected_text(cur).show_ui(ui, |ui| {
                for (name, e) in MATERIALS {
                    if ui.selectable_label(false, format!("{name}  ({})", fmt_num(*e, 2))).clicked() {
                        self.emission.emissivity = *e;
                    }
                }
            });
        });
        ui.label(RichText::new("Glänzende Metalle spiegeln stark – dort am besten mattes Klebeband (ε 0,95) aufkleben und darauf messen.").small().color(MUTED));
        section(ui, "Umgebung");
        egui::Grid::new("env").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("Reflektierte Temperatur");
            ui.add(egui::DragValue::new(&mut self.emission.reflected_c).speed(0.2).suffix(" °C").max_decimals(1));
            ui.end_row();
            ui.label("Lufttemperatur");
            ui.add(egui::DragValue::new(&mut self.emission.ambient_c).speed(0.2).suffix(" °C").max_decimals(1));
            ui.end_row();
            ui.label("Entfernung");
            ui.add(egui::DragValue::new(&mut self.emission.distance_m).speed(0.05).range(0.0..=100.0).suffix(" m").max_decimals(2));
            ui.end_row();
            let mut hum = self.emission.humidity * 100.0;
            ui.label("Luftfeuchte");
            if ui.add(egui::DragValue::new(&mut hum).speed(1.0).range(0.0..=100.0).suffix(" %").max_decimals(0)).changed() {
                self.emission.humidity = hum / 100.0;
            }
            ui.end_row();
            ui.label("Korrektur (Offset)");
            ui.add(egui::DragValue::new(&mut self.emission.offset_c).speed(0.1).range(-20.0..=20.0).suffix(" °C").max_decimals(1));
            ui.end_row();
        });
        ui.label(RichText::new(format!("Geschätzte Luft-Durchlässigkeit: {} %", fmt_num(self.emission.transmission() * 100.0, 1))).small().color(MUTED));
        ui.add_space(6.0);
        if ui.button("Zurücksetzen (ε 0,95 · 22 °C · 1 m)").clicked() {
            self.emission = EmissionSettings::default();
        }
        section(ui, "So wird gerechnet");
        ui.label(
            RichText::new(
                "Die Kamera liefert für jedes Pixel einen Rohwert. Die App rechnet daraus mit Emissionsgrad, reflektierter Temperatur und Luftweg die Objekttemperatur. \
                 Weil Rohdaten-Aufnahmen die Originalwerte speichern, können Sie diese Einstellungen auch nachträglich ändern und die Aufnahme neu auswerten.",
            )
            .small()
            .color(MUTED),
        );
        if let Some(f) = &self.frame {
            let c = f.get(f.w / 2, f.h / 2);
            ui.label(
                RichText::new(format!(
                    "Bildmitte: Rohwert {} → scheinbar {} → korrigiert {}",
                    c,
                    fmt_t(c as f32 / 64.0 - 273.15, self.unit),
                    fmt_t(self.lut.c(c), self.unit)
                ))
                .small(),
            );
        }
    }

    fn tab_capture(&mut self, ui: &mut egui::Ui) {
        section(ui, "Speicherort");
        let r = ui.add(egui::TextEdit::singleline(&mut self.out_dir_text).desired_width(ui.available_width() - 4.0));
        if r.lost_focus() {
            self.out_dir = PathBuf::from(self.out_dir_text.trim());
        }
        ui.horizontal(|ui| {
            if ui.button("📁 Ändern …").clicked() {
                if let Some(d) = rfd::FileDialog::new().set_title("Speicherort wählen").pick_folder() {
                    self.out_dir = d;
                    self.out_dir_text = self.out_dir.display().to_string();
                }
            }
            if ui.button("Im Explorer öffnen").clicked() {
                if let Ok(d) = self.ensure_out_dir() {
                    open_folder(&d);
                }
            }
        });
        ui.checkbox(&mut self.csv_german, "CSV für deutsches Excel (; und Komma)");

        section(ui, "🏷 Beschriftung (Foto, Intervall, Video)");
        ui.add_enabled_ui(self.video.is_none(), |ui| {
            ui.checkbox(&mut self.cap_labels, "Messwerte an Messpunkten, Bereichen und Linien");
            ui.checkbox(&mut self.cap_scale, "Farbskala mit Temperaturen");
            ui.checkbox(&mut self.cap_info, "Datum, Uhrzeit und Max/Min/Mitte darunter");
        });

        section(ui, "📷 Foto");
        ui.horizontal(|ui| {
            ui.label("Größe");
            for s in [1u32, 2, 4, 6] {
                ui.selectable_value(&mut self.photo_scale, s, format!("{}×", s));
            }
        });
        ui.checkbox(&mut self.photo_marks, "Messwerkzeuge einzeichnen");
        ui.checkbox(&mut self.photo_raw, "Rohdaten mitspeichern (.tcraw, für spätere Auswertung)");
        ui.checkbox(&mut self.photo_csv, "Temperatur-Tabelle mitspeichern (CSV, jedes Pixel)");
        if ui.add(big_button("📷 Foto aufnehmen", false)).clicked() {
            self.take_photo();
        }

        section(ui, "⏱ Intervall-Fotos");
        ui.add_enabled_ui(!self.interval.active, |ui| {
            ui.horizontal(|ui| {
                ui.label("alle");
                ui.add(egui::DragValue::new(&mut self.interval.every_s).speed(0.5).range(0.5..=86400.0).suffix(" s").max_decimals(1));
                ui.label("Anzahl");
                ui.add(egui::DragValue::new(&mut self.interval.max_count).speed(1.0).range(0..=100000));
            });
            ui.label(RichText::new("Anzahl 0 = unbegrenzt. Jede Serie bekommt einen eigenen Ordner.").small().color(MUTED));
        });
        let it = if self.interval.active { format!("⏹ Stoppen ({} Fotos)", self.interval.count) } else { "⏱ Intervall starten".into() };
        if ui.add(big_button(&it, self.interval.active)).clicked() {
            self.toggle_interval();
        }

        section(ui, "⏺ Video");
        let (fw, fh) = self.frame_size();
        ui.add_enabled_ui(self.video.is_none(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Größe");
                for s in [1u32, 2, 3, 4] {
                    ui.selectable_value(&mut self.video_scale, s, format!("{}×", s))
                        .on_hover_text(format!("{}×{} Pixel", fw as u32 * s, fh as u32 * s));
                }
            });
            ui.checkbox(&mut self.video_with_csv, "Messwerte jedes Bildes als CSV dazu");
        });
        let vt = if let Some(v) = &self.video {
            format!("⏹ Stoppen ({}, {} Bilder)", fmt_dur(v.started.elapsed().as_secs()), v.frames)
        } else {
            "⏺ Video starten".into()
        };
        if ui.add(big_button(&vt, self.video.is_some())).clicked() {
            self.toggle_video();
        }
        ui.label(RichText::new("Format: AVI (Motion-JPEG), läuft in den meisten Playern.").small().color(MUTED));

        section(ui, "📡 Netzwerk-Stream");
        ui.add_enabled_ui(self.stream.is_none(), |ui| {
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.stream_network, true, "im Netzwerk");
                ui.radio_value(&mut self.stream_network, false, "nur dieser PC");
                ui.label("Port");
                ui.add(egui::DragValue::new(&mut self.stream_port).range(1024..=65535).speed(1.0));
            });
        });
        ui.horizontal(|ui| {
            ui.label("Größe");
            for s in [1u32, 2, 3] {
                ui.selectable_value(&mut self.stream_scale, s, format!("{}×", s));
            }
            ui.label("Bilder/s");
            egui::ComboBox::from_id_salt("stream_fps").width(50.0).selected_text(fmt_num(self.stream_fps, 0)).show_ui(ui, |ui| {
                for f in [2.0f32, 5.0, 10.0, 15.0, 25.0] {
                    ui.selectable_value(&mut self.stream_fps, f, fmt_num(f, 0));
                }
            });
        });
        ui.checkbox(&mut self.remote_on, "Fernsteuerung im Browser erlauben (mit Passwort)");
        ui.add_enabled_ui(self.remote_on, |ui| {
            ui.horizontal(|ui| {
                ui.label("Passwort");
                ui.add(egui::TextEdit::singleline(&mut self.remote_pw).password(true).desired_width(160.0));
            });
            if self.remote_on && self.remote_pw.chars().count() < 4 {
                ui.label(RichText::new("Mindestens 4 Zeichen – vorher bleibt die Steuerung gesperrt.").small().color(REC));
            }
        });
        let st = if let Some(s) = &self.stream { format!("⏹ Stream beenden ({} Zuschauer)", s.viewers()) } else { "📡 Stream starten".into() };
        if ui.add(big_button(&st, self.stream.is_some())).clicked() {
            self.toggle_stream();
        }
        if let Some(s) = &self.stream {
            ui.label(RichText::new("Im Browser öffnen:").small());
            for u in s.urls() {
                ui.hyperlink_to(RichText::new(&u).monospace(), &u);
            }
            ui.label(RichText::new(format!("Für VLC/OBS: {}stream.mjpg", s.urls()[0])).small().color(MUTED));
            if self.remote_password().is_some() {
                let u = format!("{}steuerung", s.urls()[0]);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Steuerung:").small());
                    ui.hyperlink_to(RichText::new(&u).monospace(), &u);
                });
                if s.sessions() > 0 {
                    ui.label(RichText::new(format!("{} Steuerung(en) angemeldet", s.sessions())).small().color(ACCENT));
                }
            }
        }
        ui.label(
            RichText::new("Zeigt Live-Bild mit Beschriftung und die Messwerte auf einer Webseite. Ansehen geht ohne Passwort – „im Netzwerk“ kann also jedes Gerät im selben Netz zusehen. Die Steuerseite verlangt das Passwort; es wird unverschlüsselt (HTTP) übertragen, daher nur in vertrauenswürdigen Netzen nutzen und kein Passwort verwenden, das woanders gilt. Das Passwort wird nicht gespeichert. Beim ersten Start fragt die Windows-Firewall nach.")
                .small()
                .color(MUTED),
        );

        section(ui, "💾 Rohdaten-Aufzeichnung");
        let rt = if let Some(r) = &self.raw_rec { format!("⏹ Stoppen ({} Bilder, {})", r.frames, fmt_bytes(r.bytes())) } else { "💾 Rohdaten aufzeichnen".into() };
        if ui.add(big_button(&rt, self.raw_rec.is_some())).clicked() {
            self.toggle_raw();
        }
        ui.label(
            RichText::new("Speichert die echten Messwerte jedes Bildes (ca. 2,4 MB/s). Mit „Aufnahme öffnen“ abspielen und mit anderen Emissionseinstellungen neu auswerten.")
                .small()
                .color(MUTED),
        );

        section(ui, "📝 Temperatur-Protokoll");
        ui.add_enabled_ui(self.logger.is_none(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Eintrag alle");
                ui.add(egui::DragValue::new(&mut self.log_every).speed(0.2).range(0.0..=3600.0).suffix(" s").max_decimals(1));
            });
        });
        let lt = if let Some((l, _)) = &self.logger { format!("⏹ Stoppen ({} Zeilen)", l.rows) } else { "📝 Protokoll starten".into() };
        if ui.add(big_button(&lt, self.logger.is_some())).clicked() {
            self.toggle_logger();
        }
        ui.label(RichText::new("Schreibt Max, Min, Mittel, Bildmitte und alle Messwerkzeuge mit Uhrzeit in eine CSV-Datei.").small().color(MUTED));
    }

    // ------------------------------------------------------------ UI: chart

    fn chart_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("📈 Temperaturverlauf").strong().size(15.0));
            ui.add_space(10.0);
            for (w, n) in [(60.0, "1 min"), (120.0, "2 min"), (600.0, "10 min"), (1800.0, "30 min"), (3600.0, "1 h"), (0.0, "alles")] {
                ui.selectable_value(&mut self.chart_window, w, n);
            }
            ui.separator();
            for s in self.history.series.iter_mut() {
                if s.key >= KEY_TOOL {
                    continue;
                }
                let txt = RichText::new(format!("■ {}", s.name)).color(if s.visible { s.color } else { MUTED });
                if ui.add(egui::Button::new(txt).selected(s.visible).small()).clicked() {
                    s.visible = !s.visible;
                }
            }
            for t in &mut self.tools {
                let txt = RichText::new(format!("■ {}", t.name)).color(if t.in_chart { t.color } else { MUTED });
                if ui.add(egui::Button::new(txt).selected(t.in_chart).small()).clicked() {
                    t.in_chart = !t.in_chart;
                }
            }
            for t in &self.tools {
                for (k, name, col, _, vis) in tool_series(t, &t.reading) {
                    if let Some(s) = self.history.series.iter_mut().find(|s| s.key == k) {
                        s.visible = vis;
                        s.name = name;
                        s.color = col;
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("CSV-Export").on_hover_text("Ganzen Verlauf als CSV speichern").clicked() {
                    self.export_history();
                }
                if ui.button("Leeren").clicked() {
                    self.history.clear();
                }
                if ui
                    .add(egui::Button::new(if self.chart_paused { "▶" } else { "⏸" }).selected(self.chart_paused))
                    .on_hover_text("Anzeige anhalten (Messung läuft weiter)")
                    .clicked()
                {
                    self.chart_paused = !self.chart_paused;
                }
                egui::ComboBox::from_id_salt("rate")
                    .width(70.0)
                    .selected_text(format!("{} s", fmt_num(self.sample_every as f32, 1)))
                    .show_ui(ui, |ui| {
                        for r in [0.1, 0.2, 0.5, 1.0, 5.0, 10.0] {
                            ui.selectable_value(&mut self.sample_every, r, format!("{} s", fmt_num(r as f32, 1)));
                        }
                    })
                    .response
                    .on_hover_text("Abstand der Messwerte im Verlauf");
            });
        });
        let file_curve = matches!(&self.source, Source::Playback(pb) if pb.store.frames() > 1);
        if file_curve {
            self.chart_now = self.history.times.last().copied().unwrap_or(0.0);
        } else if !self.chart_paused {
            self.chart_now = self.now_s();
        }
        let start_label = self.history.times.first().copied().unwrap_or(0.0);
        let cursor = match &self.source {
            Source::Playback(pb) if file_curve => Some(pb.pos_ms / 1000.0),
            _ => None,
        };
        if let Some(t) = draw_chart(ui, &self.history, self.chart_now, self.chart_window, self.unit, start_label, cursor) {
            let idx = match &mut self.source {
                Source::Playback(pb) => Some(pb.store.index_at(t * 1000.0 + 0.5)),
                _ => None,
            };
            if let Some(i) = idx {
                if let Source::Playback(pb) = &mut self.source {
                    pb.playing = false;
                }
                self.seek_playback(i);
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (col, txt) = match &self.source {
                Source::Camera(_) => (Color32::from_rgb(80, 220, 120), "LIVE"),
                Source::Demo(_) => (ACCENT, "DEMO"),
                Source::Playback(_) => (COLD, "WIEDERGABE"),
                Source::None => (MUTED, "AUS"),
            };
            ui.label(RichText::new(format!("● {txt}")).color(col).strong());
            ui.label(RichText::new(&self.status).color(Color32::from_gray(200)));
            ui.separator();
            let (w, h) = self.frame_size();
            ui.label(RichText::new(format!("{}×{} Pixel", w, h)).color(MUTED));
            ui.label(RichText::new(format!("{} Bilder/s", self.frame_times.len())).color(MUTED));
            ui.label(RichText::new(format!("ε {}", fmt_num(self.emission.emissivity, 2))).color(MUTED));
            if let Some(s) = &self.stream {
                ui.separator();
                ui.label(RichText::new(format!("📡 Stream: {} Zuschauer", s.viewers())).color(ACCENT));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("Speicherort: {}", self.out_dir.display())).color(MUTED).small());
            });
        });
    }

    // ------------------------------------------------------------ UI: analysis window

    /// Asks with which settings a CSV temperature table was calculated, so it can be
    /// turned back into raw values and evaluated again with other settings.
    fn csv_import_window(&mut self, ctx: &egui::Context) {
        let Some(imp) = &mut self.csv_import else { return };
        let mut go = false;
        let mut cancel = false;
        egui::Window::new("CSV-Tabelle öffnen")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(420.0);
                let n = imp.paths.len();
                ui.label(if n == 1 { "1 Tabelle gewählt.".to_string() } else { format!("{n} Tabellen gewählt (werden als Serie geöffnet).") });
                ui.add_space(4.0);
                ui.label("In der Tabelle stehen fertig berechnete Temperaturen. Damit Sie sie mit anderen Werten neu auswerten können, braucht das Programm die Einstellungen, mit denen sie gespeichert wurden:");
                ui.add_space(6.0);
                let e = &mut imp.settings;
                egui::Grid::new("csvset").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Emissionsgrad ε");
                    ui.add(egui::DragValue::new(&mut e.emissivity).speed(0.01).range(0.01..=1.0).fixed_decimals(2));
                    ui.end_row();
                    ui.label("Reflektierte Temperatur");
                    ui.add(egui::DragValue::new(&mut e.reflected_c).speed(0.2).suffix(" °C").max_decimals(1));
                    ui.end_row();
                    ui.label("Lufttemperatur");
                    ui.add(egui::DragValue::new(&mut e.ambient_c).speed(0.2).suffix(" °C").max_decimals(1));
                    ui.end_row();
                    ui.label("Entfernung");
                    ui.add(egui::DragValue::new(&mut e.distance_m).speed(0.05).range(0.0..=100.0).suffix(" m").max_decimals(2));
                    ui.end_row();
                    let mut hum = e.humidity * 100.0;
                    ui.label("Luftfeuchte");
                    if ui.add(egui::DragValue::new(&mut hum).speed(1.0).range(0.0..=100.0).suffix(" %").max_decimals(0)).changed() {
                        e.humidity = hum / 100.0;
                    }
                    ui.end_row();
                    ui.label("Korrektur (Offset)");
                    ui.add(egui::DragValue::new(&mut e.offset_c).speed(0.1).range(-20.0..=20.0).suffix(" °C").max_decimals(1));
                    ui.end_row();
                });
                ui.label(RichText::new("Vorbelegt sind die aktuellen Einstellungen. Wissen Sie es nicht genau, lassen Sie sie so – angezeigt werden dann genau die Werte aus der Tabelle.").small().color(MUTED));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add(big_button("Öffnen", false)).clicked() {
                        go = true;
                    }
                    if ui.button("Abbrechen").clicked() {
                        cancel = true;
                    }
                });
            });
        if go {
            if let Some(imp) = self.csv_import.take() {
                self.finish_csv_import(imp);
            }
        } else if cancel {
            self.csv_import = None;
        }
    }

    fn analysis_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_analysis;
        egui::Window::new("📊 Rohdaten-Auswertung")
            .open(&mut open)
            .default_size([520.0, 560.0])
            .default_pos([380.0, 120.0])
            .show(ctx, |ui| self.analysis_ui(ui));
        self.show_analysis = open;
    }

    fn analysis_ui(&mut self, ui: &mut egui::Ui) {
        let (w, h) = self.frame_size();
        if self.temps.len() != w * h {
            ui.label("Noch kein Bild.");
            return;
        }
        ui.horizontal(|ui| {
            ui.label("Bereich:");
            let cur = self
                .analysis_region
                .and_then(|id| self.tools.iter().find(|t| t.id == id))
                .map(|t| format!("{} ({})", t.name, t.kind_name()))
                .unwrap_or_else(|| "Gesamtes Bild".into());
            egui::ComboBox::from_id_salt("region").width(180.0).selected_text(cur).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.analysis_region, None, "Gesamtes Bild");
                for t in &self.tools {
                    if !matches!(t.shape, Shape::Spot { .. }) {
                        ui.selectable_value(&mut self.analysis_region, Some(t.id), format!("{} ({})", t.name, t.kind_name()));
                    }
                }
            });
            if self.frozen {
                ui.label(RichText::new("Standbild").color(ACCENT));
            } else if ui.button("⏸ Standbild").clicked() {
                self.frozen = true;
            }
        });
        let shape = self.analysis_region.and_then(|id| self.tools.iter().find(|t| t.id == id)).map(|t| t.shape);
        let raw_vals: Vec<f32> = match shape {
            Some(Shape::Area { x0, y0, x1, y1 }) => {
                let (a, b, c, d) = (x0.min(x1) as usize, y0.min(y1) as usize, x0.max(x1) as usize, y0.max(y1) as usize);
                let mut v = Vec::new();
                for y in b..=d.min(h - 1) {
                    for x in a..=c.min(w - 1) {
                        v.push(self.temps[y * w + x]);
                    }
                }
                v
            }
            Some(Shape::Line { x0, y0, x1, y1 }) => line_pixels(x0, y0, x1, y1, w, h).iter().map(|(x, y)| self.temps[y * w + x]).collect(),
            _ => self.temps.clone(),
        };
        let is_line = matches!(shape, Some(Shape::Line { .. }));
        let mut sorted = raw_vals.clone();
        let Some(st) = region_stats(&mut sorted) else { return };
        let u = self.unit;
        let above = raw_vals.iter().filter(|v| **v >= self.analysis_threshold).count();
        egui::Grid::new("stats").num_columns(4).spacing([18.0, 6.0]).striped(true).show(ui, |ui| {
            ui.label("Maximum");
            ui.label(RichText::new(fmt_t(st.max, u)).color(HOT).strong());
            ui.label("Minimum");
            ui.label(RichText::new(fmt_t(st.min, u)).color(COLD).strong());
            ui.end_row();
            ui.label("Mittelwert");
            ui.label(RichText::new(fmt_t(st.mean, u)).strong());
            ui.label("Median");
            ui.label(RichText::new(fmt_t(st.median, u)).strong());
            ui.end_row();
            ui.label("Streuung (σ)");
            ui.label(format!("{} K", fmt_num(st.std, 2)));
            ui.label("Spannweite");
            ui.label(format!("{} K", fmt_num(st.max - st.min, 1)));
            ui.end_row();
            ui.label("Pixel");
            ui.label(format!("{}", st.count));
            ui.label("Emissionsgrad");
            ui.label(fmt_num(self.emission.emissivity, 2));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            ui.label("Anteil über");
            ui.add(temp_dv(&mut self.analysis_threshold));
            ui.label(RichText::new(format!("{} %  ({} Pixel)", fmt_num(above as f32 * 100.0 / st.count as f32, 1), above)).strong());
        });
        ui.add_space(4.0);
        ui.label(RichText::new("Häufigkeitsverteilung (Histogramm)").strong());
        let bins = 60usize;
        let span = (st.max - st.min).max(0.1);
        let mut hist = vec![0u32; bins];
        for v in &raw_vals {
            let b = (((v - st.min) / span) * bins as f32).floor().clamp(0.0, bins as f32 - 1.0) as usize;
            hist[b] += 1;
        }
        let hmax = *hist.iter().max().unwrap_or(&1) as f32;
        let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 130.0), Sense::hover());
        let p = ui.painter();
        p.rect_filled(r, 4.0, Color32::from_rgb(18, 20, 26));
        let inner = r.shrink2(Vec2::new(8.0, 16.0));
        let bw = inner.width() / bins as f32;
        for (i, c) in hist.iter().enumerate() {
            let t = st.min + (i as f32 + 0.5) / bins as f32 * span;
            let idx = (((t - self.cur_lo) / (self.cur_hi - self.cur_lo).max(0.01)) * 255.0).clamp(0.0, 255.0) as usize;
            let hh = (*c as f32 / hmax.max(1.0)).sqrt() * inner.height();
            let br = Rect::from_min_max(
                Pos2::new(inner.left() + i as f32 * bw, inner.bottom() - hh),
                Pos2::new(inner.left() + (i + 1) as f32 * bw - 1.0, inner.bottom()),
            );
            p.rect_filled(br, 1.0, self.pal_lut[idx]);
        }
        let f = FontId::proportional(11.0);
        p.text(Pos2::new(inner.left(), r.bottom() - 2.0), Align2::LEFT_BOTTOM, fmt_t(st.min, u), f.clone(), MUTED);
        p.text(Pos2::new(inner.right(), r.bottom() - 2.0), Align2::RIGHT_BOTTOM, fmt_t(st.max, u), f.clone(), MUTED);
        if let Some(pos) = resp.hover_pos() {
            if inner.contains(pos) {
                let i = (((pos.x - inner.left()) / bw) as usize).min(bins - 1);
                let a = st.min + i as f32 / bins as f32 * span;
                let b = a + span / bins as f32;
                p.text(
                    Pos2::new(inner.left(), r.top() + 2.0),
                    Align2::LEFT_TOP,
                    format!("{} – {}: {} Pixel", fmt_num(u.from_c(a), 1), fmt_t(b, u), hist[i]),
                    f.clone(),
                    Color32::WHITE,
                );
            }
        }
        if is_line && raw_vals.len() > 1 {
            let prof = &raw_vals;
            ui.add_space(6.0);
            ui.label(RichText::new("Temperaturprofil entlang der Linie").strong());
            let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 130.0), Sense::hover());
            let p = ui.painter();
            p.rect_filled(r, 4.0, Color32::from_rgb(18, 20, 26));
            let inner = r.shrink2(Vec2::new(40.0, 12.0));
            let pts: Vec<Pos2> = prof
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    Pos2::new(
                        inner.left() + i as f32 / (prof.len() - 1) as f32 * inner.width(),
                        inner.bottom() - (v - st.min) / span * inner.height(),
                    )
                })
                .collect();
            p.line(pts, Stroke::new(2.0, ACCENT));
            p.text(Pos2::new(inner.left() - 4.0, inner.top()), Align2::RIGHT_CENTER, fmt_num(u.from_c(st.max), 1), f.clone(), MUTED);
            p.text(Pos2::new(inner.left() - 4.0, inner.bottom()), Align2::RIGHT_CENTER, fmt_num(u.from_c(st.min), 1), f.clone(), MUTED);
            if let Some(pos) = resp.hover_pos() {
                if inner.contains(pos) {
                    let i = (((pos.x - inner.left()) / inner.width()) * (prof.len() - 1) as f32).round() as usize;
                    let v = prof[i.min(prof.len() - 1)];
                    p.text(Pos2::new(inner.right(), r.top() + 2.0), Align2::RIGHT_TOP, format!("Pixel {}: {}", i, fmt_t(v, u)), f.clone(), Color32::WHITE);
                }
            }
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui.button("Temperatur-Tabelle (CSV)").on_hover_text("Jedes Pixel des aktuellen Bildes in °C").clicked() {
                if let Ok(dir) = self.ensure_out_dir() {
                    let p = dir.join(format!("{}.csv", timestamp_name("temperaturen")));
                    let r = save_matrix_csv(&self.temps, w, h, &self.csv_style(), &p);
                    self.log(match r {
                        Ok(()) => format!("Temperatur-Tabelle gespeichert: {}", p.display()),
                        Err(e) => format!("Speichern fehlgeschlagen: {e}"),
                    });
                }
            }
            if ui.button("Rohbild speichern (.tcraw)").clicked() {
                if let (Ok(dir), Some(f)) = (self.ensure_out_dir(), self.frame.clone()) {
                    let p = dir.join(format!("{}.tcraw", timestamp_name("rohbild")));
                    let r = save_raw_single(&f, p.clone());
                    self.log(match r {
                        Ok(()) => format!("Rohbild gespeichert: {}", p.display()),
                        Err(e) => format!("Speichern fehlgeschlagen: {e}"),
                    });
                }
            }
            if ui.button("📂 Aufnahme öffnen").clicked() {
                self.open_playback();
            }
        });
    }
}

// ---------------------------------------------------------------- free helpers

fn measure(shape: &Shape, temps: &[f32], w: usize, h: usize) -> Reading {
    if temps.len() != w * h || w == 0 || h == 0 {
        return Reading { value: f32::NAN, min: f32::NAN, max: f32::NAN, avg: f32::NAN, max_pos: (0, 0), min_pos: (0, 0) };
    }
    match *shape {
        Shape::Spot { x, y } => {
            let (xi, yi) = (x.round().clamp(0.0, (w - 1) as f32) as usize, y.round().clamp(0.0, (h - 1) as f32) as usize);
            let v = spot_value(temps, w, h, xi, yi);
            Reading { value: v, min: v, max: v, avg: v, max_pos: (xi, yi), min_pos: (xi, yi) }
        }
        Shape::Area { x0, y0, x1, y1 } => {
            let cl = |v: f32, m: usize| v.round().clamp(0.0, (m - 1) as f32) as usize;
            let (a, c) = (cl(x0.min(x1), w), cl(x0.max(x1), w));
            let (b, d) = (cl(y0.min(y1), h), cl(y0.max(y1), h));
            let mut r = Reading { value: 0.0, min: f32::MAX, max: f32::MIN, avg: 0.0, max_pos: (a, b), min_pos: (a, b) };
            let mut sum = 0.0f64;
            let mut n = 0;
            for y in b..=d {
                for x in a..=c {
                    let t = temps[y * w + x];
                    sum += t as f64;
                    n += 1;
                    if t < r.min {
                        r.min = t;
                        r.min_pos = (x, y);
                    }
                    if t > r.max {
                        r.max = t;
                        r.max_pos = (x, y);
                    }
                }
            }
            r.avg = (sum / n.max(1) as f64) as f32;
            r.value = r.max;
            r
        }
        Shape::Line { x0, y0, x1, y1 } => {
            let px = line_pixels(x0, y0, x1, y1, w, h);
            let mut r = Reading { value: 0.0, min: f32::MAX, max: f32::MIN, avg: 0.0, max_pos: (0, 0), min_pos: (0, 0) };
            let mut sum = 0.0f64;
            for (x, y) in &px {
                let t = temps[y * w + x];
                sum += t as f64;
                if t < r.min {
                    r.min = t;
                    r.min_pos = (*x, *y);
                }
                if t > r.max {
                    r.max = t;
                    r.max_pos = (*x, *y);
                }
            }
            r.avg = (sum / px.len().max(1) as f64) as f32;
            r.value = r.max;
            r
        }
    }
}

/// Last-modified time of a file as local time.
fn file_time(p: &std::path::Path) -> Option<chrono::DateTime<chrono::Local>> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok().map(chrono::DateTime::<chrono::Local>::from)
}

fn metric_value(m: Metric, r: &Reading) -> f32 {
    match m {
        Metric::Max => r.max,
        Metric::Avg => r.avg,
        Metric::Min => r.min,
    }
}

/// Mix a colour towards grey (k = 0 unchanged … 1 grey) so the Ø/Min curves of a tool differ.
fn shade(c: Color32, to: u8, k: f32) -> Color32 {
    let m = |v: u8| (v as f32 * (1.0 - k) + to as f32 * k).round() as u8;
    Color32::from_rgb(m(c.r()), m(c.g()), m(c.b()))
}

/// Chart series of a tool: (key, name, colour, value, visible). Lines and areas always
/// record Max, Mittel and Min, so switching one on later still shows its past values.
fn tool_series(t: &Tool, r: &Reading) -> Vec<(u64, String, Color32, f32, bool)> {
    let base = KEY_TOOL + t.id as u64 * 4;
    match t.shape {
        Shape::Spot { .. } => vec![(base, t.name.clone(), t.color, r.value, t.in_chart)],
        _ => Metric::ALL
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let col = match m {
                    Metric::Max => t.color,
                    Metric::Avg => shade(t.color, 150, 0.45),
                    Metric::Min => shade(t.color, 40, 0.45),
                };
                (base + i as u64, format!("{} {}", t.name, m.short()), col, metric_value(*m, r), t.in_chart && t.metrics[i])
            })
            .collect(),
    }
}

fn tool_keys(id: u32) -> [u64; 3] {
    let b = KEY_TOOL + id as u64 * 4;
    [b, b + 1, b + 2]
}

/// Label text of a tool, e.g. "P1 50,2 °C" or "L1 Max 47,9 · Ø 30,1 · Min 12,0 °C".
fn tool_label(t: &Tool, unit: TempUnit) -> String {
    match t.shape {
        Shape::Spot { .. } => format!("{} {}", t.name, fmt_t(t.reading.value, unit)),
        _ => {
            let parts: Vec<String> = Metric::ALL
                .iter()
                .enumerate()
                .filter(|(i, _)| t.metrics[*i])
                .map(|(_, m)| format!("{} {}", m.short(), fmt_num(unit.from_c(metric_value(*m, &t.reading)), 1)))
                .collect();
            if parts.is_empty() {
                t.name.clone()
            } else {
                format!("{} {} {}", t.name, parts.join(" · "), unit.label())
            }
        }
    }
}

/// The main value of a tool (first selected of Max, Mittel, Min).
fn tool_value(t: &Tool) -> f32 {
    match t.shape {
        Shape::Spot { .. } => t.reading.value,
        _ => Metric::ALL
            .iter()
            .enumerate()
            .find(|(i, _)| t.metrics[*i])
            .map(|(_, m)| metric_value(*m, &t.reading))
            .unwrap_or(t.reading.max),
    }
}

fn clamp_shape(s: &mut Shape, w: f32, h: f32) {
    let cx = |v: f32| v.clamp(0.0, w - 1.0);
    let cy = |v: f32| v.clamp(0.0, h - 1.0);
    *s = match *s {
        Shape::Spot { x, y } => Shape::Spot { x: cx(x), y: cy(y) },
        Shape::Area { x0, y0, x1, y1 } => {
            // keep the size while pushing against the border
            let (bw, bh) = ((x1 - x0).abs().min(w - 1.0), (y1 - y0).abs().min(h - 1.0));
            let nx0 = x0.min(x1).clamp(0.0, w - 1.0 - bw);
            let ny0 = y0.min(y1).clamp(0.0, h - 1.0 - bh);
            Shape::Area { x0: nx0, y0: ny0, x1: nx0 + bw, y1: ny0 + bh }
        }
        Shape::Line { x0, y0, x1, y1 } => Shape::Line { x0: cx(x0), y0: cy(y0), x1: cx(x1), y1: cy(y1) },
    };
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
    (a + ab * t).distance(p)
}

fn label_box(p: &egui::Painter, pos: Pos2, text: &str, color: Color32, font: &FontId) {
    let g = p.layout_no_wrap(text.to_string(), font.clone(), color);
    let r = Rect::from_min_size(pos, g.size() + Vec2::new(8.0, 4.0));
    p.rect_filled(r, 3.0, Color32::from_black_alpha(170));
    p.galley(r.min + Vec2::new(4.0, 2.0), g, color);
}

/// Marks where the Max (▲) and Min (▼) of a line or area are, when they are selected.
fn extreme_marks(p: &egui::Painter, t: &Tool, col: Color32, to_screen: &dyn Fn(f32, f32) -> Pos2) {
    if t.metrics[0] {
        let c = to_screen(t.reading.max_pos.0 as f32, t.reading.max_pos.1 as f32);
        small_triangle(p, c, true, col);
    }
    if t.metrics[2] {
        let c = to_screen(t.reading.min_pos.0 as f32, t.reading.min_pos.1 as f32);
        small_triangle(p, c, false, col);
    }
}

fn small_triangle(p: &egui::Painter, c: Pos2, up: bool, color: Color32) {
    let s = 5.0;
    let pts = if up {
        vec![c + Vec2::new(0.0, -s), c + Vec2::new(s, s * 0.8), c + Vec2::new(-s, s * 0.8)]
    } else {
        vec![c + Vec2::new(0.0, s), c + Vec2::new(s, -s * 0.8), c + Vec2::new(-s, -s * 0.8)]
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::new(1.2, Color32::BLACK)));
}

fn triangle(p: &egui::Painter, c: Pos2, up: bool, color: Color32) {
    let s = 7.0;
    let pts = if up {
        vec![c + Vec2::new(0.0, -s), c + Vec2::new(s, s * 0.8), c + Vec2::new(-s, s * 0.8)]
    } else {
        vec![c + Vec2::new(0.0, s), c + Vec2::new(s, -s * 0.8), c + Vec2::new(-s, -s * 0.8)]
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::new(1.5, Color32::BLACK)));
}

fn readout(ui: &mut egui::Ui, title: &str, value: &str, color: Color32) {
    egui::Frame::new().fill(CARD).corner_radius(6.0).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).small().color(MUTED));
        ui.label(RichText::new(value).size(20.0).strong().color(color));
    });
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).strong().size(14.0).color(ACCENT));
    ui.add_space(2.0);
}

fn rec_button(ui: &mut egui::Ui, text: &str, active: bool, tip: &str) -> egui::Response {
    let b = egui::Button::new(RichText::new(text).color(if active { Color32::WHITE } else { Color32::from_gray(220) })).fill(if active { REC } else { CARD });
    ui.add(b).on_hover_text(tip)
}

fn big_button(text: &str, active: bool) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).strong().size(14.0).color(if active { Color32::WHITE } else { Color32::BLACK }))
        .fill(if active { REC } else { ACCENT })
        .min_size(Vec2::new(240.0, 30.0))
}

fn fmt_dur(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

fn fmt_bytes(b: u64) -> String {
    if b > 1 << 30 {
        format!("{} GB", fmt_num(b as f32 / (1u64 << 30) as f32, 2))
    } else {
        format!("{} MB", fmt_num(b as f32 / (1u64 << 20) as f32, 1))
    }
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = Color32::from_rgb(16, 18, 22);
    v.faint_bg_color = Color32::from_rgb(32, 35, 43);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.55);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.widgets.inactive.weak_bg_fill = CARD;
    v.widgets.inactive.bg_fill = CARD;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(52, 57, 70);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(64, 70, 86);
    v.widgets.inactive.corner_radius = CornerRadius::same(5);
    v.widgets.hovered.corner_radius = CornerRadius::same(5);
    v.widgets.active.corner_radius = CornerRadius::same(5);
    v.window_corner_radius = CornerRadius::same(8);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.spacing.item_spacing = Vec2::new(7.0, 6.0);
        s.spacing.button_padding = Vec2::new(9.0, 4.0);
        s.spacing.interact_size.y = 24.0;
    });
}

impl eframe::App for ThermoApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_sources(&ctx);
        self.process(&ctx);

        egui::Panel::top("top")
            .frame(egui::Frame::new().fill(Color32::from_rgb(17, 19, 24)).inner_margin(egui::Margin::symmetric(8, 8)))
            .show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(Color32::from_rgb(17, 19, 24)).inner_margin(egui::Margin::symmetric(10, 4)))
            .show(ui, |ui| self.status_bar(ui));
        egui::Panel::bottom("chart")
            .resizable(true)
            .default_size(230.0)
            .min_size(140.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ui, |ui| self.chart_panel(ui));
        egui::Panel::right("side")
            .resizable(true)
            .default_size(350.0)
            .min_size(300.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::symmetric(10, 4)))
            .show(ui, |ui| self.side_panel(ui));
        egui::Panel::left("tools")
            .exact_size(56.0)
            .frame(egui::Frame::new().fill(Color32::from_rgb(24, 26, 32)).inner_margin(egui::Margin::symmetric(6, 4)))
            .show(ui, |ui| self.tool_bar(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(10))
            .show(ui, |ui| self.image_view(ui));
        if self.show_analysis {
            self.analysis_window(&ctx);
        }
        self.csv_import_window(&ctx);

        // files dropped onto the window
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            self.open_paths(dropped);
        }
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let r = ctx.content_rect();
            let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop")));
            p.rect_filled(r, 0.0, Color32::from_black_alpha(170));
            p.text(r.center(), Align2::CENTER_CENTER, "Loslassen zum Öffnen\n(.tcraw, .csv oder Ordner einer Intervall-Serie)", FontId::proportional(24.0), Color32::WHITE);
        }

        // keep the curve of an opened recording/series in step with tools and emission settings
        if self.drag.is_none() && matches!(&self.source, Source::Playback(pb) if pb.store.frames() > 1) {
            if self.curve_for.as_deref() != Some(self.curve_key().as_str()) {
                self.compute_curve();
            }
        }

        // keep live views ticking
        if !matches!(self.source, Source::None) || self.interval.active {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
        if self.stream.is_some() {
            self.stream_tick();
            ctx.request_repaint_after(Duration::from_secs_f32(1.0 / self.stream_fps.max(0.5)));
        }

        // --screenshot: draw ~120 frames, save the window picture, exit
        if let Some(path) = self.screenshot.clone() {
            self.frames_drawn += 1;
            ctx.request_repaint();
            let live_wait = std::env::var_os("WBS_LIVE").is_some()
                && !matches!(self.source, Source::Camera(_))
                && self.frames_drawn < 2000;
            // WBS_OPEN=<file;file;…> together with --screenshot: open files (CSV with current settings).
            if self.frames_drawn == 5 {
                if let Some(list) = std::env::var_os("WBS_OPEN") {
                    let paths: Vec<PathBuf> = list.to_string_lossy().split(';').map(PathBuf::from).collect();
                    self.open_paths(paths);
                    if let Some(imp) = self.csv_import.take() {
                        self.finish_csv_import(imp);
                    }
                }
            }
            // WBS_PHOTO=<folder> together with --screenshot: also save test photos (sizes 1×, 2×, 4×).
            if self.frames_drawn == 100 {
                if let Some(dir) = std::env::var_os("WBS_PHOTO").map(PathBuf::from) {
                    let keep = self.photo_scale;
                    let keep_csv = self.photo_csv;
                    self.photo_csv = true;
                    for s in [1u32, 2, 4] {
                        self.photo_scale = s;
                        let r = self.save_photo_to(&dir, &format!("testfoto_{s}x"));
                        self.photo_scale = keep;
                        self.photo_csv = keep_csv;
                        if let Err(e) = r {
                            eprintln!("Testfoto fehlgeschlagen: {e}");
                        }
                    }
                }
            }
            // WBS_EXPORT=<folder> together with --screenshot: export the chart history there.
            if self.frames_drawn == 110 {
                if let Some(dir) = std::env::var_os("WBS_EXPORT").map(PathBuf::from) {
                    self.out_dir = dir;
                    self.export_history();
                }
            }
            if self.frames_drawn >= 120 && !live_wait && !self.shot_requested {
                self.shot_requested = true;
                let done = self.shot_done.clone();
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let mut bytes = Vec::with_capacity(w * h * 4);
                    for c in &img.pixels {
                        bytes.extend_from_slice(&c.to_srgba_unmultiplied());
                    }
                    if let Some(parent) = path.parent() {
                        if !parent.as_os_str().is_empty() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                    }
                    if let Err(e) = image::save_buffer(&path, &bytes, w as u32, h as u32, image::ExtendedColorType::Rgba8) {
                        eprintln!("Screenshot konnte nicht gespeichert werden: {e}");
                    }
                    done.store(true, Ordering::SeqCst);
                });
            }
            if self.shot_done.load(Ordering::SeqCst) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn on_exit(&mut self) {
        self.stream = None;
        self.stop_video();
        self.stop_raw_recording("beendet");
        if let Some((l, _)) = self.logger.take() {
            l.finish();
        }
    }
}

/// Temperature input with German decimal comma.
fn temp_dv(v: &mut f32) -> egui::DragValue<'_> {
    egui::DragValue::new(v)
        .speed(0.5)
        .suffix(" °C")
        .custom_formatter(|n, _| fmt_num(n as f32, 1))
        .custom_parser(|s| s.replace(',', ".").replace("°C", "").trim().parse::<f64>().ok())
}
