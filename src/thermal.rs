//! Temperature maths: raw sensor values, emissivity correction, palettes and statistics.

use eframe::egui::Color32;

/// Sensor size of TC001-class cameras (thermal part of the 256x384 USB frame).
pub const SENSOR_W: usize = 256;
pub const SENSOR_H: usize = 192;

/// One thermal frame with raw 16-bit values (Kelvin * 64).
#[derive(Clone)]
pub struct ThermalFrame {
    pub w: usize,
    pub h: usize,
    pub raw: Vec<u16>,
}

impl ThermalFrame {
    pub fn get(&self, x: usize, y: usize) -> u16 {
        self.raw[y.min(self.h - 1) * self.w + x.min(self.w - 1)]
    }

    /// Rotate (in quarter turns clockwise) and mirror the frame.
    pub fn transformed(&self, quarter_turns: u8, flip_h: bool, flip_v: bool) -> ThermalFrame {
        let q = quarter_turns % 4;
        if q == 0 && !flip_h && !flip_v {
            return self.clone();
        }
        let (ow, oh) = if q % 2 == 1 { (self.h, self.w) } else { (self.w, self.h) };
        let mut out = vec![0u16; ow * oh];
        for oy in 0..oh {
            for ox in 0..ow {
                let mut x = ox;
                let mut y = oy;
                if flip_h {
                    x = ow - 1 - x;
                }
                if flip_v {
                    y = oh - 1 - y;
                }
                // map output (x,y) back to source coordinates
                let (sx, sy) = match q {
                    0 => (x, y),
                    1 => (y, self.h - 1 - x),
                    2 => (self.w - 1 - x, self.h - 1 - y),
                    _ => (self.w - 1 - y, x),
                };
                out[oy * ow + ox] = self.raw[sy * self.w + sx];
            }
        }
        ThermalFrame { w: ow, h: oh, raw: out }
    }
}

pub fn c_to_raw(c: f32) -> u16 {
    ((c + 273.15) * 64.0).round().clamp(0.0, 65535.0) as u16
}

/// Settings that turn the camera's apparent temperature into the object temperature.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct EmissionSettings {
    pub emissivity: f32,
    pub reflected_c: f32,
    pub ambient_c: f32,
    pub distance_m: f32,
    pub humidity: f32,
    pub offset_c: f32,
}

impl Default for EmissionSettings {
    fn default() -> Self {
        Self {
            emissivity: 0.95,
            reflected_c: 22.0,
            ambient_c: 22.0,
            distance_m: 1.0,
            humidity: 0.5,
            offset_c: 0.0,
        }
    }
}

impl EmissionSettings {
    /// Simple estimate of atmospheric transmission for the 8-14 µm band.
    pub fn transmission(&self) -> f32 {
        let alpha = 0.002 + 0.006 * self.humidity.clamp(0.0, 1.0);
        (-alpha * self.distance_m.max(0.0)).exp().clamp(0.3, 1.0)
    }

    /// Correct an apparent (black body) temperature in Kelvin.
    pub fn correct_kelvin(&self, t_app: f32) -> f32 {
        let e = self.emissivity.clamp(0.01, 1.0);
        let tau = self.transmission();
        if (e - 1.0).abs() < 1e-4 && (tau - 1.0).abs() < 1e-4 {
            return t_app;
        }
        let tr = self.reflected_c + 273.15;
        let ta = self.ambient_c + 273.15;
        let w = t_app.powi(4);
        let wr = tr.powi(4);
        let wa = ta.powi(4);
        let wobj = (w - (1.0 - e) * tau * wr - (1.0 - tau) * wa) / (e * tau);
        wobj.max(1.0).powf(0.25)
    }
}

/// Lookup table raw value -> object temperature in °C.
pub struct TempLut {
    pub table: Vec<f32>,
    pub settings: EmissionSettings,
}

impl TempLut {
    pub fn build(s: &EmissionSettings) -> Self {
        let mut table = Vec::with_capacity(65536);
        for r in 0..65536u32 {
            let k = r as f32 / 64.0;
            table.push(s.correct_kelvin(k) - 273.15 + s.offset_c);
        }
        Self { table, settings: *s }
    }

