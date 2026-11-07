//! Frame sources: the USB thermal camera (via Windows Media Foundation) and the demo scene.
//!
//! TC001-class cameras show up as a normal USB webcam. In 256x384 YUYV mode the upper half
//! of each frame is a picture and the lower half holds the raw temperatures as 16-bit
//! little-endian values (Kelvin * 64). This is public community knowledge about the device.

use crate::sim::DemoScene;
use crate::thermal::ThermalFrame;
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub index: u32,
    pub name: String,
    pub thermal: bool,
    /// Bus and address when the camera is reached directly over USB (libusbK/WinUSB driver).
    pub usb: Option<(u8, u8)>,
}

pub enum CamCmd {
    Probe,
    Open(DeviceInfo),
    Close,
    Shutdown,
}

pub enum CamMsg {
    Devices(Vec<DeviceInfo>),
    Opened(String),
    Frame(ThermalFrame),
    Closed,
    Error(String),
}

/// One long-lived thread owns the camera (Media Foundation must stay on one thread).
pub struct CameraWorker {
    tx: Sender<CamCmd>,
    pub rx: Receiver<CamMsg>,
    handle: Option<JoinHandle<()>>,
}

impl CameraWorker {
    pub fn start(ctx: egui::Context) -> Self {
        let (tx, cmd_rx) = channel::<CamCmd>();
        let (msg_tx, rx) = channel::<CamMsg>();
        let handle = std::thread::Builder::new()
            .name("kamera".into())
            .spawn(move || camera_thread(cmd_rx, msg_tx, ctx))
            .ok();
        Self { tx, rx, handle }
    }

