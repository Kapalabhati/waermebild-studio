//! Direct USB access to UVC thermal cameras that are bound to the libusbK / WinUSB driver.
//!
//! Some thermal cameras are installed with a vendor driver (libusbK) instead of the Windows
//! webcam driver, so Media Foundation never lists them. This module talks to them with libusb:
//! it reads the video-streaming descriptors, negotiates the format with the standard UVC
//! probe/commit requests and receives frames over isochronous transfers.
//!
//! The module is self-contained (no `crate::` imports) so a small test program can reuse it.

#![allow(dead_code)]

use libusb1_sys as ffi;
use libusb1_sys::constants::*;
use rusb::{Device, DeviceHandle, GlobalContext, UsbContext};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::time::{Duration, Instant};

const VS_CLASS: u8 = 0x0e;
const VS_SUBCLASS_STREAMING: u8 = 0x02;
const TIMEOUT: Duration = Duration::from_millis(1000);
const NUM_TRANSFERS: usize = 8;
const PACKETS_PER_TRANSFER: usize = 32;

#[derive(Clone, Debug)]
pub struct UvcFrameDesc {
    pub format_index: u8,
    pub frame_index: u8,
    pub width: u16,
    pub height: u16,
    pub interval: u32,
    pub buffer_size: u32,
}

#[derive(Clone, Debug)]
pub struct UsbCamInfo {
    pub bus: u8,
    pub address: u8,
    pub vid: u16,
    pub pid: u16,
    pub name: String,
    pub iface: u8,
    pub frames: Vec<UvcFrameDesc>,
}

impl UsbCamInfo {
    /// Frame size that carries the thermal data: 256 wide, picture + temperature halves.
    pub fn thermal_frame(&self) -> Option<&UvcFrameDesc> {
        self.frames
            .iter()
            .find(|f| f.width == 256 && (f.height == 384 || f.height == 392))
            .or_else(|| self.frames.iter().max_by_key(|f| f.width as u32 * f.height as u32))
    }
    pub fn is_thermal(&self) -> bool {
        self.frames.iter().any(|f| f.width == 256 && (f.height == 384 || f.height == 392))
    }
}

/// Parse the class-specific video-streaming descriptors (uncompressed formats only).
fn parse_vs_descriptors(extra: &[u8]) -> Vec<UvcFrameDesc> {
    let mut out = Vec::new();
    let mut fmt_index = 0u8;
    let mut fmt_uncompressed = false;
    let mut i = 0usize;
    while i + 3 <= extra.len() {
        let len = extra[i] as usize;
        if len < 3 || i + len > extra.len() {
            break;
        }
        let d = &extra[i..i + len];
        if d[1] == 0x24 {
            match d[2] {
                // VS_FORMAT_UNCOMPRESSED
                0x04 if len >= 4 => {
                    fmt_index = d[3];
                    fmt_uncompressed = true;
                }
                // other formats (MJPEG 0x06, frame based 0x10 ...)
                0x06 | 0x10 => fmt_uncompressed = false,
                // VS_FRAME_UNCOMPRESSED
                0x05 if fmt_uncompressed && len >= 26 => {
                    out.push(UvcFrameDesc {
                        format_index: fmt_index,
                        frame_index: d[3],
                        width: u16::from_le_bytes([d[5], d[6]]),
                        height: u16::from_le_bytes([d[7], d[8]]),
                        buffer_size: u32::from_le_bytes([d[17], d[18], d[19], d[20]]),
                        interval: u32::from_le_bytes([d[21], d[22], d[23], d[24]]),
                    });
                }
                _ => {}
            }
        }
        i += len;
    }
    out
}