    #[inline]
    pub fn c(&self, raw: u16) -> f32 {
        self.table[raw as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TempUnit {
    Celsius,
    Fahrenheit,
    Kelvin,
}

impl TempUnit {
    pub fn label(&self) -> &'static str {
        match self {
            TempUnit::Celsius => "°C",
            TempUnit::Fahrenheit => "°F",
            TempUnit::Kelvin => "K",
        }
    }
    pub fn from_c(&self, c: f32) -> f32 {
        match self {
            TempUnit::Celsius => c,
            TempUnit::Fahrenheit => c * 9.0 / 5.0 + 32.0,
            TempUnit::Kelvin => c + 273.15,
        }
    }
}

/// German number formatting with a decimal comma.
pub fn fmt_num(v: f32, decimals: usize) -> String {
    if !v.is_finite() {
        return "–".into();
    }
    format!("{:.*}", decimals, v).replace('.', ",")
}

pub fn fmt_t(c: f32, unit: TempUnit) -> String {
    if !c.is_finite() {
        return "–".into();
    }
    format!("{} {}", fmt_num(unit.from_c(c), 1), unit.label())
}

// ---------------------------------------------------------------- palettes

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Palette {
    Iron,
    Rainbow,
    WhiteHot,
    BlackHot,
    Lava,
    Arctic,
    HighContrast,
    Medical,
}

impl Palette {
    pub const ALL: [Palette; 8] = [
        Palette::Iron,
        Palette::Rainbow,
        Palette::WhiteHot,
        Palette::BlackHot,
        Palette::Lava,
        Palette::Arctic,
        Palette::HighContrast,
        Palette::Medical,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Palette::Iron => "Eisen",
            Palette::Rainbow => "Regenbogen",
            Palette::WhiteHot => "Weiß = heiß",
            Palette::BlackHot => "Schwarz = heiß",
            Palette::Lava => "Lava",
            Palette::Arctic => "Arktis",
            Palette::HighContrast => "Hoher Kontrast",
            Palette::Medical => "Medizin",
        }
    }

    fn stops(&self) -> &'static [(f32, [u8; 3])] {
        match self {
            Palette::Iron => &[
                (0.00, [0, 0, 0]),
                (0.12, [28, 4, 92]),
                (0.30, [112, 8, 150]),
                (0.47, [196, 30, 98]),
                (0.62, [234, 88, 22]),
                (0.79, [250, 168, 0]),
                (0.92, [255, 230, 84]),
                (1.00, [255, 255, 238]),
            ],
            Palette::Rainbow => &[
                (0.00, [8, 0, 120]),
                (0.15, [0, 30, 255]),
                (0.33, [0, 200, 255]),
                (0.50, [0, 220, 80]),
                (0.66, [235, 235, 0]),
                (0.83, [255, 120, 0]),
                (0.95, [255, 0, 0]),
                (1.00, [255, 200, 200]),
            ],
            Palette::WhiteHot => &[(0.0, [0, 0, 0]), (1.0, [255, 255, 255])],
            Palette::BlackHot => &[(0.0, [255, 255, 255]), (1.0, [0, 0, 0])],
            Palette::Lava => &[
                (0.00, [0, 0, 0]),
                (0.25, [70, 6, 6]),
                (0.50, [180, 20, 0]),
                (0.72, [245, 100, 0]),
                (0.88, [255, 200, 40]),
                (1.00, [255, 255, 220]),
            ],
            Palette::Arctic => &[
                (0.00, [6, 10, 60]),
                (0.35, [24, 80, 190]),
                (0.62, [110, 190, 240]),
                (0.78, [235, 235, 245]),
                (0.90, [250, 190, 70]),
                (1.00, [255, 120, 0]),
            ],
            Palette::HighContrast => &[
                (0.00, [0, 0, 0]),
                (0.14, [0, 0, 255]),
                (0.28, [0, 255, 255]),
                (0.42, [0, 255, 0]),
                (0.56, [255, 255, 0]),
                (0.70, [255, 0, 0]),
                (0.84, [255, 0, 255]),
                (1.00, [255, 255, 255]),
            ],
            Palette::Medical => &[
                (0.00, [10, 20, 80]),
                (0.30, [20, 110, 200]),
                (0.50, [30, 190, 90]),
                (0.70, [240, 230, 30]),
                (0.85, [240, 110, 20]),
                (1.00, [210, 0, 0]),
            ],
        }
    }

