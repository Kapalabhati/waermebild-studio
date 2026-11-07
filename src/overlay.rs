//! Text and legends burnt into saved pictures and videos: value labels next to the
//! measuring tools, a colour scale with temperatures and an info strip with date/time.

use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use eframe::egui::Color32;
use image::{Rgb, RgbImage};
use std::sync::OnceLock;

fn font() -> &'static FontRef<'static> {
    static F: OnceLock<FontRef<'static>> = OnceLock::new();
    F.get_or_init(|| FontRef::try_from_slice(epaint_default_fonts::HACK_REGULAR).expect("built-in font"))
}

pub fn text_width(text: &str, px: f32) -> f32 {
    let f = font();
    let sf = f.as_scaled(PxScale::from(px));
    text.chars().map(|c| sf.h_advance(f.glyph_id(c))).sum()
}

fn blend(img: &mut RgbImage, x: i32, y: i32, c: Color32, a: f32) {
    let (w, h) = img.dimensions();
    if x < 0 || y < 0 || x as u32 >= w || y as u32 >= h || a <= 0.0 {
        return;
    }
    let a = a.min(1.0);
    let p = img.get_pixel_mut(x as u32, y as u32);
    let mix = |o: u8, n: u8| (o as f32 * (1.0 - a) + n as f32 * a).round() as u8;
    *p = Rgb([mix(p[0], c.r()), mix(p[1], c.g()), mix(p[2], c.b())]);
}

/// Text with its top-left corner at (x, y).
pub fn draw_text(img: &mut RgbImage, x: f32, y: f32, text: &str, px: f32, color: Color32) {
    let f = font();
    let scale = PxScale::from(px);
    let sf = f.as_scaled(scale);
    let base = y + sf.ascent();
    let mut cx = x;
    for c in text.chars() {
        let id = f.glyph_id(c);
        if let Some(o) = f.outline_glyph(id.with_scale_and_position(scale, point(cx, base))) {
            let b = o.px_bounds();
            o.draw(|gx, gy, cov| blend(img, b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32, color, cov));
        }
        cx += sf.h_advance(id);
    }
}

fn fill_rect(img: &mut RgbImage, x0: f32, y0: f32, x1: f32, y1: f32, c: Color32, a: f32) {
    for y in y0.round() as i32..y1.round() as i32 {
        for x in x0.round() as i32..x1.round() as i32 {
            blend(img, x, y, c, a);
        }
    }
}

fn box_size(text: &str, px: f32) -> (f32, f32, f32) {
    let pad = (px * 0.3).round();
    (text_width(text, px) + 2.0 * pad, px * 1.25 + pad, pad)
}

/// Text in a dark, half transparent box with its top-left corner at (x, y).
fn label_box(img: &mut RgbImage, x: f32, y: f32, text: &str, px: f32, color: Color32) {
    let (w, h, pad) = box_size(text, px);
    fill_rect(img, x, y, x + w, y + h, Color32::BLACK, 0.68);
    draw_text(img, x + pad, y + pad * 0.5, text, px, color);
}

/// How a label sits relative to its anchor.
#[derive(Clone, Copy)]
pub enum Anchor {
    /// next to a point (spot, hottest/coldest point, line start)
    Point,
    /// at the top-left corner of a rectangle, preferably above it
    Corner,
}

/// A value label anchored at a picture position (already scaled to output pixels).
pub struct Label {
    pub x: f32,
    pub y: f32,
    pub anchor: Anchor,
    pub text: String,
    pub color: Color32,
}

type R = (f32, f32, f32, f32);

fn overlap(a: R, b: R) -> f32 {
    let w = (a.2.min(b.2) - a.0.max(b.0)).max(0.0);
    let h = (a.3.min(b.3) - a.1.max(b.1)).max(0.0);
    w * h
}