fn describe(dev: &Device<GlobalContext>) -> Option<UsbCamInfo> {
    let dd = dev.device_descriptor().ok()?;
    let cfg = dev.active_config_descriptor().or_else(|_| dev.config_descriptor(0)).ok()?;
    for itf in cfg.interfaces() {
        for alt in itf.descriptors() {
            if alt.class_code() == VS_CLASS && alt.sub_class_code() == VS_SUBCLASS_STREAMING && alt.setting_number() == 0 {
                let frames = parse_vs_descriptors(alt.extra());
                if frames.is_empty() {
                    continue;
                }
                return Some(UsbCamInfo {
                    bus: dev.bus_number(),
                    address: dev.address(),
                    vid: dd.vendor_id(),
                    pid: dd.product_id(),
                    name: String::new(),
                    iface: alt.interface_number(),
                    frames,
                });
            }
        }
    }
    None
}

/// All USB video devices that libusb can open (i.e. not owned by the Windows webcam driver).
pub fn list() -> Result<Vec<UsbCamInfo>, String> {
    let devices = rusb::devices().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for dev in devices.iter() {
        let Some(mut info) = describe(&dev) else { continue };
        // Only devices we can actually open (libusbK / WinUSB / libusb0 driver).
        let Ok(h) = dev.open() else { continue };
        let product = dev
            .device_descriptor()
            .ok()
            .and_then(|dd| h.read_product_string_ascii(&dd).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "USB-Kamera".to_string());
        info.name = if info.is_thermal() {
            "Wärmebildkamera (USB direkt)".to_string()
        } else {
            format!("{product} (USB direkt)")
        };
        out.push(info);
    }
    Ok(out)
}

/// Shared between the camera and the libusb callbacks. Lives in a Box so its address is stable.
struct StreamState {
    buf: Vec<u8>,
    fid: Option<u8>,
    frame_size: usize,
    tx: SyncSender<Vec<u8>>,
    in_flight: usize,
    stopping: bool,
    lost: bool,
    good: u64,
    bad: u64,
}

impl StreamState {
    fn finish(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        if self.buf.len() >= self.frame_size {
            let mut frame = std::mem::replace(&mut self.buf, Vec::with_capacity(self.frame_size + 4096));
            frame.truncate(self.frame_size);
            let _ = self.tx.try_send(frame);
            self.good += 1;
        } else {
            self.bad += 1;
            self.buf.clear();
        }
    }

    fn packet(&mut self, data: &[u8]) {
        if data.len() < 2 {
            return;
        }
        let hl = data[0] as usize;
        if hl < 2 || hl > data.len() {
            return;
        }
        let flags = data[1];
        let fid = flags & 0x01;
        if let Some(prev) = self.fid {
            if prev != fid {
                self.finish();
            }
        }
        self.fid = Some(fid);
        if flags & 0x40 != 0 {
            // error bit: drop this frame
            self.buf.clear();
            self.bad += 1;
            return;
        }
        if self.buf.len() < self.frame_size + 65536 {
            self.buf.extend_from_slice(&data[hl..]);
        }
        if flags & 0x02 != 0 {
            self.finish();
        }
    }
}

extern "system" fn iso_callback(t: *mut ffi::libusb_transfer) {
    unsafe {
        let st = &mut *((*t).user_data as *mut StreamState);
        let status = (*t).status;
        if status == LIBUSB_TRANSFER_COMPLETED {
            let n = (*t).num_iso_packets.max(0) as usize;
            let descs = (*t).iso_packet_desc.as_ptr();
            for i in 0..n {
                let pd = &*descs.add(i);
                if pd.status != LIBUSB_TRANSFER_COMPLETED || pd.actual_length == 0 {
                    continue;
                }
                let p = ffi::libusb_get_iso_packet_buffer_simple(t, i as u32);
                if p.is_null() {
                    continue;
                }
                let data = std::slice::from_raw_parts(p, pd.actual_length as usize);
                st.packet(data);
            }
        }
        if status == LIBUSB_TRANSFER_NO_DEVICE {
            st.lost = true;
        }
        if st.stopping || status == LIBUSB_TRANSFER_CANCELLED || status == LIBUSB_TRANSFER_NO_DEVICE {
            st.in_flight = st.in_flight.saturating_sub(1);
            return;
        }
        if ffi::libusb_submit_transfer(t) != 0 {
            st.in_flight = st.in_flight.saturating_sub(1);
            st.lost = true;
        }
    }
}

