//! CAN output of the measured values via a PEAK PCAN-USB interface (PCAN-Basic).
//!
//! Layout: signed 16 bit values, factor 0.1 °C, Intel byte order, four values per
//! 8-byte message, IDs counting up from a base ID. 0x8000 (−32768) means "no value".
//! `PCANBasic.dll` is loaded only when sending starts, so the program runs without it.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub const NO_VALUE: i16 = i16::MIN;
pub const FACTOR: f32 = 0.1;

/// One value on the bus.
#[derive(Clone, Debug, PartialEq)]
pub struct Signal {
    pub name: String,
    pub comment: String,
}

/// One CAN message: ID and up to four signals.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: u32,
    pub name: String,
    pub signals: Vec<Signal>,
}

/// Distributes signals over messages, four per message, IDs from `base_id` on.
pub fn layout(signals: &[Signal], base_id: u32) -> Vec<Message> {
    signals
        .chunks(4)
        .enumerate()
        .map(|(i, c)| Message { id: base_id + i as u32, name: format!("WBS_Werte_{:02}", i + 1), signals: c.to_vec() })
        .collect()
}

/// °C → raw value (rounded, clamped; NaN → NO_VALUE).
pub fn encode(c: f32) -> i16 {
    if !c.is_finite() {
        return NO_VALUE;
    }
    (c / FACTOR).round().clamp(-32767.0, 32767.0) as i16
}

/// The 8 data bytes of a message (unused slots carry NO_VALUE).
pub fn pack(values: &[f32]) -> [u8; 8] {
    let mut d = [0u8; 8];
    for i in 0..4 {
        let v = values.get(i).map(|v| encode(*v)).unwrap_or(NO_VALUE);
        d[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
    }
    d
}

/// Signal names for a DBC: letters, digits and _, not starting with a digit, unique.
pub fn dbc_name(raw: &str, used: &mut Vec<String>) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| match c {
            'ä' => "ae".to_string(),
            'ö' => "oe".to_string(),
            'ü' => "ue".to_string(),
            'Ä' => "Ae".to_string(),
            'Ö' => "Oe".to_string(),
            'Ü' => "Ue".to_string(),
            'ß' => "ss".to_string(),
            'Ø' | 'ø' => "Mittel".to_string(),
            c if c.is_ascii_alphanumeric() => c.to_string(),
            _ => "_".to_string(),
        })
        .collect();
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    let mut s = s.trim_matches('_').to_string();
    if s.is_empty() || s.chars().next().unwrap().is_ascii_digit() {
        s = format!("S_{s}");
    }
    s.truncate(32);
    let mut out = s.clone();
    let mut n = 2;
    while used.iter().any(|u| u.eq_ignore_ascii_case(&out)) {
        out = format!("{s}_{n}");
        n += 1;
    }
    used.push(out.clone());
    out
}

/// DBC file text for the layout.
pub fn dbc(messages: &[Message], extended: bool, cycle_ms: u32) -> String {
    let mut o = String::new();
    o.push_str("VERSION \"\"\n\n\nNS_ :\n\tCM_\n\tBA_DEF_\n\tBA_\n\tVAL_\n\tBA_DEF_DEF_\n\nBS_:\n\nBU_: WBS\n\n");
    let dbc_id = |id: u32| if extended { id | 0x8000_0000 } else { id };
    for m in messages {
        o.push_str(&format!("BO_ {} {}: 8 WBS\n", dbc_id(m.id), m.name));
        for (i, s) in m.signals.iter().enumerate() {
            o.push_str(&format!(
                " SG_ {} : {}|16@1- (0.1,0) [-3276.8|3276.7] \"degC\" Vector__XXX\n",
                s.name,
                i * 16
            ));
        }
        o.push('\n');
    }
    o.push_str("\nCM_ BU_ WBS \"Waermebild-Studio (Waermebildkamera)\";\n");
    for m in messages {
        o.push_str(&format!("CM_ BO_ {} \"Messwerte der Waermebildkamera\";\n", dbc_id(m.id)));
        for s in &m.signals {
            o.push_str(&format!("CM_ SG_ {} {} \"{}\";\n", dbc_id(m.id), s.name, ascii(&s.comment)));
        }
    }
    o.push_str("BA_DEF_ BO_ \"GenMsgCycleTime\" INT 0 65535;\n");
    o.push_str("BA_DEF_ BO_ \"GenMsgSendType\" ENUM \"Cyclic\",\"NotUsed\";\n");
    o.push_str("BA_DEF_ \"BusType\" STRING ;\n");
    o.push_str("BA_DEF_DEF_ \"GenMsgCycleTime\" 0;\n");
    o.push_str("BA_DEF_DEF_ \"GenMsgSendType\" \"Cyclic\";\n");
    o.push_str("BA_DEF_DEF_ \"BusType\" \"CAN\";\n");
    for m in messages {
        o.push_str(&format!("BA_ \"GenMsgCycleTime\" BO_ {} {};\n", dbc_id(m.id), cycle_ms));
    }
    for m in messages {
        for s in &m.signals {
            o.push_str(&format!("VAL_ {} {} -32768 \"kein Wert\" ;\n", dbc_id(m.id), s.name));
        }
    }
    o
}