/// Puts every label at the first spot around its anchor that hides no other label
/// and no other mark; if none is free, at the spot with the least overlap.
fn place_labels(img: &mut RgbImage, labels: &[Label], px: f32) {
    let (iw, ih) = img.dimensions();
    let (iw, ih) = (iw as f32, ih as f32);
    let m = px * 0.6; // half size of the area kept free around each mark
    let mut taken: Vec<R> = labels
        .iter()
        .filter(|l| matches!(l.anchor, Anchor::Point))
        .map(|l| (l.x - m, l.y - m, l.x + m, l.y + m))
        .collect();
    for l in labels {
        let (w, h, _) = box_size(&l.text, px);
        let d = px * 0.7;
        let cands: Vec<(f32, f32)> = match l.anchor {
            Anchor::Point => vec![
                (l.x + d, l.y - h - d * 0.3),
                (l.x + d, l.y + d * 0.3),
                (l.x - w - d, l.y - h - d * 0.3),
                (l.x - w - d, l.y + d * 0.3),
                (l.x - w / 2.0, l.y - h - d),
                (l.x - w / 2.0, l.y + d),
                (l.x + d, l.y - 2.0 * h - d),
                (l.x + d, l.y + h + d),
            ],
            Anchor::Corner => vec![
                (l.x, l.y - h - 2.0),
                (l.x + 2.0, l.y + 2.0),
                (l.x, l.y - 2.0 * h - 4.0),
                (l.x + 2.0, l.y + h + 4.0),
            ],
        };
        let own = (l.x - m, l.y - m, l.x + m, l.y + m);
        let mut best = (f32::MAX, 0.0, 0.0);
        for (x, y) in cands {
            let x = x.clamp(1.0, (iw - w - 1.0).max(1.0));
            let y = y.clamp(1.0, (ih - h - 1.0).max(1.0));
            let r = (x, y, x + w, y + h);
            let cost: f32 = taken.iter().filter(|t| **t != own).map(|t| overlap(*t, r)).sum();
            if cost < best.0 {
                best = (cost, x, y);
            }
            if cost == 0.0 {
                break;
            }
        }
        let (_, x, y) = best;
        label_box(img, x, y, &l.text, px, l.color);
        taken.push((x, y, x + w, y + h));
    }
}

pub struct ColorScale<'a> {
    pub lut: &'a [Color32],
    /// lowest / highest temperature of the colour range, already in the display unit
    pub lo: f32,
    pub hi: f32,
    pub unit: &'static str,
}

pub struct Annotations<'a> {
    pub labels: Vec<Label>,
    pub scale: Option<ColorScale<'a>>,
    /// Info strip below the picture: line 1 (left, right), line 2 (left).
    pub info: Option<[String; 3]>,
}

/// Font size used for a picture enlarged `scale` times.
pub fn font_px(scale: u32) -> f32 {
    (4.0 * scale as f32 + 6.0).clamp(10.0, 30.0)
}

fn nice_step(range: f32, target: f32) -> f32 {
    let raw = (range / target).max(1e-3);
    let mag = 10f32.powf(raw.log10().floor());
    let n = raw / mag;
    let k = if n <= 1.0 {
        1.0
    } else if n <= 2.0 {
        2.0
    } else if n <= 2.5 {
        2.5
    } else if n <= 5.0 {
        5.0
    } else {
        10.0
    };
    k * mag
}

fn fmt(v: f32, d: usize) -> String {
    format!("{:.*}", d, v).replace('.', ",")
}