pub struct UsbCam {
    handle: DeviceHandle<GlobalContext>,
    iface: u8,
    pub w: usize,
    pub h: usize,
    pub alt: u8,
    pub packet_size: usize,
    state: Box<StreamState>,
    rx: Receiver<Vec<u8>>,
    transfers: Vec<*mut ffi::libusb_transfer>,
    buffers: Vec<Vec<u8>>,
    streaming: bool,
}

fn effective_packet_size(mps: u16) -> usize {
    let base = (mps & 0x7ff) as usize;
    let mult = (((mps >> 11) & 0x3) + 1) as usize;
    base * mult
}

impl UsbCam {
    /// Open the camera at the given bus/address and negotiate the thermal frame format.
    pub fn open(bus: u8, address: u8) -> Result<Self, String> {
        let devices = rusb::devices().map_err(|e| e.to_string())?;
        let dev = devices
            .iter()
            .find(|d| d.bus_number() == bus && d.address() == address)
            .ok_or("Kamera nicht mehr gefunden (abgesteckt?)")?;
        let info = describe(&dev).ok_or("kein Videoanschluss gefunden")?;
        let frame = info.thermal_frame().cloned().ok_or("kein passendes Bildformat")?;
        let handle = dev.open().map_err(|e| format!("USB öffnen: {e} – läuft noch ein anderes Kameraprogramm?"))?;
        let iface = info.iface;
        // The control interface is optional for streaming; claim it if we can.
        let _ = handle.claim_interface(0);
        handle
            .claim_interface(iface)
            .map_err(|e| format!("USB-Schnittstelle belegt: {e} – läuft noch ein anderes Kameraprogramm?"))?;
        let _ = handle.set_alternate_setting(iface, 0);

        // UVC probe / commit
        let mut probe = [0u8; 26];
        probe[0..2].copy_from_slice(&1u16.to_le_bytes()); // bmHint: keep frame interval
        probe[2] = frame.format_index;
        probe[3] = frame.frame_index;
        probe[4..8].copy_from_slice(&frame.interval.to_le_bytes());
        let w_index = iface as u16;
        handle
            .write_control(0x21, 0x01, 0x0100, w_index, &probe, TIMEOUT)
            .map_err(|e| format!("Formatabfrage (probe): {e}"))?;
        let mut cur = [0u8; 26];
        let n = handle
            .read_control(0xA1, 0x81, 0x0100, w_index, &mut cur, TIMEOUT)
            .map_err(|e| format!("Formatabfrage (get): {e}"))?;
        if n < 26 {
            cur = probe;
        }
        handle
            .write_control(0x21, 0x01, 0x0200, w_index, &cur, TIMEOUT)
            .map_err(|e| format!("Format bestätigen (commit): {e}"))?;
        let mut frame_size = u32::from_le_bytes([cur[18], cur[19], cur[20], cur[21]]) as usize;
        let expected = frame.width as usize * frame.height as usize * 2;
        if frame_size == 0 || frame_size > expected * 2 {
            frame_size = expected;
        }
        let max_payload = u32::from_le_bytes([cur[22], cur[23], cur[24], cur[25]]) as usize;

        // pick the smallest alternate setting that carries max_payload per (micro)frame
        let cfg = dev.active_config_descriptor().or_else(|_| dev.config_descriptor(0)).map_err(|e| e.to_string())?;
        let mut alts: Vec<(u8, u8, usize)> = Vec::new();
        for itf in cfg.interfaces() {
            for alt in itf.descriptors() {
                if alt.interface_number() != iface || alt.setting_number() == 0 {
                    continue;
                }
                for ep in alt.endpoint_descriptors() {
                    if ep.transfer_type() == rusb::TransferType::Isochronous && ep.direction() == rusb::Direction::In {
                        alts.push((alt.setting_number(), ep.address(), effective_packet_size(ep.max_packet_size())));
                    }
                }
            }
        }
        if alts.is_empty() {
            return Err("Kamera bietet keinen isochronen Bildkanal".into());
        }
        alts.sort_by_key(|a| a.2);
        let (alt, ep, psize) = alts
            .iter()
            .copied()
            .find(|a| max_payload > 0 && a.2 >= max_payload)
            .unwrap_or(*alts.last().unwrap());
        handle
            .set_alternate_setting(iface, alt)
            .map_err(|e| format!("Bildkanal einschalten: {e}"))?;

        let (tx, rx) = sync_channel(3);
        let state = Box::new(StreamState {
            buf: Vec::with_capacity(frame_size + 4096),
            fid: None,
            frame_size,
            tx,
            in_flight: 0,
            stopping: false,
            lost: false,
            good: 0,
            bad: 0,
        });
        let mut cam = UsbCam {
            handle,
            iface,
            w: frame.width as usize,
            h: frame.height as usize,
            alt,
            packet_size: psize,
            state,
            rx,
            transfers: Vec::new(),
            buffers: Vec::new(),
            streaming: false,
        };
        cam.start_transfers(ep)?;
        Ok(cam)
    }

