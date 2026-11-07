//! Saving: colour pictures (PNG), video (MJPEG-AVI), raw temperature recordings and CSV files.

use crate::thermal::{fmt_num, ThermalFrame};
use eframe::egui::Color32;
use image::{ImageBuffer, Rgb, RgbImage};
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::thread::JoinHandle;
use std::time::Instant;

pub fn timestamp_name(prefix: &str) -> String {
    format!("{}_{}", prefix, chrono::Local::now().format("%Y-%m-%d_%H-%M-%S"))
}

/// Shapes burnt into pictures and videos.
pub enum Mark {
    Cross { x: f32, y: f32, color: Color32 },
    Rect { x0: f32, y0: f32, x1: f32, y1: f32, color: Color32 },
    Line { x0: f32, y0: f32, x1: f32, y1: f32, color: Color32 },
}

/// Colour picture of a frame, scaled up, with marks and an optional colour bar.
pub fn render_rgb(
    colors: &[Color32],
    w: usize,
    h: usize,
    scale: u32,
    marks: &[Mark],
    bar: Option<&[Color32]>,
) -> RgbImage {
    let small: RgbImage = ImageBuffer::from_fn(w as u32, h as u32, |x, y| {
        let c = colors[y as usize * w + x as usize];
        Rgb([c.r(), c.g(), c.b()])
    });
    let s = scale.max(1);
    let mut img = if s > 1 {
        image::imageops::resize(&small, w as u32 * s, h as u32 * s, image::imageops::FilterType::Triangle)
    } else {
        small
    };
    let sf = s as f32;
    for m in marks {
        match *m {
            Mark::Cross { x, y, color } => {
                let (cx, cy) = ((x + 0.5) * sf, (y + 0.5) * sf);
                let r = 5.0 * sf.max(2.0) / 2.0;
                draw_line(&mut img, cx - r, cy, cx + r, cy, color);
                draw_line(&mut img, cx, cy - r, cx, cy + r, color);
            }
            Mark::Rect { x0, y0, x1, y1, color } => {
                let (a, b, c, d) = (x0 * sf, y0 * sf, (x1 + 1.0) * sf - 1.0, (y1 + 1.0) * sf - 1.0);
                draw_line(&mut img, a, b, c, b, color);
                draw_line(&mut img, c, b, c, d, color);
                draw_line(&mut img, c, d, a, d, color);
                draw_line(&mut img, a, d, a, b, color);
            }
            Mark::Line { x0, y0, x1, y1, color } => {
                draw_line(&mut img, (x0 + 0.5) * sf, (y0 + 0.5) * sf, (x1 + 0.5) * sf, (y1 + 0.5) * sf, color);
            }
        }
    }
    if let Some(lut) = bar {
        let (iw, ih) = img.dimensions();
        let bw = (6 * s).max(10);
        let mut out = RgbImage::from_pixel(iw + bw + 4, ih, Rgb([16, 16, 20]));
        image::imageops::overlay(&mut out, &img, 0, 0);
        for y in 0..ih {
            let idx = 255 - ((y as f32 / (ih - 1).max(1) as f32) * 255.0).round() as usize;
            let c = lut[idx.min(255)];
            for x in 0..bw {
                out.put_pixel(iw + 4 + x, y, Rgb([c.r(), c.g(), c.b()]));
            }
        }
        out
    } else {
        img
    }
}

fn draw_line(img: &mut RgbImage, x0: f32, y0: f32, x1: f32, y1: f32, color: Color32) {
    let (w, h) = img.dimensions();
    let n = ((x1 - x0).abs().max((y1 - y0).abs()).ceil() as usize).max(1);
    for i in 0..=n {
        let f = i as f32 / n as f32;
        let x = x0 + (x1 - x0) * f;
        let y = y0 + (y1 - y0) * f;
        for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0)] {
            let (px, py) = ((x + dx) as i64, (y + dy) as i64);
            if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                img.put_pixel(px as u32, py as u32, Rgb([color.r(), color.g(), color.b()]));
            }
        }
    }
}

pub fn save_png(img: &RgbImage, path: &Path) -> Result<(), String> {
    img.save(path).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- CSV

pub struct CsvStyle {
    /// German Excel: `;` between columns and a decimal comma.
    pub german: bool,
}

impl CsvStyle {
    pub fn sep(&self) -> &'static str {
        if self.german {
            ";"
        } else {
            ","
        }
    }
    pub fn num(&self, v: f32, d: usize) -> String {
        if !v.is_finite() {
            return String::new();
        }
        if self.german {
            fmt_num(v, d)
        } else {
            format!("{:.*}", d, v)
        }
    }
}