/// Comment text as plain ASCII (DBC tools differ in which code page they expect).
fn ascii(t: &str) -> String {
    t.chars()
        .map(|c| match c {
            'ä' => "ae".into(),
            'ö' => "oe".into(),
            'ü' => "ue".into(),
            'Ä' => "Ae".into(),
            'Ö' => "Oe".into(),
            'Ü' => "Ue".into(),
            'ß' => "ss".into(),
            '–' | '—' => "-".into(),
            '°' => "deg".into(),
            '"' => "'".into(),
            c if c.is_ascii() && !c.is_ascii_control() => c.to_string(),
            _ => "_".into(),
        })
        .collect()
}

// ---------------------------------------------------------------- PCAN-Basic

pub const BAUDRATES: &[(u32, u16)] = &[
    (1_000_000, 0x0014),
    (800_000, 0x0016),
    (500_000, 0x001C),
    (250_000, 0x011C),
    (125_000, 0x031C),
    (100_000, 0x432F),
    (50_000, 0x472F),
    (20_000, 0x532F),
    (10_000, 0x672F),
    (5_000, 0x7F7F),
];

/// PCAN-USB channel handles 1…16.
pub fn usb_channel(n: u8) -> u16 {
    match n {
        1..=8 => 0x50 + n as u16,
        9..=16 => 0x500 + n as u16,
        _ => 0x51,
    }
}

#[repr(C)]
struct TPCANMsg {
    id: u32,
    msgtype: u8,
    len: u8,
    data: [u8; 8],
}

type FnInit = unsafe extern "system" fn(u16, u16, u8, u32, u16) -> u32;
type FnUninit = unsafe extern "system" fn(u16) -> u32;
type FnWrite = unsafe extern "system" fn(u16, *mut TPCANMsg) -> u32;
type FnErrText = unsafe extern "system" fn(u32, u16, *mut u8) -> u32;

#[link(name = "kernel32")]
extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

struct Pcan {
    init: FnInit,
    uninit: FnUninit,
    write: FnWrite,
    err_text: FnErrText,
}

impl Pcan {
    fn load() -> Result<Self, String> {
        let name: Vec<u16> = "PCANBasic.dll\0".encode_utf16().collect();
        // SAFETY: plain Win32 calls with a NUL-terminated name; the functions are
        // looked up by their documented names and signatures from PCAN-Basic.
        unsafe {
            let h = LoadLibraryW(name.as_ptr());
            if h.is_null() {
                return Err("PCANBasic.dll nicht gefunden – bitte den PEAK-Treiber (PCAN-Basic) installieren".into());
            }
            let f = |n: &[u8]| {
                let p = GetProcAddress(h, n.as_ptr());
                if p.is_null() {
                    Err(format!("PCANBasic.dll: Funktion {} fehlt", String::from_utf8_lossy(&n[..n.len() - 1])))
                } else {
                    Ok(p)
                }
            };
            Ok(Self {
                init: std::mem::transmute::<*mut c_void, FnInit>(f(b"CAN_Initialize\0")?),
                uninit: std::mem::transmute::<*mut c_void, FnUninit>(f(b"CAN_Uninitialize\0")?),
                write: std::mem::transmute::<*mut c_void, FnWrite>(f(b"CAN_Write\0")?),
                err_text: std::mem::transmute::<*mut c_void, FnErrText>(f(b"CAN_GetErrorText\0")?),
            })
        }
    }

    fn text(&self, status: u32) -> String {
        let mut buf = [0u8; 256];
        // SAFETY: PCAN-Basic writes at most 256 bytes incl. NUL into the buffer.
        let r = unsafe { (self.err_text)(status, 0x07, buf.as_mut_ptr()) };
        if r != 0 {
            return format!("Fehler 0x{status:X}");
        }
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        // PCAN returns Windows-1252; good enough for German umlauts in most texts
        buf[..end].iter().map(|b| *b as char).collect()
    }
}

/// Settings for sending.
#[derive(Clone, PartialEq, Debug)]
pub struct CanConfig {
    pub channel: u8,
    pub baud: u32,
    pub base_id: u32,
    pub extended: bool,
    pub cycle_ms: u32,
}

struct Shared {
    frames: Mutex<Vec<(u32, [u8; 8])>>,
    stop: AtomicBool,
    sent: AtomicU64,
    errors: AtomicU64,
    last_error: Mutex<String>,
}