    fn start_transfers(&mut self, ep: u8) -> Result<(), String> {
        let len = self.packet_size * PACKETS_PER_TRANSFER;
        let user = &mut *self.state as *mut StreamState as *mut std::ffi::c_void;
        for _ in 0..NUM_TRANSFERS {
            let mut buf = vec![0u8; len];
            unsafe {
                let t = ffi::libusb_alloc_transfer(PACKETS_PER_TRANSFER as i32);
                if t.is_null() {
                    return Err("Speicher für USB-Übertragung fehlt".into());
                }
                ffi::libusb_fill_iso_transfer(
                    t,
                    self.handle.as_raw(),
                    ep,
                    buf.as_mut_ptr(),
                    len as i32,
                    PACKETS_PER_TRANSFER as i32,
                    iso_callback,
                    user,
                    1000,
                );
                ffi::libusb_set_iso_packet_lengths(t, self.packet_size as u32);
                self.transfers.push(t);
                self.buffers.push(buf);
                let r = ffi::libusb_submit_transfer(t);
                if r != 0 {
                    let msg = rusb_error_text(r);
                    self.stop();
                    return Err(format!("Bildübertragung starten: {msg}"));
                }
                self.state.in_flight += 1;
            }
        }
        self.streaming = true;
        Ok(())
    }

    fn pump(&mut self, wait: Duration) {
        let tv = libc::timeval { tv_sec: 0, tv_usec: wait.as_micros().min(999_999) as _ };
        unsafe {
            ffi::libusb_handle_events_timeout(GlobalContext::default().as_raw(), &tv);
        }
    }

    /// Wait for the next complete frame (raw YUYV bytes, `w * h * 2`).
    pub fn grab(&mut self) -> Result<Vec<u8>, String> {
        let t0 = Instant::now();
        loop {
            if let Ok(f) = self.rx.try_recv() {
                // always deliver the newest frame
                let mut f = f;
                while let Ok(n) = self.rx.try_recv() {
                    f = n;
                }
                return Ok(f);
            }
            if self.state.lost {
                return Err("Kamera getrennt".into());
            }
            if !self.streaming || self.state.in_flight == 0 {
                return Err("Bildübertragung steht".into());
            }
            if t0.elapsed() > Duration::from_millis(1500) {
                return Err(format!(
                    "keine Bilder ({} gut, {} unvollständig)",
                    self.state.good, self.state.bad
                ));
            }
            self.pump(Duration::from_millis(50));
        }
    }

    pub fn stats(&self) -> (u64, u64) {
        (self.state.good, self.state.bad)
    }

