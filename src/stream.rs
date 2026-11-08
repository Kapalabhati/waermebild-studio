//! Network stream: a small built-in web server.
//!
//! - `/`            web page with the live picture and the current measured values
//! - `/stream.mjpg` live picture as MJPEG (browser, VLC, OBS, …)
//! - `/bild.jpg`    the current picture as one JPEG
//! - `/werte.json`  the current measured values
//!
//! Uses only the standard library: one thread accepts connections, every client gets
//! its own thread that waits for new pictures.

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Default)]
struct Latest {
    seq: u64,
    jpeg: Arc<Vec<u8>>,
    json: Arc<String>,
}

struct Shared {
    latest: Mutex<Latest>,
    cond: Condvar,
    stop: AtomicBool,
    /// clients currently receiving the MJPEG stream
    viewers: AtomicUsize,
}

pub struct StreamServer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    pub port: u16,
    pub network: bool,
}

impl StreamServer {
    /// `network = false` listens on this computer only (127.0.0.1).
    pub fn start(port: u16, network: bool) -> Result<Self, String> {
        let ip = if network { IpAddr::V4(Ipv4Addr::UNSPECIFIED) } else { IpAddr::V4(Ipv4Addr::LOCALHOST) };
        let listener = TcpListener::bind(SocketAddr::new(ip, port)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("Port {port} ist schon belegt – bitte einen anderen wählen")
            } else {
                e.to_string()
            }
        })?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let shared = Arc::new(Shared {
            latest: Mutex::new(Latest::default()),
            cond: Condvar::new(),
            stop: AtomicBool::new(false),
            viewers: AtomicUsize::new(0),
        });
        let sh = shared.clone();
        let thread = std::thread::Builder::new()
            .name("stream-server".into())
            .spawn(move || {
                while !sh.stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((sock, _)) => {
                            let sh = sh.clone();
                            let _ = std::thread::Builder::new().name("stream-client".into()).spawn(move || {
                                let _ = handle(sock, &sh);
                            });
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
                        Err(_) => std::thread::sleep(Duration::from_millis(200)),
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { shared, thread: Some(thread), port, network })
    }

    /// Number of clients watching the live stream right now.
    pub fn viewers(&self) -> usize {
        self.shared.viewers.load(Ordering::Relaxed)
    }

    /// Hands a new picture and the matching values to all clients.
    pub fn publish(&self, jpeg: Vec<u8>, json: String) {
        let mut l = self.shared.latest.lock().unwrap();
        l.seq += 1;
        l.jpeg = Arc::new(jpeg);
        l.json = Arc::new(json);
        drop(l);
        self.shared.cond.notify_all();
    }

    /// Only the values (when nobody watches the picture, it need not be encoded).
    pub fn publish_values(&self, json: String) {
        self.shared.latest.lock().unwrap().json = Arc::new(json);
    }

    /// Addresses under which the page can be opened.
    pub fn urls(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.network {
            if let Some(ip) = local_ip() {
                v.push(format!("http://{ip}:{}/", self.port));
            }
        }
        v.push(format!("http://localhost:{}/", self.port));
        v
    }
}

impl Drop for StreamServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.cond.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The address of this computer in the local network (no data is sent).
pub fn local_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

fn handle(mut sock: TcpStream, sh: &Shared) -> std::io::Result<()> {
    sock.set_read_timeout(Some(Duration::from_secs(5)))?;
    sock.set_nodelay(true)?;
    let mut reader = BufReader::new(sock.try_clone()?);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    // skip the header lines
    loop {
        let mut l = String::new();
        if reader.read_line(&mut l)? == 0 || l == "\r\n" || l == "\n" {
            break;
        }
    }
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("/").split('?').next().unwrap_or("/");
    if method != "GET" && method != "HEAD" {
        return respond(&mut sock, "405 Method Not Allowed", "text/plain; charset=utf-8", b"nur GET");
    }
    match path {
        "/" | "/index.html" => respond(&mut sock, "200 OK", "text/html; charset=utf-8", PAGE.as_bytes()),
        "/werte.json" => {
            let j = sh.latest.lock().unwrap().json.clone();
            let body: &[u8] = if j.is_empty() { b"{}" } else { j.as_bytes() };
            respond(&mut sock, "200 OK", "application/json; charset=utf-8", body)
        }
        "/bild.jpg" => {
            let j = wait_first(sh);
            match j {
                Some(j) => respond(&mut sock, "200 OK", "image/jpeg", &j),
                None => respond(&mut sock, "503 Service Unavailable", "text/plain; charset=utf-8", b"noch kein Bild"),
            }
        }
        "/stream.mjpg" => {
            sh.viewers.fetch_add(1, Ordering::Relaxed);
            let r = mjpeg(&mut sock, sh);
            sh.viewers.fetch_sub(1, Ordering::Relaxed);
            r
        }
        _ => respond(&mut sock, "404 Not Found", "text/plain; charset=utf-8", b"nicht gefunden"),
    }
}

