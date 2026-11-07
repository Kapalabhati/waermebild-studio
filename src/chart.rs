//! Temperature history: stored samples and the time chart.

use crate::thermal::{fmt_num, TempUnit};
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

pub const MAX_SAMPLES: usize = 400_000;

pub struct Series {
    pub key: u64,
    pub name: String,
    pub color: Color32,
    pub values: Vec<f32>,
    pub visible: bool,
}

/// Columns of samples sharing one time axis (seconds since the app started).
pub struct History {
    pub times: Vec<f64>,
    pub series: Vec<Series>,
}

impl History {
    pub fn new() -> Self {
        Self { times: Vec::new(), series: Vec::new() }
    }

    pub fn ensure(&mut self, key: u64, name: &str, color: Color32) {
        if let Some(s) = self.series.iter_mut().find(|s| s.key == key) {
            s.name = name.to_string();
            s.color = color;
            return;
        }
        self.series.push(Series {
            key,
            name: name.to_string(),
            color,
            values: vec![f32::NAN; self.times.len()],
            visible: true,
        });
    }

    pub fn remove(&mut self, key: u64) {
        self.series.retain(|s| s.key != key);
    }

    /// Add one sample; `values` holds (series key, value). Missing series get NaN.
    pub fn push(&mut self, t: f64, values: &[(u64, f32)]) {
        self.times.push(t);
        for s in &mut self.series {
            let v = values.iter().find(|(k, _)| *k == s.key).map(|(_, v)| *v).unwrap_or(f32::NAN);
            s.values.push(v);
        }
        if self.times.len() > MAX_SAMPLES {
            let cut = MAX_SAMPLES / 10;
            self.times.drain(0..cut);
            for s in &mut self.series {
                s.values.drain(0..cut.min(s.values.len()));
            }
        }
    }

    pub fn clear(&mut self) {
        self.times.clear();
        for s in &mut self.series {
            s.values.clear();
        }
    }
}

fn nice_step(range: f64, target: f64) -> f64 {
    let raw = (range / target).max(1e-9);
    let mag = 10f64.powf(raw.log10().floor());
    let n = raw / mag;
    let f = if n < 1.5 {
        1.0
    } else if n < 3.5 {
        2.0
    } else if n < 7.5 {
        5.0
    } else {
        10.0
    };
    f * mag
}

fn fmt_time(secs: f64, span: f64) -> String {
    let s = secs.max(0.0);
    let h = (s / 3600.0).floor();
    let m = ((s - h * 3600.0) / 60.0).floor();
    let sec = s - h * 3600.0 - m * 60.0;
    if h > 0.0 {
        format!("{}:{:02}:{:02}", h as u64, m as u64, sec as u64)
    } else if span < 20.0 {
        format!("{}:{:04.1}", m as u64, sec)
    } else {
        format!("{}:{:02}", m as u64, sec as u64)
    }
}