/// Background thread that sends the latest values every `cycle_ms`.
pub struct CanSender {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl CanSender {
    pub fn start(config: CanConfig) -> Result<Self, String> {
        let pcan = Pcan::load()?;
        let baud = BAUDRATES.iter().find(|b| b.0 == config.baud).map(|b| b.1).ok_or("unbekannte Baudrate")?;
        let ch = usb_channel(config.channel);
        // SAFETY: documented PCAN-Basic call; 0 for type/port/interrupt = plug-and-play hardware.
        let st = unsafe { (pcan.init)(ch, baud, 0, 0, 0) };
        if st != 0 {
            return Err(format!("PCAN-USB {} öffnen: {}", config.channel, pcan.text(st)));
        }
        let shared = Arc::new(Shared {
            frames: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            sent: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            last_error: Mutex::new(String::new()),
        });
        let sh = shared.clone();
        let cfg = config.clone();
        let thread = std::thread::Builder::new()
            .name("can-sender".into())
            .spawn(move || {
                let cycle = Duration::from_millis(cfg.cycle_ms.max(1) as u64);
                let mut next = Instant::now();
                while !sh.stop.load(Ordering::Relaxed) {
                    let frames = sh.frames.lock().unwrap().clone();
                    for (id, data) in frames {
                        let mut m = TPCANMsg { id, msgtype: if cfg.extended { 0x02 } else { 0x00 }, len: 8, data };
                        // SAFETY: the message struct matches TPCANMsg and lives for the call.
                        let st = unsafe { (pcan.write)(ch, &mut m) };
                        if st == 0 {
                            sh.sent.fetch_add(1, Ordering::Relaxed);
                        } else {
                            sh.errors.fetch_add(1, Ordering::Relaxed);
                            *sh.last_error.lock().unwrap() = pcan.text(st);
                        }
                    }
                    next += cycle;
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    } else {
                        next = now; // fell behind, do not burst
                    }
                }
                // SAFETY: closes the channel opened above.
                unsafe { (pcan.uninit)(ch) };
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { shared, thread: Some(thread) })
    }

    /// New values (°C) per message, in layout order.
    pub fn update(&self, msgs: &[Message], values: &[f32]) {
        let mut frames = Vec::with_capacity(msgs.len());
        let mut k = 0;
        for m in msgs {
            let n = m.signals.len();
            frames.push((m.id, pack(&values[k.min(values.len())..(k + n).min(values.len())])));
            k += n;
        }
        *self.shared.frames.lock().unwrap() = frames;
    }

    pub fn sent(&self) -> u64 {
        self.shared.sent.load(Ordering::Relaxed)
    }
    pub fn errors(&self) -> u64 {
        self.shared.errors.load(Ordering::Relaxed)
    }
    pub fn last_error(&self) -> String {
        self.shared.last_error.lock().unwrap().clone()
    }
}

impl Drop for CanSender {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(n: &str) -> Signal {
        Signal { name: n.into(), comment: n.into() }
    }

    #[test]
    fn packing() {
        assert_eq!(encode(23.45), 235);
        assert_eq!(encode(-12.3), -123);
        assert_eq!(encode(f32::NAN), NO_VALUE);
        assert_eq!(encode(99999.0), 32767);
        let d = pack(&[23.4, -1.0]);
        assert_eq!(&d[0..2], &234i16.to_le_bytes());
        assert_eq!(&d[2..4], &(-10i16).to_le_bytes());
        assert_eq!(&d[4..6], &NO_VALUE.to_le_bytes());
    }

    #[test]
    fn layout_and_dbc() {
        let s: Vec<Signal> = ["Max", "Min", "Mitte", "Mittel", "P1", "B1_Max"].iter().map(|n| sig(n)).collect();
        let m = layout(&s, 0x700);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].id, 0x701);
        assert_eq!(m[1].signals.len(), 2);
        let d = dbc(&m, false, 100);
        assert!(d.contains("BO_ 1792 WBS_Werte_01: 8 WBS"));
        assert!(d.contains(" SG_ B1_Max : 16|16@1- (0.1,0)"));
        assert!(d.contains("BA_ \"GenMsgCycleTime\" BO_ 1793 100;"));
        let e = dbc(&m, true, 100);
        assert!(e.contains(&format!("BO_ {} WBS_Werte_01", 0x700u32 | 0x8000_0000)));
    }

    #[test]
    fn names() {
        let mut used = Vec::new();
        assert_eq!(dbc_name("P1", &mut used), "P1");
        assert_eq!(dbc_name("p1", &mut used), "p1_2");
        assert_eq!(dbc_name("Heizkörper Ø", &mut used), "Heizkoerper_Mittel");
        assert_eq!(dbc_name("1. Punkt", &mut used), "S_1_Punkt");
    }

    #[test]
    fn channels() {
        assert_eq!(usb_channel(1), 0x51);
        assert_eq!(usb_channel(8), 0x58);
        assert_eq!(usb_channel(9), 0x509);
        assert_eq!(std::mem::size_of::<TPCANMsg>(), 16);
    }
}