/// Temperature matrix of one frame (°C) as CSV.
pub fn save_matrix_csv(temps: &[f32], w: usize, h: usize, style: &CsvStyle, path: &Path) -> Result<(), String> {
    let f = File::create(path).map_err(|e| e.to_string())?;
    let mut out = BufWriter::new(f);
    for y in 0..h {
        let row: Vec<String> = (0..w).map(|x| style.num(temps[y * w + x], 2)).collect();
        writeln!(out, "{}", row.join(style.sep())).map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())
}

/// A CSV file that gets one row per sample.
pub struct CsvLogger {
    out: BufWriter<File>,
    pub path: PathBuf,
    pub rows: u64,
    pub started: Instant,
    #[allow(dead_code)]
    pub columns: usize,
}

impl CsvLogger {
    pub fn create(path: PathBuf, header: &[String], style: &CsvStyle) -> Result<Self, String> {
        let f = File::create(&path).map_err(|e| e.to_string())?;
        let mut out = BufWriter::new(f);
        if style.german {
            // BOM so Excel shows umlauts and °C correctly
            out.write_all(&[0xEF, 0xBB, 0xBF]).map_err(|e| e.to_string())?;
        }
        writeln!(out, "{}", header.join(style.sep())).map_err(|e| e.to_string())?;
        Ok(Self { out, path, rows: 0, started: Instant::now(), columns: header.len() })
    }

    pub fn row(&mut self, cells: &[String], style: &CsvStyle) {
        let _ = writeln!(self.out, "{}", cells.join(style.sep()));
        self.rows += 1;
        if self.rows % 50 == 0 {
            let _ = self.out.flush();
        }
    }

    pub fn finish(mut self) {
        let _ = self.out.flush();
    }
}

// ---------------------------------------------------------------- raw recordings

const RAW_MAGIC: &[u8; 8] = b"TCRAW001";
const RAW_HEADER: u64 = 24;

/// Raw radiometric recording: every frame keeps its 16-bit sensor values, so it can be
/// evaluated again later with other emissivity settings.
pub struct RawWriter {
    out: BufWriter<File>,
    pub path: PathBuf,
    pub frames: u64,
    pub started: Instant,
    w: usize,
    h: usize,
}

impl RawWriter {
    pub fn create(path: PathBuf, w: usize, h: usize) -> Result<Self, String> {
        let f = File::create(&path).map_err(|e| e.to_string())?;
        let mut out = BufWriter::with_capacity(1 << 20, f);
        let mut hdr = Vec::with_capacity(RAW_HEADER as usize);
        hdr.extend_from_slice(RAW_MAGIC);
        hdr.extend_from_slice(&(w as u32).to_le_bytes());
        hdr.extend_from_slice(&(h as u32).to_le_bytes());
        hdr.extend_from_slice(&[0u8; 8]);
        out.write_all(&hdr).map_err(|e| e.to_string())?;
        Ok(Self { out, path, frames: 0, started: Instant::now(), w, h })
    }

    /// Returns false when the frame size changed (e.g. after rotating).
    pub fn push(&mut self, f: &ThermalFrame) -> bool {
        if f.w != self.w || f.h != self.h {
            return false;
        }
        let t_ms = self.started.elapsed().as_secs_f64() * 1000.0;
        let mut buf = Vec::with_capacity(8 + f.raw.len() * 2);
        buf.extend_from_slice(&t_ms.to_le_bytes());
        for v in &f.raw {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        let _ = self.out.write_all(&buf);
        self.frames += 1;
        true
    }

    pub fn bytes(&self) -> u64 {
        RAW_HEADER + self.frames * (8 + (self.w * self.h * 2) as u64)
    }

    pub fn finish(mut self) {
        let _ = self.out.flush();
    }
}

pub fn save_raw_single(f: &ThermalFrame, path: PathBuf) -> Result<(), String> {
    let mut w = RawWriter::create(path, f.w, f.h)?;
    w.push(f);
    w.finish();
    Ok(())
}

pub struct RawReader {
    file: File,
    #[allow(dead_code)]
    pub path: PathBuf,
    pub w: usize,
    pub h: usize,
    pub frames: usize,
    pub duration_ms: f64,
}

impl RawReader {
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let mut hdr = [0u8; RAW_HEADER as usize];
        file.read_exact(&mut hdr).map_err(|_| "Datei zu kurz".to_string())?;
        if &hdr[0..8] != RAW_MAGIC {
            return Err("keine Rohdaten-Aufnahme dieser App (.tcraw)".into());
        }
        let w = u32::from_le_bytes(hdr[8..12].try_into().unwrap()) as usize;
        let h = u32::from_le_bytes(hdr[12..16].try_into().unwrap()) as usize;
        if w == 0 || h == 0 || w > 4096 || h > 4096 {
            return Err("ungültige Bildgröße".into());
        }
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let fsz = 8 + (w * h * 2) as u64;
        let frames = ((len.saturating_sub(RAW_HEADER)) / fsz) as usize;
        if frames == 0 {
            return Err("keine Bilder in der Datei".into());
        }
        let mut r = Self { file, path: path.to_path_buf(), w, h, frames, duration_ms: 0.0 };
        if let Ok((t, _)) = r.read(frames - 1) {
            r.duration_ms = t;
        }
        Ok(r)
    }