/// The current picture; waits up to 3 s for the first one (the app only encodes
/// pictures while someone asks for them).
fn wait_first(sh: &Shared) -> Option<Arc<Vec<u8>>> {
    sh.viewers.fetch_add(1, Ordering::Relaxed);
    let mut l = sh.latest.lock().unwrap();
    let start = l.seq;
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while l.seq == start && std::time::Instant::now() < deadline && !sh.stop.load(Ordering::Relaxed) {
        l = sh.cond.wait_timeout(l, Duration::from_millis(200)).unwrap().0;
    }
    let j = l.jpeg.clone();
    drop(l);
    sh.viewers.fetch_sub(1, Ordering::Relaxed);
    (!j.is_empty()).then_some(j)
}

fn respond(sock: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        sock,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    sock.write_all(body)?;
    sock.flush()
}

fn mjpeg(sock: &mut TcpStream, sh: &Shared) -> std::io::Result<()> {
    sock.set_write_timeout(Some(Duration::from_secs(5)))?;
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    )?;
    let mut last = 0u64;
    loop {
        let jpeg = {
            let mut l = sh.latest.lock().unwrap();
            while l.seq == last && !sh.stop.load(Ordering::Relaxed) {
                l = sh.cond.wait_timeout(l, Duration::from_millis(500)).unwrap().0;
            }
            if sh.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            last = l.seq;
            l.jpeg.clone()
        };
        if jpeg.is_empty() {
            continue;
        }
        write!(sock, "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len())?;
        sock.write_all(&jpeg)?;
        sock.write_all(b"\r\n")?;
        sock.flush()?;
    }
}

/// JSON string with quotes and escapes.
pub fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// JSON number (null for missing values).
pub fn json_num(v: f32) -> String {
    if v.is_finite() {
        format!("{:.2}", v)
    } else {
        "null".into()
    }
}

