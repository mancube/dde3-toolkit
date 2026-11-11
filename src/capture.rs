//! Passive K-line capture — for improving/fixing dde3-toolkit.
//!
//! Opens a serial port (by default the ECU-side tap, `/dev/ttyUSB0`) and
//! ONLY READS. There is no transmit path anywhere in this module: the
//! [`Tap`] handle exposes no write method, the [`Framer`] is pure, and the
//! capture loop never writes a byte to the wire. Flash erase/write is
//! therefore impossible here — by construction, not by discipline.
//!
//! Line format (timestamp = seconds since capture start, one line per event):
//!
//! ```text
//! [  12.345] OK   12 04 00 16   IDENT
//! [  12.500] RAW  38 38 37 35 32 30 31 31   "8875201"
//! ```
//!
//! * `OK`  — a complete frame whose trailing XOR validates (the byte string
//!   shown is the full wire frame, payload + XOR). Framing uses the same
//!   shortest-XOR-prefix / 0x12-anchor semantics as
//!   [`crate::protocol::FrameParser`].
//! * `RAW` — bytes that never formed a complete frame: line noise and
//!   truncated frames. Kept verbatim so nothing is lost. (The IDENT reply is
//!   NOT one of these — it arrives in the normal `12 <len> A0 <ascii>`
//!   envelope like every other service.)
//!
//! Service names come from the frame catalog (HANDOVER.md §14–15); the
//! leading `12 <len> <sid> [<sub>]` prefix is matched on (sid, sub).

use std::collections::BTreeMap;
use std::io::Write as _;
use std::sync::atomic::Ordering;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serialport::{DataBits, Parity, SerialPort, StopBits};

// ---------------------------------------------------------------------------
// Service names (ported from tools/monitor.py, extended with the DTC/baud
// services from `protocol.rs`)
// ---------------------------------------------------------------------------

/// Name a DDE3 service from a frame payload: `12 <len> <sid> [<sub>] ...`.
pub fn name_service(payload: &[u8]) -> &'static str {
    if payload.len() < 3 || payload[0] != 0x12 {
        return "?";
    }
    let sid = payload[2];
    let sub = payload.get(3);
    // The ECU's ~16-byte periodic live block: `12 10 A0 10 00 ...` —
    // indistinguishable from a 5-id MW_SELECT_LESEN reply (same data, see
    // protocol::is_not_live_broadcast).
    if payload.get(1) == Some(&0x10) && payload.get(3) == Some(&0x10) {
        return "LIVE_BROADCAST (periodic/5-id)";
    }
    match (sid, sub) {
        (0x00, _) => "IDENT",
        (0x0D, _) => "UPROG_EIN",
        (0x0A, _) => "STATUS_CODIER_CHECKSUMME",
        (0x9F, _) => "DIAGNOSE_ENDE",
        (0x6D, _) => "STATUS_SYNC_MODE",
        (0x90, Some(0x42)) => "SEED_KEY",
        (0x90, Some(0x56)) => "SCHALTE_PROTOKOLL_DS2_NACH_K2",
        (0x90, _) => "service 0x90",
        (0x91, _) => "BAUD_UMSTELLUNG (0x91)",
        (0x06, _) => "FLASH_LESEN",
        (0x07, Some(0x06)) => "FLASH_LOESCHEN (ERASE!)",
        (0x07, Some(0x02)) => "FLASH_SCHREIBEN (WRITE!)",
        (0x07, Some(0x0F)) => "FLASH_SCHREIBEN_ENDE",
        (0x07, Some(0x0E)) => "DATENBEREICH_LOESCHEN_0E (fault clear)",
        (0x07, _) => "service 0x07",
        (0x0B, Some(0x10)) => "MW_SELECT_LESEN (live data)",
        (0x0B, Some(0x03)) => "MW_STATUS (live data)",
        (0x0B, _) => "service 0x0B",
        (0x04, Some(0x00)) => "FS_LESEN (DTC entry)",
        (0x04, Some(0x01)) => "FS_SHADOW_LESEN (freeze frame)",
        (0x04, _) => "service 0x04",
        (0x05, None) => "FS_LOESCHEN (DTC clear)",
        (0x14, Some(0x01)) => "FS_QUICK_LESEN (DTC count)",
        // ECU reply: `12 <len> <SB> <data...>` with SB a JOBRESULT status byte.
        (sb, _) if crate::protocol::known_status_sb(sb) => "REPLY",
        _ => "service ??",
    }
}