    pub fn stop(&mut self) {
        self.state.stopping = true;
        for &t in &self.transfers {
            unsafe {
                ffi::libusb_cancel_transfer(t);
            }
        }
        let t0 = Instant::now();
        while self.state.in_flight > 0 && t0.elapsed() < Duration::from_secs(2) {
            self.pump(Duration::from_millis(50));
        }
        if self.state.in_flight == 0 {
            for &t in &self.transfers {
                unsafe { ffi::libusb_free_transfer(t) };
            }
            self.transfers.clear();
            self.buffers.clear();
        } else {
            // Transfers still owned by the driver: keep the memory alive rather than free it.
            std::mem::forget(std::mem::take(&mut self.buffers));
            self.transfers.clear();
        }
        if self.streaming {
            let _ = self.handle.set_alternate_setting(self.iface, 0);
            let _ = self.handle.release_interface(self.iface);
            let _ = self.handle.release_interface(0);
        }
        self.streaming = false;
    }
}

impl Drop for UsbCam {
    fn drop(&mut self) {
        if self.streaming || !self.transfers.is_empty() {
            self.stop();
        }
        if self.state.in_flight > 0 {
            // callbacks may still reference the state: leak it instead of freeing
            let dummy = Box::new(StreamState {
                buf: Vec::new(),
                fid: None,
                frame_size: 0,
                tx: sync_channel(1).0,
                in_flight: 0,
                stopping: true,
                lost: false,
                good: 0,
                bad: 0,
            });
            std::mem::forget(std::mem::replace(&mut self.state, dummy));
        }
    }
}

fn rusb_error_text(code: i32) -> String {
    match code {
        LIBUSB_ERROR_IO => "Ein-/Ausgabefehler".into(),
        LIBUSB_ERROR_ACCESS => "kein Zugriff".into(),
        LIBUSB_ERROR_NO_DEVICE => "Gerät getrennt".into(),
        LIBUSB_ERROR_BUSY => "Gerät belegt".into(),
        LIBUSB_ERROR_NOT_SUPPORTED => "nicht unterstützt".into(),
        _ => format!("Fehler {code}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frame_descriptors() {
        let extra: Vec<u8> = [
            vec![0x0e, 0x24, 0x01, 0x01, 0x79, 0x00, 0x81, 0x00, 0x03, 0x02, 0x01, 0x00, 0x01, 0x00],
            vec![
                0x1b, 0x24, 0x04, 0x01, 0x02, 0x59, 0x55, 0x59, 0x32, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71,
                0x10, 0x01, 0, 0, 0, 0,
            ],
            vec![
                0x1e, 0x24, 0x05, 0x02, 0x00, 0x00, 0x01, 0x80, 0x01, 0, 0, 0x58, 0x02, 0, 0, 0x58, 0x02, 0, 0, 0x03, 0,
                0x80, 0x1a, 0x06, 0, 0x01, 0x80, 0x1a, 0x06, 0,
            ],
        ]
        .concat();
        let f = parse_vs_descriptors(&extra);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].width, f[0].height, f[0].frame_index, f[0].interval), (256, 384, 2, 400000));
        assert_eq!(f[0].buffer_size, 196608);
    }

    #[test]
    fn packet_size_with_extra_transactions() {
        assert_eq!(effective_packet_size(5120), 3072);
        assert_eq!(effective_packet_size(512), 512);
    }

    #[test]
    fn assembles_frames_by_fid() {
        let (tx, rx) = sync_channel(4);
        let mut st = StreamState { buf: vec![], fid: None, frame_size: 4, tx, in_flight: 0, stopping: false, lost: false, good: 0, bad: 0 };
        st.packet(&[2, 0x00, 1, 2]);
        st.packet(&[2, 0x02, 3, 4]); // EOF
        st.packet(&[2, 0x01, 5, 6]);
        st.packet(&[2, 0x00, 7, 8, 9, 10]); // FID toggled: previous incomplete
        assert_eq!(rx.try_recv().unwrap(), vec![1, 2, 3, 4]);
        assert!(rx.try_recv().is_err());
        assert_eq!(st.bad, 1);
    }
}