    pub fn read(&mut self, index: usize) -> Result<(f64, ThermalFrame), String> {
        let fsz = 8 + (self.w * self.h * 2) as u64;
        let i = index.min(self.frames - 1) as u64;
        self.file.seek(SeekFrom::Start(RAW_HEADER + i * fsz)).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; fsz as usize];
        self.file.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let t = f64::from_le_bytes(buf[0..8].try_into().unwrap());
        let raw = buf[8..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Ok((t, ThermalFrame { w: self.w, h: self.h, raw }))
    }

    /// Index of the frame shown at time `t_ms` (binary search over the stored times).
    pub fn index_at(&mut self, t_ms: f64) -> usize {
        let (mut lo, mut hi) = (0usize, self.frames - 1);
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            let tm = self.time_of(mid);
            if tm <= t_ms {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo
    }

    fn time_of(&mut self, index: usize) -> f64 {
        let fsz = 8 + (self.w * self.h * 2) as u64;
        let mut b = [0u8; 8];
        if self.file.seek(SeekFrom::Start(RAW_HEADER + index as u64 * fsz)).is_ok() && self.file.read_exact(&mut b).is_ok() {
            f64::from_le_bytes(b)
        } else {
            0.0
        }
    }
}

// ---------------------------------------------------------------- opening files for analysis

/// Frames to look at again: one streamed recording, or pictures loaded into memory
/// (single photos, interval series, CSV tables).
pub enum FrameStore {
    File(RawReader),
    Memory { frames: Vec<(f64, ThermalFrame)>, names: Vec<String> },
}

impl FrameStore {
    pub fn frames(&self) -> usize {
        match self {
            FrameStore::File(r) => r.frames,
            FrameStore::Memory { frames, .. } => frames.len(),
        }
    }

    pub fn duration_ms(&self) -> f64 {
        match self {
            FrameStore::File(r) => r.duration_ms,
            FrameStore::Memory { frames, .. } => frames.last().map(|f| f.0).unwrap_or(0.0),
        }
    }

    pub fn read(&mut self, index: usize) -> Result<(f64, ThermalFrame), String> {
        match self {
            FrameStore::File(r) => r.read(index),
            FrameStore::Memory { frames, .. } => {
                frames.get(index.min(frames.len().saturating_sub(1))).cloned().ok_or_else(|| "leer".to_string())
            }
        }
    }

    pub fn index_at(&mut self, t_ms: f64) -> usize {
        match self {
            FrameStore::File(r) => r.index_at(t_ms),
            FrameStore::Memory { frames, .. } => frames.partition_point(|f| f.0 <= t_ms).saturating_sub(1),
        }
    }