// ---------------------------------------------------------------------------
// Framer (pure — no I/O, fully unit-testable)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum Event {
    /// A complete frame: payload (trailing XOR stripped).
    Frame { payload: Vec<u8> },
    /// Bytes that never formed a complete frame (kept verbatim).
    Raw(Vec<u8>),
}

const MAX_BUF: usize = 192;

/// Silence after the last line byte that means "whatever is buffered will not
/// be completed": comfortably above the ~50 ms gap WinKFP inserts before the
/// trailing XOR byte.
const STALE: Duration = Duration::from_millis(150);

/// Streaming frame extractor with the same semantics as
/// [`crate::protocol::FrameParser`] (shortest XOR-terminated prefix anchored
/// at the ECU address; leading garbage skipped; oversized buffers resync by
/// keeping the tail half), plus: every skipped byte is emitted as an
/// [`Event::Raw`] instead of being dropped silently, and a stale buffer
/// (no line activity for longer than [`STALE`]) is flushed as [`Event::Raw`]
/// via [`Framer::stale`].
pub struct Framer {
    buf: Vec<u8>,
    garbage: Vec<u8>,
    last_rx: Option<Instant>,
}

impl Default for Framer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            garbage: Vec::new(),
            last_rx: None,
        }
    }

    /// Feed new bytes from the line; return events in arrival order.
    pub fn feed(&mut self, data: &[u8], now: Instant) -> Vec<Event> {
        if !data.is_empty() {
            self.last_rx = Some(now);
        }
        self.buf.extend_from_slice(data);
        self.extract()
    }

    /// If the buffer is non-empty and has seen no line activity for longer
    /// than [`STALE`], flush it as one [`Event::Raw`] (a truncated frame or
    /// a bare reply the line will not complete) and clear it.
    pub fn stale(&mut self, now: Instant) -> Option<Event> {
        if self.buf.is_empty() {
            return None;
        }
        let waiting = self.last_rx.map(|t| now.duration_since(t));
        if waiting.unwrap_or(Duration::ZERO) < STALE {
            return None;
        }
        let raw = std::mem::take(&mut self.buf);
        self.last_rx = None;
        Some(Event::Raw(raw))
    }

    /// End-of-capture flush: extract whatever frames are complete, then keep
    /// any remaining bytes verbatim as one [`Event::Raw`].
    pub fn drain(&mut self) -> Vec<Event> {
        let mut out = self.extract();
        if !self.buf.is_empty() {
            out.push(Event::Raw(std::mem::take(&mut self.buf)));
        }
        out
    }

    fn extract(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        loop {
            if self.buf.len() > MAX_BUF {
                // Persistent garbage: drop the front half (same as FrameParser),
                // keeping it in the log as RAW.
                let cut = self.buf.len() - MAX_BUF / 2;
                for _ in 0..cut {
                    self.garbage.push(self.buf.remove(0));
                }
            }
            if self.buf.is_empty() {
                if !self.garbage.is_empty() {
                    out.push(Event::Raw(std::mem::take(&mut self.garbage)));
                }
                break;
            }
            if self.buf[0] == crate::protocol::ECU_ADDR {
                match xor_end(&self.buf) {
                    Some(k) => {
                        // Time order: any preceding garbage run goes first.
                        if !self.garbage.is_empty() {
                            out.push(Event::Raw(std::mem::take(&mut self.garbage)));
                        }
                        let payload = self.buf[..k].to_vec();
                        self.buf.drain(0..=k);
                        out.push(Event::Frame { payload });
                    }
                    None => break, // partial frame, wait for the trailing XOR
                }
            } else {
                // Leading non-0x12 byte: line noise, or the start of a
                // bare-ASCII reply (e.g. the IDENT string). Keep it in the
                // RAW run; a 0x12-anchored frame behind it is picked up on a
                // later iteration of this loop.
                self.garbage.push(self.buf.remove(0));
                if self.garbage.len() >= 64 {
                    out.push(Event::Raw(std::mem::take(&mut self.garbage)));
                }
            }
        }
        out
    }
}