    pub fn send(&self, cmd: CamCmd) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for CameraWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(CamCmd::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Extract the thermal half of a raw YUYV frame.
pub fn parse_usb_frame(buf: &[u8], w: usize, h: usize) -> Option<ThermalFrame> {
    if w == 0 || h < 2 || buf.len() < w * h * 2 {
        return None;
    }
    let th = h / 2;
    let off = th * w * 2;
    let mut raw = Vec::with_capacity(w * th);
    for i in 0..w * th {
        raw.push(u16::from_le_bytes([buf[off + 2 * i], buf[off + 2 * i + 1]]));
    }
    Some(ThermalFrame { w, h: th, raw })
}

#[cfg(windows)]
mod backend {
    use super::*;
    use nokhwa::pixel_format::RgbFormat;
    use nokhwa::utils::{
        ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution,
    };
    use nokhwa::Camera;

    pub struct Cam {
        pub cam: Camera,
        pub w: usize,
        pub h: usize,
    }

    pub fn list() -> Result<Vec<(u32, String)>, String> {
        let infos = nokhwa::query(ApiBackend::MediaFoundation).map_err(|e| e.to_string())?;
        Ok(infos
            .iter()
            .enumerate()
            .map(|(i, info)| {
                let idx = match info.index() {
                    CameraIndex::Index(n) => *n,
                    _ => i as u32,
                };
                (idx, info.human_name())
            })
            .collect())
    }

    pub fn open(index: u32, require_thermal: bool) -> Result<Cam, String> {
        let fmt = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(CameraFormat::new(
            Resolution::new(256, 384),
            FrameFormat::YUYV,
            25,
        )));
        let cam = Camera::with_backend(CameraIndex::Index(index), fmt, ApiBackend::MediaFoundation)
            .map_err(|e| e.to_string())?;
        let res = cam.resolution();
        let (w, h) = (res.width() as usize, res.height() as usize);
        let thermal = w == 256 && (h == 384 || h == 392) && cam.frame_format() == FrameFormat::YUYV;
        if require_thermal && !thermal {
            return Err(format!("kein Wärmebild-Format ({}×{})", w, h));
        }
        Ok(Cam { cam, w, h })
    }

    pub fn start(c: &mut Cam) -> Result<(), String> {
        c.cam.open_stream().map_err(|e| e.to_string())
    }

    pub fn grab(c: &mut Cam) -> Result<ThermalFrame, String> {
        let buf = c.cam.frame_raw().map_err(|e| e.to_string())?;
        // 256x384: upper 192 rows picture, lower 192 rows temperatures.
        let h = if c.h == 392 { 384 } else { c.h };
        parse_usb_frame(&buf, c.w, h).ok_or_else(|| "Bild unvollständig".to_string())
    }

    pub fn stop(c: &mut Cam) {
        let _ = c.cam.stop_stream();
    }
}

#[cfg(not(windows))]
mod backend {
    use super::*;
    pub struct Cam;
    pub fn list() -> Result<Vec<(u32, String)>, String> {
        Ok(vec![])
    }
    pub fn open(_i: u32, _r: bool) -> Result<Cam, String> {
        Err("nur unter Windows".into())
    }
    pub fn start(_c: &mut Cam) -> Result<(), String> {
        Ok(())
    }
    pub fn grab(_c: &mut Cam) -> Result<ThermalFrame, String> {
        Err("nur unter Windows".into())
    }
    pub fn stop(_c: &mut Cam) {}
}

/// An open camera: either a Windows webcam (Media Foundation) or a direct USB connection.
enum AnyCam {
    Mf(backend::Cam),
    Usb(crate::usbcam::UsbCam),
}

impl AnyCam {
    fn size(&self) -> (usize, usize) {
        match self {
            AnyCam::Mf(c) => (c.w, c.h),
            AnyCam::Usb(c) => (c.w, c.h),
        }
    }
    fn grab(&mut self) -> Result<ThermalFrame, String> {
        match self {
            AnyCam::Mf(c) => backend::grab(c),
            AnyCam::Usb(c) => {
                let buf = c.grab()?;
                let h = if c.h == 392 { 384 } else { c.h };
                parse_usb_frame(&buf, c.w, h).ok_or_else(|| "Bild unvollständig".to_string())
            }
        }
    }
    fn stop(&mut self) {
        match self {
            AnyCam::Mf(c) => backend::stop(c),
            AnyCam::Usb(c) => c.stop(),
        }
    }
}

fn open_any(d: &DeviceInfo) -> Result<AnyCam, String> {
    match d.usb {
        Some((bus, addr)) => crate::usbcam::UsbCam::open(bus, addr).map(AnyCam::Usb),
        None => backend::open(d.index, false).and_then(|mut c| backend::start(&mut c).map(|_| AnyCam::Mf(c))),
    }
}

fn probe_devices() -> (Vec<DeviceInfo>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    // 1) cameras with the vendor (libusbK) or WinUSB driver: invisible to Windows' webcam API
    match crate::usbcam::list() {
        Ok(list) => {
            for (i, c) in list.into_iter().enumerate() {
                out.push(DeviceInfo { index: 1000 + i as u32, name: c.name.clone(), thermal: c.is_thermal(), usb: Some((c.bus, c.address)) });
            }
        }
        Err(e) => errors.push(format!("USB-Suche: {e}")),
    }
    // 2) normal webcams (Windows camera driver)
    match backend::list() {
        Ok(list) => {
            for (index, name) in list {
                let thermal = backend::open(index, true).is_ok();
                out.push(DeviceInfo { index, name, thermal, usb: None });
            }
        }
        Err(e) => errors.push(format!("Webcam-Suche: {e}")),
    }
    (out, errors)
}

fn camera_thread(rx: Receiver<CamCmd>, tx: Sender<CamMsg>, ctx: egui::Context) {
    let mut cam: Option<AnyCam> = None;
    let mut errors = 0u32;
    loop {
        let cmd = if cam.is_some() {
            match rx.try_recv() {
                Ok(c) => Some(c),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            }
        };
        match cmd {
            Some(CamCmd::Probe) => {
                if cam.is_some() {
                    continue;
                }
                let (list, errors) = probe_devices();
                for e in errors {
                    let _ = tx.send(CamMsg::Error(e));
                }
                let _ = tx.send(CamMsg::Devices(list));
                ctx.request_repaint();
            }
            Some(CamCmd::Open(d)) => {
                if let Some(mut c) = cam.take() {
                    c.stop();
                }
                match open_any(&d) {
                    Ok(c) => {
                        let (w, h) = c.size();
                        let _ = (w, h);
                        let _ = tx.send(CamMsg::Opened(d.name.clone()));
                        cam = Some(c);
                        errors = 0;
                    }
                    Err(e) => {
                        let _ = tx.send(CamMsg::Error(format!("Kamera „{}“ ließ sich nicht öffnen: {e}", d.name)));
                    }
                }
                ctx.request_repaint();
            }
            Some(CamCmd::Close) => {
                if let Some(mut c) = cam.take() {
                    c.stop();
                }
                let _ = tx.send(CamMsg::Closed);
                ctx.request_repaint();
            }
            Some(CamCmd::Shutdown) => break,
            None => {}
        }
        if let Some(c) = cam.as_mut() {
            match c.grab() {
                Ok(f) => {
                    errors = 0;
                    if tx.send(CamMsg::Frame(f)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
                Err(e) => {
                    errors += 1;
                    if errors > 40 || e == "Kamera getrennt" {
                        c.stop();
                        cam = None;
                        let _ = tx.send(CamMsg::Error(format!("Verbindung zur Kamera verloren: {e}")));
                        let _ = tx.send(CamMsg::Closed);
                        ctx.request_repaint();
                    } else {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            }
        }
    }
    if let Some(mut c) = cam.take() {
        c.stop();
    }
}

/// Demo source: renders the demo scene at 25 frames per second on its own thread.
pub struct DemoSource {
    pub rx: Receiver<ThermalFrame>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl DemoSource {
    pub fn start(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::Builder::new()
            .name("demo".into())
            .spawn(move || {
                let mut scene = DemoScene::new();
                let t0 = Instant::now();
                while !stop2.load(Ordering::Relaxed) {
                    let f = scene.frame(t0.elapsed().as_secs_f64() + 90.0);
                    if tx.send(f).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                    std::thread::sleep(Duration::from_millis(40));
                }
            })
            .ok();
        Self { rx, stop, handle }
    }
}

impl Drop for DemoSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