    pub fn lut(&self) -> Vec<Color32> {
        let stops = self.stops();
        (0..256)
            .map(|i| {
                let t = i as f32 / 255.0;
                let mut k = 0;
                while k + 1 < stops.len() - 1 && t > stops[k + 1].0 {
                    k += 1;
                }
                let (t0, c0) = stops[k];
                let (t1, c1) = stops[(k + 1).min(stops.len() - 1)];
                let f = if t1 > t0 { ((t - t0) / (t1 - t0)).clamp(0.0, 1.0) } else { 0.0 };
                let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
                Color32::from_rgb(mix(c0[0], c1[0]), mix(c0[1], c1[1]), mix(c0[2], c1[2]))
            })
            .collect()
    }
}

// ---------------------------------------------------------------- statistics

#[derive(Clone, Copy, Default, Debug)]
pub struct FrameStats {
    pub min: f32,
    pub max: f32,
    pub mean: f32,
    pub center: f32,
    pub min_pos: (usize, usize),
    pub max_pos: (usize, usize),
}

pub fn frame_stats(temps: &[f32], w: usize, h: usize) -> FrameStats {
    let mut s = FrameStats { min: f32::MAX, max: f32::MIN, ..Default::default() };
    let mut sum = 0.0f64;
    for (i, &t) in temps.iter().enumerate() {
        sum += t as f64;
        if t < s.min {
            s.min = t;
            s.min_pos = (i % w, i / w);
        }
        if t > s.max {
            s.max = t;
            s.max_pos = (i % w, i / w);
        }
    }
    s.mean = (sum / temps.len().max(1) as f64) as f32;
    s.center = spot_value(temps, w, h, w / 2, h / 2);
    s
}

/// 3x3 mean around a pixel.
pub fn spot_value(temps: &[f32], w: usize, h: usize, x: usize, y: usize) -> f32 {
    let mut sum = 0.0;
    let mut n = 0.0;
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            let xx = x as i32 + dx;
            let yy = y as i32 + dy;
            if xx >= 0 && yy >= 0 && (xx as usize) < w && (yy as usize) < h {
                sum += temps[yy as usize * w + xx as usize];
                n += 1.0;
            }
        }
    }
    if n > 0.0 {
        sum / n
    } else {
        f32::NAN
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RegionStats {
    pub min: f32,
    pub max: f32,
    pub mean: f32,
    pub median: f32,
    pub std: f32,
    pub count: usize,
}

pub fn region_stats(values: &mut Vec<f32>) -> Option<RegionStats> {
    if values.is_empty() {
        return None;
    }
    let n = values.len();
    let mean = values.iter().map(|v| *v as f64).sum::<f64>() / n as f64;
    let var = values.iter().map(|v| (*v as f64 - mean).powi(2)).sum::<f64>() / n as f64;
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(RegionStats {
        min: values[0],
        max: values[n - 1],
        mean: mean as f32,
        median: values[n / 2],
        std: var.sqrt() as f32,
        count: n,
    })
}

/// Pixels along a line (Bresenham-style sampling).
pub fn line_pixels(x0: f32, y0: f32, x1: f32, y1: f32, w: usize, h: usize) -> Vec<(usize, usize)> {
    let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt().max(1.0);
    let n = len.ceil() as usize + 1;
    (0..n)
        .map(|i| {
            let f = i as f32 / (n - 1).max(1) as f32;
            let x = (x0 + (x1 - x0) * f).round().clamp(0.0, (w - 1) as f32) as usize;
            let y = (y0 + (y1 - y0) * f).round().clamp(0.0, (h - 1) as f32) as usize;
            (x, y)
        })
        .collect()
}

/// Material presets for the emissivity setting.
pub const MATERIALS: &[(&str, f32)] = &[
    ("Menschliche Haut", 0.98),
    ("Wasser", 0.96),
    ("Lack, matt schwarz", 0.97),
    ("Lack, hell", 0.90),
    ("Holz", 0.90),
    ("Beton", 0.92),
    ("Ziegel", 0.93),
    ("Putz", 0.91),
    ("Papier", 0.93),
    ("Kunststoff", 0.91),
    ("Gummi", 0.95),
    ("Glas", 0.92),
    ("Keramik / Fliese", 0.92),
    ("Textil", 0.90),
    ("Erde / Boden", 0.93),
    ("Isolierband", 0.95),
    ("Stahl, oxidiert", 0.79),
    ("Gusseisen, oxidiert", 0.64),
    ("Kupfer, oxidiert", 0.65),
    ("Aluminium, eloxiert", 0.77),
    ("Aluminium, oxidiert", 0.30),
    ("Edelstahl", 0.16),
    ("Aluminium, poliert", 0.05),
];