    /// File name of a picture of a series (empty for a single recording).
    pub fn name_of(&self, index: usize) -> &str {
        match self {
            FrameStore::File(_) => "",
            FrameStore::Memory { names, .. } => names.get(index).map(|s| s.as_str()).unwrap_or(""),
        }
    }
}

/// Temperature table (°C) as saved by "Temperatur-Tabelle mitspeichern".
/// Accepts ";" with decimal comma, "," with decimal point and tabs; skips text rows.
pub fn load_matrix_csv(path: &Path) -> Result<(usize, usize, Vec<f32>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let text = text.trim_start_matches('\u{feff}');
    let mut rows: Vec<Vec<f32>> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (sep, comma_decimal) = if line.contains(';') {
            (';', true)
        } else if line.contains('\t') {
            ('\t', true)
        } else {
            (',', false)
        };
        let vals: Option<Vec<f32>> = line
            .split(sep)
            .map(|c| {
                let c = c.trim().trim_matches('"');
                let c = if comma_decimal { c.replace(',', ".") } else { c.to_string() };
                c.parse::<f32>().ok()
            })
            .collect();
        match vals {
            Some(v) if !v.is_empty() => rows.push(v),
            _ if rows.is_empty() => continue, // header lines
            _ => return Err(format!("Zeile {} ist keine Zahlenreihe", rows.len() + 1)),
        }
    }
    let h = rows.len();
    let w = rows.first().map(|r| r.len()).unwrap_or(0);
    if w < 2 || h < 2 {
        return Err("keine Temperatur-Tabelle gefunden".into());
    }
    if rows.iter().any(|r| r.len() != w) {
        return Err("Zeilen sind unterschiedlich lang".into());
    }
    Ok((w, h, rows.into_iter().flatten().collect()))
}

/// Turns object temperatures back into raw sensor values, given the table
/// raw → °C that was used when the temperatures were calculated.
pub fn temps_to_raw(temps: &[f32], table: &[f32]) -> Vec<u16> {
    temps
        .iter()
        .map(|&t| {
            let i = table.partition_point(|&v| v < t).min(table.len() - 1);
            if i > 0 && (t - table[i - 1]).abs() < (table[i] - t).abs() {
                (i - 1) as u16
            } else {
                i as u16
            }
        })
        .collect()
}

/// Loads every frame of every given .tcraw file into memory, ordered by file name.
/// Times come from the stored frame times, or from the file dates for single photos.
pub fn load_raw_series(paths: &[PathBuf]) -> Result<FrameStore, String> {
    let mut paths = paths.to_vec();
    paths.sort();
    let mut frames = Vec::new();
    let mut names = Vec::new();
    let mut first_mtime: Option<std::time::SystemTime> = None;
    let mut last_t = 0.0f64;
    let mut size: Option<(usize, usize)> = None;
    for p in &paths {
        let mut r = RawReader::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
        if let Some(s) = size {
            if s != (r.w, r.h) {
                return Err(format!("{}: andere Bildgröße als die übrigen Dateien", p.display()));
            }
        }
        size = Some((r.w, r.h));
        let mtime = std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let base_ms = match (mtime, first_mtime) {
            (Some(m), Some(f)) => m.duration_since(f).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0),
            (Some(m), None) => {
                first_mtime = Some(m);
                0.0
            }
            _ => 0.0,
        };
        let base_ms = base_ms.max(if frames.is_empty() { 0.0 } else { last_t + 1.0 });
        let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        for i in 0..r.frames {
            let (t, f) = r.read(i)?;
            last_t = base_ms + t;
            frames.push((last_t, f));
            names.push(if r.frames > 1 { format!("{name} [{}]", i + 1) } else { name.clone() });
        }
    }
    if frames.is_empty() {
        return Err("keine Bilder gefunden".into());
    }
    Ok(FrameStore::Memory { frames, names })
}

// ---------------------------------------------------------------- video (MJPEG AVI)

struct AviWriter {
    out: BufWriter<File>,
    index: Vec<(u32, u32)>,
    pos: u64,
    w: u32,
    h: u32,
    max_size: u32,
}

const MOVI_FOURCC_POS: u64 = 220;

