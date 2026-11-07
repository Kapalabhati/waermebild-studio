//! Demo scene used when no thermal camera is connected (and for screenshots).

use crate::thermal::{c_to_raw, ThermalFrame, SENSOR_H, SENSOR_W};

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Weight 0..1 for a rectangle with soft edges.
fn soft_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, soft: f32) -> f32 {
    smooth(x0 - soft, x0 + soft, x)
        * (1.0 - smooth(x1 - soft, x1 + soft, x))
        * smooth(y0 - soft, y0 + soft, y)
        * (1.0 - smooth(y1 - soft, y1 + soft, y))
}

fn blob(x: f32, y: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> f32 {
    let dx = (x - cx) / rx;
    let dy = (y - cy) / ry;
    (-(dx * dx + dy * dy)).exp()
}

pub struct DemoScene {
    seed: u32,
}

impl DemoScene {
    pub fn new() -> Self {
        Self { seed: 0x1234_5678 }
    }

    fn noise(&mut self) -> f32 {
        // xorshift32
        let mut x = self.seed;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.seed = x;
        (x as f32 / u32::MAX as f32) - 0.5
    }

    /// Temperature scene at time `t` seconds.
    pub fn frame(&mut self, t: f64) -> ThermalFrame {
        let t = t as f32;
        let (w, h) = (SENSOR_W, SENSOR_H);
        let mut raw = vec![0u16; w * h];
        let mug_t = 57.0 + 4.0 * (t * 0.11).sin() - (t * 0.01).min(6.0);
        let can_t = 6.0 + (t * 0.02).min(5.0) + 0.4 * (t * 0.07).sin();
        let rad_t = 46.0 + 2.5 * (t * 0.05).sin();
        let hand_x = 88.0 + 18.0 * (t * 0.31).sin();
        let hand_y = 70.0 + 9.0 * (t * 0.23).cos();
        for y in 0..h {
            for x in 0..w {
                let fx = x as f32;
                let fy = y as f32;
                // wall with warm air at the top
                let mut c = 20.6 + (1.0 - fy / h as f32) * 2.0 + 0.4 * ((fx * 0.05).sin() * (fy * 0.07).cos());
                // window (cold) top-left
                let win = soft_rect(fx, fy, 14.0, 12.0, 72.0, 78.0, 2.5);
                c += win * (12.5 - c);
                let frame_w = soft_rect(fx, fy, 10.0, 8.0, 76.0, 82.0, 1.5) * (1.0 - win);
                c += frame_w * (17.5 - c);
                // radiator with fins below the window
                let rad = soft_rect(fx, fy, 10.0, 104.0, 78.0, 150.0, 2.0);
                let fins = 0.75 + 0.25 * ((fx * 0.9).sin()).abs();
                let rad_grad = 1.0 - (fy - 104.0).max(0.0) / 120.0;
                c += rad * (rad_t * fins * rad_grad + 4.0 - c);
                // warm plume above radiator
                c += 3.5 * blob(fx, fy, 44.0, 96.0, 40.0, 14.0);
                // table surface
                let table = smooth(150.0, 154.0, fy);
                c += table * (19.2 + 0.6 * (fx * 0.02).sin() - c);
                // mug body
                let mug = soft_rect(fx, fy, 130.0, 86.0, 170.0, 152.0, 1.6);
                let mug_grad = 1.0 - 0.18 * ((fy - 86.0) / 66.0).clamp(0.0, 1.0);
                c += mug * (mug_t * mug_grad - c);
                // mug handle ring
                let hd = ((fx - 172.0).powi(2) / 1.0 + (fy - 118.0).powi(2) / 2.2).sqrt();
                let handle = (1.0 - smooth(1.5, 3.5, (hd - 11.0).abs())) * smooth(169.0, 172.0, fx);
                c += handle * (mug_t * 0.78 - c);
                // steam rising from the mug
                for k in 0..3 {
                    let phase = (t * 0.6 + k as f32 * 1.7) % 5.0;
                    let sy = 82.0 - phase * 14.0;
                    let sx = 150.0 + 6.0 * (t * 0.8 + k as f32).sin();
                    c += (6.0 - phase) * 0.9 * blob(fx, fy, sx, sy, 7.0 + phase * 2.0, 6.0 + phase);
                }
                // cold drink can
                let can = soft_rect(fx, fy, 204.0, 96.0, 232.0, 152.0, 1.5);
                c += can * (can_t + 1.5 * ((fy - 96.0) / 56.0) - c);
                // condensation ring on the table
                c -= 2.0 * blob(fx, fy, 218.0, 154.0, 18.0, 3.0) * table;
                // hand moving in front of the wall
                let hand = blob(fx, fy, hand_x, hand_y, 15.0, 19.0).powf(0.5);
                let fingers = blob(fx, fy, hand_x, hand_y - 24.0, 13.0, 9.0).powf(0.7);
                let hw = (hand.max(fingers) * 1.6).min(1.0) * (1.0 - win * 0.0);
                c += hw * (33.6 - 1.5 * fingers - c);
                // sensor noise
                c += self.noise() * 0.22;
                raw[y * w + x] = c_to_raw(c);
            }
        }
        ThermalFrame { w, h, raw }
    }
}