const PAGE: &str = r##"<!doctype html>
<html lang="de">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Wärmebild-Studio Live</title>
<style>
  :root { color-scheme: dark; --bg:#15171c; --card:#24282f; --muted:#969caa; --hot:#ff5440; --cold:#46a0ff; --accent:#ff8a1f; }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: #eee; font: 15px/1.4 system-ui, "Segoe UI", sans-serif; }
  header { display: flex; align-items: center; gap: 12px; padding: 10px 16px; background: #111318; }
  header h1 { font-size: 18px; margin: 0; font-weight: 600; }
  #state { margin-left: auto; color: var(--muted); font-size: 13px; }
  #state.ok::before { content: "● "; color: #50dc78; }
  main { display: grid; grid-template-columns: minmax(0, 1fr) 300px; gap: 16px; padding: 16px; }
  @media (max-width: 860px) { main { grid-template-columns: 1fr; } }
  .view { background: #0c0d10; border-radius: 8px; overflow: hidden; display: flex; justify-content: center; }
  .view img { max-width: 100%; height: auto; display: block; }
  .card { background: var(--card); border-radius: 8px; padding: 12px 14px; margin-bottom: 12px; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
  .lbl { color: var(--muted); font-size: 12px; }
  .big { font-size: 22px; font-weight: 600; }
  table { width: 100%; border-collapse: collapse; }
  td { padding: 4px 0; border-bottom: 1px solid #333842; }
  td:last-child { text-align: right; font-variant-numeric: tabular-nums; }
  .sw { display: inline-block; width: 10px; height: 10px; border-radius: 2px; margin-right: 6px; }
  a { color: var(--accent); }
  .small { color: var(--muted); font-size: 12px; }
</style>
</head>
<body>
<header><h1>Wärmebild-Studio · Live</h1><span id="state">verbinde …</span></header>
<main>
  <div class="view"><img id="live" src="stream.mjpg" alt="Live-Wärmebild"></div>
  <div>
    <div class="card grid">
      <div><div class="lbl">Maximum</div><div class="big" id="max" style="color:var(--hot)">–</div></div>
      <div><div class="lbl">Minimum</div><div class="big" id="min" style="color:var(--cold)">–</div></div>
      <div><div class="lbl">Bildmitte</div><div class="big" id="mitte">–</div></div>
      <div><div class="lbl">Mittelwert</div><div class="big" id="mittel">–</div></div>
    </div>
    <div class="card"><div class="lbl" style="margin-bottom:4px">Messwerkzeuge</div><table id="tools"></table></div>
    <div class="card small">
      <div id="info">–</div>
      <div style="margin-top:6px"><a href="bild.jpg" download>Einzelbild speichern</a> · <a href="werte.json">Werte (JSON)</a></div>
      <div style="margin-top:6px">Stream für VLC/OBS: <code id="surl"></code></div>
    </div>
  </div>
</main>
<script>
  const $ = id => document.getElementById(id);
  $("surl").textContent = location.origin + "/stream.mjpg";
  const fmt = (v, u) => v === null || v === undefined ? "–" : v.toFixed(1).replace(".", ",") + " " + u;
  async function tick() {
    try {
      const r = await fetch("werte.json", { cache: "no-store" });
      const d = await r.json();
      if (!d.zeit) throw 0;
      const u = d.einheit;
      $("max").textContent = fmt(d.max, u); $("min").textContent = fmt(d.min, u);
      $("mitte").textContent = fmt(d.mitte, u); $("mittel").textContent = fmt(d.mittel, u);
      $("tools").innerHTML = "";
      for (const t of d.werkzeuge) {
        const tr = document.createElement("tr");
        const a = document.createElement("td"), b = document.createElement("td");
        const sw = document.createElement("span"); sw.className = "sw"; sw.style.background = t.farbe;
        a.append(sw, document.createTextNode(t.name));
        b.textContent = Object.entries(t.werte).map(([k, v]) => (Object.keys(t.werte).length > 1 ? k + " " : "") + fmt(v, u)).join(" · ");
        if (t.alarm) b.style.color = "var(--hot)";
        tr.append(a, b); $("tools").append(tr);
      }
      $("info").textContent = d.zeit + " · ε " + d.emission.toFixed(2).replace(".", ",") + " · " + d.quelle;
      $("state").textContent = "verbunden"; $("state").className = "ok";
    } catch (e) { $("state").textContent = "keine Verbindung"; $("state").className = ""; }
  }
  tick(); setInterval(tick, 1000);
  $("live").addEventListener("error", () => setTimeout(() => { $("live").src = "stream.mjpg?" + Date.now(); }, 2000));
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn serves_page_values_and_stream() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let srv = StreamServer::start(port, false).unwrap();
        srv.publish(vec![0xFF, 0xD8, 1, 2, 3, 0xFF, 0xD9], "{\"zeit\":\"x\"}".into());
        let get = |p: &str| {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(s, "GET {p} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut out = Vec::new();
            s.read_to_end(&mut out).unwrap();
            String::from_utf8_lossy(&out).to_string()
        };
        assert!(get("/").contains("Wärmebild-Studio"));
        assert!(get("/werte.json").ends_with("{\"zeit\":\"x\"}"));
        assert!(get("/nix").starts_with("HTTP/1.1 404"));
        // stream: read the header and one frame
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET /stream.mjpg HTTP/1.1\r\n\r\n").unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        srv.publish(vec![0xFF, 0xD8, 9, 0xFF, 0xD9], "{}".into());
        let mut buf = [0u8; 512];
        let mut got = Vec::new();
        while !String::from_utf8_lossy(&got).contains("image/jpeg") {
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0);
            got.extend_from_slice(&buf[..n]);
        }
        assert!(String::from_utf8_lossy(&got).contains("multipart/x-mixed-replace"));
        assert_eq!(srv.viewers(), 1);
    }

    #[test]
    fn json_escaping() {
        assert_eq!(json_str("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(json_num(f32::NAN), "null");
    }
}