/// Index of the trailing XOR byte: shortest `k >= 2` with
/// `xor(s[..k]) == s[k]` (same rule as `protocol::xor_end`).
fn xor_end(s: &[u8]) -> Option<usize> {
    for k in 2..s.len() {
        let x = crate::protocol::xor_cs(&s[..k]);
        if x == s[k] {
            return Some(k);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tap (read-only serial handle)
// ---------------------------------------------------------------------------

/// Read-only handle on the serial line. Deliberately exposes **no** write
/// method: the capture command cannot drive the K-line, so nothing it does
/// can erase or program the ECU.
struct Tap {
    port: Box<dyn SerialPort>,
}

impl Tap {
    fn open(path: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(path, baud)
            .data_bits(DataBits::Eight)
            .parity(Parity::Even)
            .stop_bits(StopBits::One)
            .timeout(Duration::from_millis(10))
            .open()
            .with_context(|| format!("cannot open {path}"))?;
        Ok(Self { port })
    }

    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // serialport::SerialPort::read — the only operation this handle ever
        // performs. The 10 ms port timeout is a "no data yet" poll, not an
        // error (same convention as protocol::Kline::read_chunk).
        match self.port.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Capture run
// ---------------------------------------------------------------------------

pub struct CaptureOptions {
    pub port: String,
    pub baud: u32,
    /// Mirror all lines to this file (for `tail -f`).
    pub out: Option<std::path::PathBuf>,
    /// Stop after this long (`None` = until Ctrl-C).
    pub duration: Option<Duration>,
}

pub struct Summary {
    pub frames: u64,
    pub raw_bytes: u64,
    pub services: BTreeMap<String, u64>,
    pub elapsed: Duration,
}

impl Summary {
    pub fn render(&self) -> String {
        let mut s = format!(
            "capture summary: {} frames, {} raw bytes, {:7.3} s\n",
            self.frames,
            self.raw_bytes,
            self.elapsed.as_secs_f32()
        );
        for (name, n) in &self.services {
            s.push_str(&format!("  {:<38} {}\n", name, n));
        }
        s
    }
}

static STOP: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
fn install_sigint() {
    extern "C" fn handler(_sig: libc::c_int) {
        STOP.store(true, Ordering::Relaxed);
    }
    // The handler body is only an atomic store — safe in practice.
    let _ = unsafe {
        libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t)
    };
}

#[cfg(not(unix))]
fn install_sigint() {}

pub fn run(opts: &CaptureOptions) -> Result<Summary> {
    let mut tap = Tap::open(&opts.port, opts.baud)?;
    install_sigint();
    STOP.store(false, Ordering::Relaxed);

    let mut file: Option<std::io::BufWriter<std::fs::File>> = match &opts.out {
        Some(p) => Some(
            std::io::BufWriter::new(
                std::fs::File::create(p)
                    .with_context(|| format!("cannot create capture file {}", p.display()))?,
            ),
        ),
        None => None,
    };

    let t0 = Instant::now();
    let deadline = opts.duration.map(|d| t0 + d);

    println!("dde3-toolkit capture — PASSIVE (read-only, nothing is transmitted)");
    println!(
        "port={} baud={} 8E1  (Ctrl-C to stop)",
        opts.port, opts.baud
    );
    if let Some(f) = file.as_mut() {
        let _ = writeln!(
            f,
            "# dde3-toolkit capture — PASSIVE (read-only, nothing is transmitted)"
        );
        let _ = writeln!(f, "# port={} baud={} 8E1", opts.port, opts.baud);
        let _ = f.flush();
    }

    let mut framer = Framer::new();
    let mut frames: u64 = 0;
    let mut raw_bytes: u64 = 0;
    let mut services: BTreeMap<String, u64> = BTreeMap::new();
    let mut buf = [0u8; 512];

    while !STOP.load(Ordering::Relaxed) {
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                break;
            }
        }

        let now = Instant::now();
        let n = tap.read(&mut buf)?;
        let mut events = if n > 0 {
            framer.feed(&buf[..n], now)
        } else {
            Vec::new()
        };
        if let Some(ev) = framer.stale(now) {
            events.push(ev);
        }
        for ev in events {
            let (line, frame, name, nbytes) = match &ev {
                Event::Frame { payload } => {
                    let name = name_service(payload);
                    let wire = crate::protocol::build_frame(payload);
                    (
                        format!(
                            "[{:9.3}] OK   {}   {}",
                            now.duration_since(t0).as_secs_f32(),
                            crate::protocol::hex(&wire),
                            name
                        ),
                        true,
                        name.to_string(),
                        wire.len() as u64,
                    )
                }
                Event::Raw(bytes) => {
                    let hex = crate::protocol::hex(bytes);
                    let ascii: String = bytes
                        .iter()
                        .filter(|b| matches!(**b, 0x20..=0x7E))
                        .map(|b| *b as char)
                        .collect();
                    let ascii = if !ascii.is_empty() && ascii.len() == bytes.len() {
                        format!("  \"{}\"", ascii)
                    } else {
                        String::new()
                    };
                    (
                        format!(
                            "[{:9.3}] RAW  {}{}",
                            now.duration_since(t0).as_secs_f32(),
                            hex,
                            ascii
                        ),
                        false,
                        String::new(),
                        bytes.len() as u64,
                    )
                }
            };
            if frame {
                frames += 1;
                *services.entry(name).or_insert(0) += 1;
            } else {
                raw_bytes += nbytes;
            }
            println!("{line}");
            if let Some(f) = file.as_mut() {
                let _ = writeln!(f, "{line}");
                let _ = f.flush();
            }
        }
    }

    // End-of-capture flush so a trailing partial frame is not lost.
    for ev in framer.drain() {
        let line = match &ev {
            Event::Frame { payload } => {
                frames += 1;
                let name = name_service(payload);
                *services.entry(name.to_string()).or_insert(0) += 1;
                let wire = crate::protocol::build_frame(payload);
                format!("[end] OK   {}   {}", crate::protocol::hex(&wire), name)
            }
            Event::Raw(bytes) => {
                raw_bytes += bytes.len() as u64;
                format!("[end] RAW  {}", crate::protocol::hex(bytes))
            }
        };
        println!("{line}");
        if let Some(f) = file.as_mut() {
            let _ = writeln!(f, "{line}");
        }
    }

    let summary = Summary {
        frames,
        raw_bytes,
        services,
        elapsed: Instant::now().duration_since(t0),
    };
    let rendered = summary.render();
    print!("{rendered}");
    if let Some(f) = file.as_mut() {
        let _ = writeln!(f, "{rendered}");
        let _ = f.flush();
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(payload: &[u8]) -> Vec<u8> {
        crate::protocol::build_frame(payload)
    }

    #[test]
    fn names_known_services() {
        assert_eq!(name_service(&[0x12, 0x04, 0x00]), "IDENT");
        assert_eq!(name_service(&[0x12, 0x09, 0x06, 0x00, 0x80, 0x03, 0x80]), "FLASH_LESEN");
        assert_eq!(name_service(&[0x12, 0x09, 0x07, 0x06]), "FLASH_LOESCHEN (ERASE!)");
        assert_eq!(name_service(&[0x12, 0x09, 0x07, 0x02]), "FLASH_SCHREIBEN (WRITE!)");
        assert_eq!(name_service(&[0x12, 0x08, 0x90, 0x42]), "SEED_KEY");
        assert_eq!(name_service(&[0x12, 0x0F, 0x0B, 0x10]), "MW_SELECT_LESEN (live data)");
        assert_eq!(name_service(&[0x12, 0x05, 0x14, 0x01]), "FS_QUICK_LESEN (DTC count)");
        assert_eq!(name_service(&[0x12, 0x04, 0x05]), "FS_LOESCHEN (DTC clear)");
        // ECU reply with OKAY status at [2]
        assert_eq!(name_service(&[0x12, 0x05, 0xA0, 0x03]), "REPLY");
        // the periodic 16-byte live block: 12 10 A0 10 00 ...
        assert_eq!(
            name_service(&[0x12, 0x10, 0xA0, 0x10]),
            "LIVE_BROADCAST (periodic/5-id)"
        );
        // unknown
        assert_eq!(name_service(&[0x12, 0x03, 0x77]), "service ??");
    }

    #[test]
    fn real_car_wire_fixture_splits_frames() {
        // Merged capture from `dde3 toolkit log.txt` (M47): ECU live reply
        // immediately followed by the echo of our request.
        let wire: Vec<u8> = vec![
            0x12, 0x10, 0xA0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xDE, 0x0B, 0x26, 0x02,
            0x4A, 0x0A, 0x12, 0x0F, 0x0B, 0x10, 0x0F, 0x83, 0x0F, 0x10, 0x0F, 0x40, 0x0F, 0x00,
            0x0F, 0x65, 0xBF,
        ];
        let mut f = Framer::new();
        let events = f.feed(&wire, Instant::now());
        let frames: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Frame { payload } => Some(payload.clone()),
                Event::Raw(_) => None,
            })
            .collect();
        assert_eq!(frames.len(), 2, "expected 2 frames, got {events:?}");
        assert_eq!(frames[0][2], 0xA0, "live reply first");
        assert_eq!(frames[1][3], 0x10, "echo of MW_SELECT_LESEN second");
    }

    #[test]
    fn leading_garbage_is_kept_as_raw_and_frame_still_found() {
        let mut wire = vec![0x00, 0xFF];
        wire.extend(frame(&[0x12, 0x04, 0x00]));
        let mut f = Framer::new();
        let events = f.feed(&wire, Instant::now());
        assert_eq!(events.len(), 2, "RAW run then frame, got: {events:?}");
        match &events[0] {
            Event::Raw(b) => assert_eq!(b, &vec![0x00, 0xFF]),
            _ => panic!("expected RAW first"),
        }
        match &events[1] {
            Event::Frame { payload } => assert_eq!(payload, &vec![0x12, 0x04, 0x00]),
            _ => panic!("expected FRAME second"),
        }
    }

    #[test]
    fn unanchored_ascii_lands_in_raw() {
        // Unanchored ASCII bytes (no 0x12 start) are kept as RAW. The real
        // IDENT reply does not do this (it is enveloped, see protocol.rs) —
        // this just pins the framer's verbatim RAW behaviour for such bytes.
        let ident = b"8875201";
        let mut wire = ident.to_vec();
        wire.push(crate::protocol::xor_cs(&ident[..]));
        let t = Instant::now();
        let mut f = Framer::new();
        let events = f.feed(&wire, t);
        // Nothing was framed (no anchor); the bytes surface verbatim as RAW
        // (unanchored runs flush immediately, preserving order and content).
        let raw: Vec<u8> = events
            .iter()
            .filter_map(|e| match e {
                Event::Raw(b) => Some(b.as_slice()),
                _ => None,
            })
            .flatten()
            .copied()
            .collect();
        assert_eq!(&raw[..ident.len()], ident, "ident string preserved: {events:?}");
        assert!(f.stale(t + STALE).is_none(), "buffer already drained");
    }

    #[test]
    fn partial_frame_reassembles_within_stale_window() {
        // WinKFP's ~50 ms XOR gap is below STALE, so a split frame reassembles.
        let mut f = Framer::new();
        let t = Instant::now();
        let full = frame(&[0x12, 0x04, 0x00]);
        let _ = f.feed(&full[..3], t); // payload without trailing XOR
        assert!(
            f.stale(t + Duration::from_millis(50)).is_none(),
            "50 ms gap is normal"
        );
        let t2 = t + Duration::from_millis(51);
        let events = f.feed(&full[3..], t2);
        match &events[0] {
            Event::Frame { payload } => assert_eq!(payload, &vec![0x12, 0x04, 0x00]),
            _ => panic!("expected reassembled frame, got {events:?}"),
        }
    }

    #[test]
    fn partial_frame_flushes_as_raw_when_truly_stale() {
        // If the trailing XOR never arrives, the stale flush keeps the bytes.
        let full = frame(&[0x12, 0x04, 0x00]);
        let t = Instant::now();
        let mut f = Framer::new();
        let _ = f.feed(&full[..3], t);
        let ev = f.stale(t + STALE + Duration::from_millis(1));
        assert!(
            matches!(ev, Some(Event::Raw(ref b)) if b == &full[..3]),
            "got {ev:?}"
        );
    }

    #[test]
    fn drain_returns_pending_bytes_at_end() {
        let mut f = Framer::new();
        let full = frame(&[0x12, 0x04, 0x0D]);
        let t = Instant::now();
        let _ = f.feed(&full[..2], t);
        let events = f.drain();
        assert!(
            matches!(events.as_slice(), [Event::Raw(ref b)] if b == &full[..2]),
            "got {events:?}"
        );
    }
}