/// Draw the chart for the last `window` seconds (0 = everything) ending at `now`.
/// `cursor` marks a time (the picture shown of an opened file); returns the time
/// clicked in the chart, if any.
pub fn draw_chart(
    ui: &mut egui::Ui,
    hist: &History,
    now: f64,
    window: f64,
    unit: TempUnit,
    start_label: f64,
    cursor: Option<f64>,
) -> Option<f64> {
    let size = ui.available_size();
    let sense = if cursor.is_some() { Sense::click_and_drag() } else { Sense::hover() };
    let (resp, painter) = ui.allocate_painter(Vec2::new(size.x, size.y.max(80.0)), sense);
    let full = resp.rect;
    painter.rect_filled(full, 4.0, Color32::from_rgb(18, 20, 26));
    let plot = Rect::from_min_max(full.min + Vec2::new(52.0, 8.0), full.max - Vec2::new(12.0, 22.0));
    if plot.width() < 40.0 || plot.height() < 20.0 {
        return None;
    }
    let t_max = now;
    let t_min = if window > 0.0 {
        t_max - window
    } else {
        hist.times.first().copied().unwrap_or(0.0).min(t_max - 10.0)
    };
    let start = hist.times.partition_point(|t| *t < t_min);
    // y range from visible data
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for s in hist.series.iter().filter(|s| s.visible) {
        for v in &s.values[start.min(s.values.len())..] {
            if v.is_finite() {
                let u = unit.from_c(*v);
                lo = lo.min(u);
                hi = hi.max(u);
            }
        }
    }
    if lo > hi {
        lo = unit.from_c(15.0);
        hi = unit.from_c(35.0);
    }
    let pad = ((hi - lo) * 0.08).max(0.5);
    let (y0, y1) = ((lo - pad) as f64, (hi + pad) as f64);
    let to_screen = |t: f64, v: f64| -> Pos2 {
        Pos2::new(
            plot.left() + ((t - t_min) / (t_max - t_min).max(1e-6)) as f32 * plot.width(),
            plot.bottom() - ((v - y0) / (y1 - y0).max(1e-6)) as f32 * plot.height(),
        )
    };
    let grid = Stroke::new(1.0, Color32::from_rgb(40, 44, 54));
    let label_c = Color32::from_rgb(150, 155, 170);
    let font = FontId::proportional(11.0);
    // horizontal grid
    let ys = nice_step(y1 - y0, (plot.height() / 32.0).max(2.0) as f64);
    let mut y = (y0 / ys).ceil() * ys;
    while y <= y1 {
        let p = to_screen(t_min, y);
        painter.line_segment([Pos2::new(plot.left(), p.y), Pos2::new(plot.right(), p.y)], grid);
        painter.text(Pos2::new(plot.left() - 6.0, p.y), Align2::RIGHT_CENTER, fmt_num(y as f32, if ys < 1.0 { 1 } else { 0 }), font.clone(), label_c);
        y += ys;
    }
    painter.text(Pos2::new(full.left() + 4.0, full.top() + 2.0), Align2::LEFT_TOP, unit.label(), font.clone(), label_c);
    // vertical grid (time)
    let span = t_max - t_min;
    let xs = [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0]
        .into_iter()
        .find(|s| span / s <= (plot.width() / 90.0) as f64)
        .unwrap_or(14400.0);
    let mut tx = (t_min / xs).ceil() * xs;
    while tx <= t_max {
        let p = to_screen(tx, y0);
        painter.line_segment([Pos2::new(p.x, plot.top()), Pos2::new(p.x, plot.bottom())], grid);
        if tx >= start_label - 1e-6 {
            painter.text(Pos2::new(p.x, plot.bottom() + 4.0), Align2::CENTER_TOP, fmt_time(tx - start_label, span), font.clone(), label_c);
        }
        tx += xs;
    }
    painter.rect_stroke(plot, 0.0, Stroke::new(1.0, Color32::from_rgb(60, 64, 76)), StrokeKind::Inside);
    // series (decimated to about one point per pixel column)
    let n = hist.times.len().saturating_sub(start);
    let step = (n as f32 / plot.width().max(1.0) / 2.0).floor().max(1.0) as usize;
    let clip = painter.with_clip_rect(plot);
    for s in hist.series.iter().filter(|s| s.visible) {
        let mut pts: Vec<Pos2> = Vec::new();
        let mut i = start;
        while i < hist.times.len() && i < s.values.len() {
            let v = s.values[i];
            if v.is_finite() {
                pts.push(to_screen(hist.times[i], unit.from_c(v) as f64));
            } else if pts.len() > 1 {
                clip.line(std::mem::take(&mut pts), Stroke::new(1.6, s.color));
            } else {
                pts.clear();
            }
            i += step;
        }
        if pts.len() > 1 {
            clip.line(pts, Stroke::new(1.6, s.color));
        }
    }
    // marker of the picture shown
    if let Some(c) = cursor {
        let x = to_screen(c, y0).x;
        if x >= plot.left() - 1.0 && x <= plot.right() + 1.0 {
            painter.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(2.0, Color32::from_rgb(255, 138, 31)));
        }
    }
    let mut clicked = None;
    if cursor.is_some() && (resp.clicked() || resp.dragged()) {
        if let Some(pos) = resp.interact_pointer_pos() {
            let f = ((pos.x - plot.left()) / plot.width()).clamp(0.0, 1.0) as f64;
            clicked = Some(t_min + f * (t_max - t_min));
        }
    }
    // hover readout
    if let Some(pos) = resp.hover_pos() {
        if plot.contains(pos) && !hist.times.is_empty() {
            let t = t_min + ((pos.x - plot.left()) / plot.width()) as f64 * (t_max - t_min);
            let idx = hist.times.partition_point(|x| *x < t).min(hist.times.len() - 1);
            let tt = hist.times[idx];
            let px = to_screen(tt, y0).x;
            painter.line_segment([Pos2::new(px, plot.top()), Pos2::new(px, plot.bottom())], Stroke::new(1.0, Color32::from_white_alpha(90)));
            let mut lines = vec![format!("t = {}", fmt_time(tt - start_label, 1000.0))];
            for s in hist.series.iter().filter(|s| s.visible) {
                if let Some(v) = s.values.get(idx) {
                    if v.is_finite() {
                        clip.circle_filled(to_screen(tt, unit.from_c(*v) as f64), 3.0, s.color);
                        lines.push(format!("{}: {} {}", s.name, fmt_num(unit.from_c(*v), 1), unit.label()));
                    }
                }
            }
            let text = lines.join("\n");
            let galley = painter.layout_no_wrap(text, FontId::proportional(12.0), Color32::WHITE);
            let mut r = Rect::from_min_size(pos + Vec2::new(14.0, 10.0), galley.size() + Vec2::splat(10.0));
            if r.right() > full.right() {
                r = r.translate(Vec2::new(-r.width() - 28.0, 0.0));
            }
            if r.bottom() > full.bottom() {
                r = r.translate(Vec2::new(0.0, full.bottom() - r.bottom()));
            }
            painter.rect_filled(r, 4.0, Color32::from_rgba_unmultiplied(10, 12, 16, 235));
            painter.galley(r.min + Vec2::splat(5.0), galley, Color32::WHITE);
        }
    }
    if hist.times.is_empty() {
        painter.text(plot.center(), Align2::CENTER_CENTER, "Noch keine Messwerte", FontId::proportional(14.0), label_c);
    }
    clicked
}