impl AviWriter {
    fn create(path: &Path, w: u32, h: u32) -> std::io::Result<Self> {
        let f = File::create(path)?;
        let mut out = BufWriter::with_capacity(1 << 20, f);
        let mut hd: Vec<u8> = Vec::with_capacity(224);
        let u32le = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
        hd.extend_from_slice(b"RIFF");
        u32le(&mut hd, 0);
        hd.extend_from_slice(b"AVI ");
        hd.extend_from_slice(b"LIST");
        u32le(&mut hd, 192);
        hd.extend_from_slice(b"hdrl");
        hd.extend_from_slice(b"avih");
        u32le(&mut hd, 56);
        u32le(&mut hd, 40_000); // µs per frame (patched)
        u32le(&mut hd, 0);
        u32le(&mut hd, 0);
        u32le(&mut hd, 0x10); // AVIF_HASINDEX
        u32le(&mut hd, 0); // total frames (patched)
        u32le(&mut hd, 0);
        u32le(&mut hd, 1);
        u32le(&mut hd, 0);
        u32le(&mut hd, w);
        u32le(&mut hd, h);
        hd.extend_from_slice(&[0u8; 16]);
        hd.extend_from_slice(b"LIST");
        u32le(&mut hd, 116);
        hd.extend_from_slice(b"strl");
        hd.extend_from_slice(b"strh");
        u32le(&mut hd, 56);
        hd.extend_from_slice(b"vids");
        hd.extend_from_slice(b"MJPG");
        u32le(&mut hd, 0);
        u32le(&mut hd, 0);
        u32le(&mut hd, 0);
        u32le(&mut hd, 1000); // scale (patched)
        u32le(&mut hd, 25_000); // rate (patched)
        u32le(&mut hd, 0);
        u32le(&mut hd, 0); // length (patched)
        u32le(&mut hd, 0);
        u32le(&mut hd, 0xFFFF_FFFF);
        u32le(&mut hd, 0);
        hd.extend_from_slice(&0u16.to_le_bytes());
        hd.extend_from_slice(&0u16.to_le_bytes());
        hd.extend_from_slice(&(w as u16).to_le_bytes());
        hd.extend_from_slice(&(h as u16).to_le_bytes());
        hd.extend_from_slice(b"strf");
        u32le(&mut hd, 40);
        u32le(&mut hd, 40);
        u32le(&mut hd, w);
        u32le(&mut hd, h);
        hd.extend_from_slice(&1u16.to_le_bytes());
        hd.extend_from_slice(&24u16.to_le_bytes());
        hd.extend_from_slice(b"MJPG");
        u32le(&mut hd, w * h * 3);
        hd.extend_from_slice(&[0u8; 16]);
        hd.extend_from_slice(b"LIST");
        u32le(&mut hd, 0); // movi size (patched)
        hd.extend_from_slice(b"movi");
        debug_assert_eq!(hd.len(), 224);
        out.write_all(&hd)?;
        Ok(Self { out, index: Vec::new(), pos: 224, w, h, max_size: 0 })
    }

    fn push_jpeg(&mut self, jpg: &[u8]) -> std::io::Result<()> {
        let len = jpg.len() as u32;
        let offset = (self.pos - MOVI_FOURCC_POS) as u32;
        self.out.write_all(b"00dc")?;
        self.out.write_all(&len.to_le_bytes())?;
        self.out.write_all(jpg)?;
        let mut written = 8 + len as u64;
        if len % 2 == 1 {
            self.out.write_all(&[0])?;
            written += 1;
        }
        self.pos += written;
        self.index.push((offset, len));
        self.max_size = self.max_size.max(len);
        Ok(())
    }

    fn finish(mut self, fps: f64) -> std::io::Result<()> {
        let movi_end = self.pos;
        self.out.write_all(b"idx1")?;
        self.out.write_all(&((self.index.len() * 16) as u32).to_le_bytes())?;
        for (off, len) in &self.index {
            self.out.write_all(b"00dc")?;
            self.out.write_all(&0x10u32.to_le_bytes())?;
            self.out.write_all(&off.to_le_bytes())?;
            self.out.write_all(&len.to_le_bytes())?;
        }
        let file_end = movi_end + 8 + self.index.len() as u64 * 16;
        let n = self.index.len() as u32;
        let fps = if fps.is_finite() && fps > 0.5 { fps } else { 25.0 };
        let mut patch = |pos: u64, v: u32| -> std::io::Result<()> {
            self.out.seek(SeekFrom::Start(pos))?;
            self.out.write_all(&v.to_le_bytes())
        };
        patch(4, (file_end - 8) as u32)?;
        patch(32, (1_000_000.0 / fps) as u32)?;
        patch(48, n)?;
        patch(60, self.max_size)?;
        patch(128, 1000)?;
        patch(132, (fps * 1000.0).round() as u32)?;
        patch(140, n)?;
        patch(144, self.max_size)?;
        patch(216, (movi_end - 216 - 4) as u32)?;
        let _ = (self.w, self.h);
        self.out.flush()
    }
}

enum VideoMsg {
    Frame(RgbImage),
    Finish,
}

/// Video recording on a worker thread (JPEG encoding takes a few milliseconds per frame).
pub struct VideoRecorder {
    tx: Sender<VideoMsg>,
    handle: Option<JoinHandle<Result<(), String>>>,
    pub path: PathBuf,
    pub frames: u64,
    pub started: Instant,
    pub size: (u32, u32),
}

