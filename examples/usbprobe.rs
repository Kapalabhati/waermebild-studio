//! Test program: open the first direct-USB thermal camera and grab a few frames.
#[path = "../src/usbcam.rs"]
mod usbcam;

fn main() {
    let list = usbcam::list().unwrap();
    for c in &list {
        println!("{} thermal={} frames={:?}", c.name, c.is_thermal(), c.frames);
    }
    let Some(c) = list.iter().find(|c| c.is_thermal()) else { println!("keine"); return };
    let mut cam = match usbcam::UsbCam::open(c.bus, c.address) {
        Ok(cam) => cam,
        Err(e) => { println!("open: {e}"); return; }
    };
    println!("open {}x{} alt {} pkt {}", cam.w, cam.h, cam.alt, cam.packet_size);
    let t0 = std::time::Instant::now();
    for i in 0..50 {
        match cam.grab() {
            Ok(f) => {
                if i % 10 == 0 || i == 49 {
                    let w = cam.w; let th = cam.h / 2; let off = th * w * 2;
                    let raw: Vec<u16> = (0..w * th).map(|k| u16::from_le_bytes([f[off + 2 * k], f[off + 2 * k + 1]])).collect();
                    let min = *raw.iter().min().unwrap(); let max = *raw.iter().max().unwrap();
                    let c = raw[th / 2 * w + w / 2];
                    let t = |v: u16| v as f32 / 64.0 - 273.15;
                    println!("frame {i} len {} min {:.1} max {:.1} centre {:.1} °C  t={:?}", f.len(), t(min), t(max), t(c), t0.elapsed());
                }
            }
            Err(e) => println!("grab {i}: {e}"),
        }
    }
    println!("stats {:?}", cam.stats());
    cam.stop();
}