/// Adds labels, colour scale and info strip. The output size depends only on the
/// input size, the font size and which parts are switched on (constant for videos).
pub fn annotate(mut img: RgbImage, px: f32, ann: &Annotations) -> RgbImage {
    place_labels(&mut img, &ann.labels, px);
    let (iw, ih) = img.dimensions();
    let bg = Rgb([16, 16, 20]);

    // ---- colour scale on the right
    let side_w = if ann.scale.is_some() {
        let bar_w = (px * 0.9).round();
        // fixed width: room for e.g. "-888,8" so the size never changes
        (px * 0.6 + bar_w + px * 0.4 + text_width("-888,8", px) + px * 0.5).ceil() as u32
    } else {
        0
    };
    // ---- info strip at the bottom
    let line_h = (px * 1.35).round();
    let info_h = if ann.info.is_some() { (2.0 * line_h + px * 0.5).ceil() as u32 } else { 0 };

    let mut out = RgbImage::from_pixel(iw + side_w, ih + info_h, bg);
    image::imageops::overlay(&mut out, &img, 0, 0);

    if let Some(sc) = &ann.scale {
        let bar_w = (px * 0.9).round();
        let bx = iw as f32 + px * 0.6;
        let top = px * 1.6; // room for the unit
        let bot = ih as f32 - px * 0.7;
        let len = (bot - top).max(1.0);
        for y in top as i32..bot as i32 {
            let f = 1.0 - (y as f32 - top) / len;
            let c = sc.lut[((f * 255.0).round() as usize).min(255)];
            for x in bx as i32..(bx + bar_w) as i32 {
                out.put_pixel(x as u32, y as u32, Rgb([c.r(), c.g(), c.b()]));
            }
        }
        let tx = bx + bar_w + px * 0.4;
        let white = Color32::from_gray(235);
        let grey = Color32::from_gray(170);
        draw_text(&mut out, bx, px * 0.2, sc.unit, px, white);
        let span = (sc.hi - sc.lo).max(0.01);
        let y_of = |v: f32| bot - (v - sc.lo) / span * len;
        let tick = |out: &mut RgbImage, y: f32, text: &str, col: Color32| {
            fill_rect(out, bx + bar_w, y - 0.5, bx + bar_w + px * 0.3, y + 0.5 + (px / 16.0).floor(), col, 1.0);
            draw_text(out, tx, y - px * 0.62, text, px, col);
        };
        let d = if span < 8.0 { 1 } else { 0 };
        let step = nice_step(span, ((len / (px * 2.2)).floor()).clamp(2.0, 8.0));
        let mut v = (sc.lo / step).ceil() * step;
        while v < sc.hi {
            let y = y_of(v);
            if (y - top).abs() > px * 1.2 && (y - bot).abs() > px * 1.2 {
                tick(&mut out, y, &fmt(v, d.max(if step < 1.0 { 1 } else { 0 })), grey);
            }
            v += step;
        }
        tick(&mut out, top, &fmt(sc.hi, 1), white);
        tick(&mut out, bot, &fmt(sc.lo, 1), white);
    }

    if let Some([l1, r1, l2]) = &ann.info {
        let y0 = ih as f32 + px * 0.25;
        let x0 = px * 0.5;
        let full = (iw + side_w) as f32;
        draw_text(&mut out, x0, y0, l1, px, Color32::WHITE);
        let rw = text_width(r1, px);
        if x0 + text_width(l1, px) + px + rw < full - x0 {
            draw_text(&mut out, full - x0 - rw, y0, r1, px, Color32::from_gray(190));
        }
        draw_text(&mut out, x0, y0 + line_h, l2, px, Color32::from_gray(210));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_is_stable() {
        let lut = vec![Color32::RED; 256];
        let mk = |lo: f32, hi: f32, t: &str| {
            let ann = Annotations {
                labels: vec![Label { x: 10.0, y: 10.0, anchor: Anchor::Point, text: t.into(), color: Color32::WHITE }],
                scale: Some(ColorScale { lut: &lut, lo, hi, unit: "°C" }),
                info: Some([t.into(), "ε 0,95".into(), t.into()]),
            };
            annotate(RgbImage::new(512, 384), font_px(2), &ann).dimensions()
        };
        assert_eq!(mk(20.0, 30.0, "a"), mk(-20.0, 400.0, "ein langer Text mit 123,4 °C"));
        let d = mk(20.0, 30.0, "a");
        assert!(d.0 > 512 && d.1 > 384);
    }
}