impl VideoRecorder {
    pub fn start(path: PathBuf, w: u32, h: u32) -> Result<Self, String> {
        let mut avi = AviWriter::create(&path, w, h).map_err(|e| e.to_string())?;
        let (tx, rx) = channel::<VideoMsg>();
        let started = Instant::now();
        let handle = std::thread::Builder::new()
            .name("video".into())
            .spawn(move || -> Result<(), String> {
                let t0 = Instant::now();
                let mut n = 0u64;
                while let Ok(msg) = rx.recv() {
                    match msg {
                        VideoMsg::Frame(img) => {
                            if img.width() != w || img.height() != h {
                                continue;
                            }
                            let mut jpg = Vec::with_capacity(64 * 1024);
                            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 88);
                            enc.encode(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
                                .map_err(|e| e.to_string())?;
                            avi.push_jpeg(&jpg).map_err(|e| e.to_string())?;
                            n += 1;
                        }
                        VideoMsg::Finish => break,
                    }
                }
                let secs = t0.elapsed().as_secs_f64();
                let fps = if secs > 0.2 { n as f64 / secs } else { 25.0 };
                avi.finish(fps).map_err(|e| e.to_string())
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { tx, handle: Some(handle), path, frames: 0, started, size: (w, h) })
    }

    pub fn push(&mut self, img: RgbImage) {
        if self.tx.send(VideoMsg::Frame(img)).is_ok() {
            self.frames += 1;
        }
    }

    pub fn finish(mut self) -> Result<(), String> {
        let _ = self.tx.send(VideoMsg::Finish);
        match self.handle.take() {
            Some(h) => h.join().map_err(|_| "Video-Thread abgestürzt".to_string())?,
            None => Ok(()),
        }
    }
}

pub fn open_folder(path: &Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_back_to_raw() {
        let lut = crate::thermal::TempLut::build(&crate::thermal::EmissionSettings::default());
        let raw: Vec<u16> = vec![18000, 19000, 20000, 21000];
        let temps: Vec<f32> = raw.iter().map(|r| lut.c(*r)).collect();
        let dir = std::env::temp_dir().join("wbs_csv_test.csv");
        let style = CsvStyle { german: true };
        save_matrix_csv(&temps, 2, 2, &style, &dir).unwrap();
        let (w, h, t2) = load_matrix_csv(&dir).unwrap();
        assert_eq!((w, h), (2, 2));
        let back = temps_to_raw(&t2, &lut.table);
        for (a, b) in raw.iter().zip(&back) {
            assert!((*a as i32 - *b as i32).abs() <= 2, "{a} vs {b}");
        }
        let _ = std::fs::remove_file(dir);
    }

    #[test]
    fn raw_roundtrip() {
        let dir = std::env::temp_dir().join("wbs_test_raw");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.tcraw");
        let f = ThermalFrame { w: 4, h: 3, raw: (0..12).map(|i| 18000 + i).collect() };
        let mut w = RawWriter::create(p.clone(), 4, 3).unwrap();
        for _ in 0..5 {
            assert!(w.push(&f));
        }
        w.finish();
        let mut r = RawReader::open(&p).unwrap();
        assert_eq!((r.w, r.h, r.frames), (4, 3, 5));
        let (_, g) = r.read(3).unwrap();
        assert_eq!(g.raw, f.raw);
    }

    #[test]
    fn avi_structure() {
        let dir = std::env::temp_dir().join("wbs_test_avi");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.avi");
        let mut v = VideoRecorder::start(p.clone(), 64, 48).unwrap();
        for i in 0..10u8 {
            v.push(RgbImage::from_pixel(64, 48, Rgb([i * 20, 100, 200])));
        }
        v.finish().unwrap();
        let data = std::fs::read(&p).unwrap();
        assert_eq!(&data[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize, data.len() - 8);
        assert_eq!(&data[220..224], b"movi");
        assert_eq!(u32::from_le_bytes(data[48..52].try_into().unwrap()), 10);
        let movi = u32::from_le_bytes(data[216..220].try_into().unwrap()) as usize;
        assert_eq!(&data[220 + movi..220 + movi + 4], b"idx1");
        assert_eq!(&data[224..228], b"00dc");
        // first chunk is a JPEG
        assert_eq!(&data[232..234], &[0xFF, 0xD8]);
    }
}
