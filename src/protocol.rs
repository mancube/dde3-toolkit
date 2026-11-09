//! BMW DDE3.0 / Bosch EDC15 DS2 K-line protocol.
//!
//! Reverse-engineered from `10DDE30.prg` via the WinkFP VM in `tools/prg_vm.py`.
//! See `HANDOVER.md` §14–15 for the frame catalog and open questions.
//!
//! Wire framing (CONFIRMED by passive K-line capture 2026-11-09, /dev/ttyUSB0,
//! while WinKFP drove the bus — the sniffer saw exactly what the ECU saw):
//!   `<DS2 payload> <XOR>` at 9600 8E1, half-duplex with echo.
//!   NO 0xB8 prefix, no tester address, no length byte — the earlier B8/F1/LEN
//!   hypothesis was wrong. The trailing XOR covers all payload bytes.
//!   Captured frames (payload + checksum):
//!     `12 04 0D 1B`  UPROG_EIN
//!     `12 04 00 16`  IDENT
//!     `12 05 90 56 D1`  SCHALTE_PROTOKOLL_DS2_NACH_K2
//!   Frames are delimited by line silence. WinKFP inserts a ~50 ms gap before
//!   the trailing XOR byte, so the parser accumulates across reads and only
//!   commits a frame once the trailing XOR validates.
//!   The DS2 transport parameters come from INITIALISIERUNG's `xsetpar`:
//!     `06 00 80 25 12 00 00 00 00 00 E8 03 32 00 14 00 03 00`
//!   (concept=DS2, baud=9600, ECU_addr=0x12, timeout=1000 ms, ...)

use std::collections::VecDeque;
use std::io::ErrorKind;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serialport::{ClearBuffer, DataBits, Parity, SerialPort, StopBits};

use crate::checksum::{CAL_LEN, CAL_START};
use crate::sim::SimEcu;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const ECU_ADDR: u8 = 0x12;

/// DDE3 flash base address (file offset). DDE3 reads from 0x00000.
pub const BASE: u32 = 0x00000;
/// DDE3 flash image size: 256 KB.
pub const IMAGE_SIZE: usize = 0x40000;

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
/// A wire frame. On the DDE3 K-line a frame is just the DS2 payload plus the
/// trailing XOR byte — there are no address or length fields on the wire.
pub struct Frame {
    pub payload: Vec<u8>,
}

impl std::fmt::Display for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {:02X}", hex(&self.payload), xor_cs(&self.payload))
    }
}

pub fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn xor_cs(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, b| a ^ b)
}

/// Build a wire frame: `<payload> <XOR of all payload bytes>`.
pub fn build_frame(payload: &[u8]) -> Vec<u8> {
    let mut v = payload.to_vec();
    v.push(xor_cs(&v));
    v
}

/// ECU reply convention (derived from `frames.md` — "Generic OK response
/// fed to each job: `92 04 A0`" — and the FLASH_LESEN listing in
/// `decompiled_winkfp.txt`, which reads the status byte at reply offset 2
/// and copies data from reply offset 3):
///
/// ```text
/// <92> <len echo> <SB> [<data ...>]  + trailing XOR on the wire
/// ```
///
/// MW live data (`0x0B`) uses the echo style `92 05 0B <sub> A0 <data>`.
/// [`request`] treats byte [2] as the JOBRESULT status byte when it is a
/// known status (A0/A1/A2/B0–B2/FF/00): OKAY → Ok, BUSY → retry, else NRC.
/// Any other byte at [2] means a data reply and is returned as-is.
pub fn status_byte(reply: &[u8]) -> Option<u8> {
    reply.get(2).copied()
}

pub fn known_status_sb(sb: u8) -> bool {
    matches!(sb, 0xA0 | 0xA1 | 0xA2 | 0xB0 | 0xB1 | 0xB2 | 0xFF | 0x00)
}

/// Maximum plausible DS2 frame length; longer buffers are resynced by
/// dropping leading bytes.
const MAX_FRAME: usize = 64;

#[derive(Default)]
pub struct FrameParser {
    buf: Vec<u8>,
}

impl FrameParser {
    /// Accumulate bytes. Nothing is emitted until [`take_frame`] succeeds, so
    /// frames that arrive across several reads (WinKFP gaps ~50 ms before the
    /// trailing XOR byte) are handled correctly.
    pub fn feed(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        if self.buf.len() > MAX_FRAME {
            self.buf.remove(0);
        }
    }

    /// Try to complete a frame from the accumulated buffer. Call after a line
    /// silence. The last byte is the XOR checksum of the frame payload.
    ///
    /// Returns the frame, or `None` when no alignment ends in a valid XOR.
    /// On `None` the buffer is kept (the trailing XOR byte may still be
    /// on its way); [`feed`] drops leading bytes once the buffer outgrows
    /// [`MAX_FRAME`], which is also how persistent garbage is flushed.
    pub fn take_frame(&mut self) -> Option<Frame> {
        let mut i = 0;
        while self.buf.len() - i >= 2 {
            let body = &self.buf[i..self.buf.len() - 1];
            if !body.is_empty() && xor_cs(body) == self.buf[self.buf.len() - 1] {
                let payload = self.buf.drain(i..self.buf.len() - 1).collect();
                return Some(Frame { payload });
            }
            i += 1;
        }
        None
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Nrc {
    pub sid: u8,
    pub code: u8,
}

impl std::fmt::Display for Nrc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DS2 status byte 0x{:02X} for service 0x{:02X}", self.code, self.sid)
    }
}

impl std::error::Error for Nrc {}

pub fn is_link_error(err: &anyhow::Error) -> bool {
    for cause in err.chain() {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if matches!(
                io.kind(),
                ErrorKind::BrokenPipe
                    | ErrorKind::NotConnected
                    | ErrorKind::UnexpectedEof
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::ConnectionReset
            ) {
                return true;
            }
            if matches!(io.raw_os_error(), Some(5 | 6 | 19 | 32 | 9)) {
                return true;
            }
        }
        if cause.downcast_ref::<serialport::Error>().is_some() {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// JOBRESULT status byte table (from jobresult.txt / HANDOVER.md §6)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Okay,
    Busy,
    ErrorSgRejected,
    ErrorSgParameter,
    ErrorSgFunction,
    ErrorSgNumber,
    ErrorSgNack,
    ErrorSgUnknown(u8),
}

impl JobStatus {
    pub fn from_sb(sb: u8, _sid: u8) -> Self {
        match sb {
            0xA0 => JobStatus::Okay,
            0xA1 => JobStatus::Busy,
            0xA2 => JobStatus::ErrorSgRejected,
            0xB0 => JobStatus::ErrorSgParameter,
            0xB1 => JobStatus::ErrorSgFunction,
            0xB2 => JobStatus::ErrorSgNumber,
            0xFF => JobStatus::ErrorSgNack,
            0x00 => JobStatus::ErrorSgUnknown(sb),
            other => JobStatus::ErrorSgUnknown(other),
        }
    }

    pub fn is_okay(self) -> bool {
        matches!(self, JobStatus::Okay)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Okay => "OKAY",
            JobStatus::Busy => "BUSY",
            JobStatus::ErrorSgRejected => "ERROR_SG_REJECTED",
            JobStatus::ErrorSgParameter => "ERROR_SG_PARAMETER",
            JobStatus::ErrorSgFunction => "ERROR_SG_FUNCTION",
            JobStatus::ErrorSgNumber => "ERROR_SG_NUMBER",
            JobStatus::ErrorSgNack => "ERROR_SG_NACK",
            JobStatus::ErrorSgUnknown(_) => "ERROR_SG_UNKNOWN",
        }
    }
}

// ---------------------------------------------------------------------------
// Fixed frame builders (from tools/analysis/frames.md)
// ---------------------------------------------------------------------------

/// Enter programming mode: `12 04 0D`.
pub fn frame_uprog_ein() -> Vec<u8> {
    vec![0x12, 0x04, 0x0D]
}

/// ECU identification: `12 04 00`.
pub fn frame_ident() -> Vec<u8> {
    vec![0x12, 0x04, 0x00]
}

/// Checksum status: `12 04 0A`.
pub fn frame_checksum_status() -> Vec<u8> {
    vec![0x12, 0x04, 0x0A]
}

/// Leave / end session: `12 04 9F`.
pub fn frame_diagnose_ende() -> Vec<u8> {
    vec![0x12, 0x04, 0x9F]
}

/// Seed/Key init: `12 08 90 42 4D 57 05 <Z>`.
pub fn frame_seed_key(z: u8) -> Vec<u8> {
    vec![0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, 0x05, z]
}

/// Flash read: `12 09 06 <A_lo> <A_mid> <A_hi> <count>`.
pub fn frame_flash_read(addr: u32, count: u8) -> Vec<u8> {
    vec![
        0x12,
        0x09,
        0x06,
        addr as u8,
        (addr >> 8) as u8,
        (addr >> 16) as u8,
        count,
    ]
}

/// Flash erase: `12 09 07 06 <A_lo> <A_mid> <A_hi> <count>`.
pub fn frame_flash_erase(addr: u32, count: u8) -> Vec<u8> {
    vec![
        0x12,
        0x09,
        0x07,
        0x06,
        addr as u8,
        (addr >> 8) as u8,
        (addr >> 16) as u8,
        count,
    ]
}

/// Flash write block: `12 09 07 02 <A_lo> <A_mid> <A_hi> <data...>`.
pub fn frame_flash_write(addr: u32, data: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(7 + data.len());
    p.push(0x12);
    p.push(0x09);
    p.push(0x07);
    p.push(0x02);
    p.push(addr as u8);
    p.push((addr >> 8) as u8);
    p.push((addr >> 16) as u8);
    p.extend_from_slice(data);
    p
}

/// Flash write end: `12 09 07 0F <A_lo> <A_mid> <A_hi> <00>`.
pub fn frame_flash_write_end(addr: u32) -> Vec<u8> {
    vec![
        0x12,
        0x09,
        0x07,
        0x0F,
        addr as u8,
        (addr >> 8) as u8,
        (addr >> 16) as u8,
        0x00,
    ]
}

/// Fault clear (K-line BAP): `12 09 07 0E 42 41 50 00`.
pub fn frame_fault_clear() -> Vec<u8> {
    vec![0x12, 0x09, 0x07, 0x0E, 0x42, 0x41, 0x50, 0x00]
}

/// MW live data (selectable): `12 05 0B 10 <id_lo> <id_hi> ...`.
pub fn frame_mw_select(ids: &[u16]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + ids.len() * 2);
    p.push(0x12);
    p.push(0x05);
    p.push(0x0B);
    p.push(0x10);
    for &id in ids {
        p.push((id >> 8) as u8);
        p.push(id as u8);
    }
    p
}

/// MW live data (fixed block): `12 05 0B 03`.
pub fn frame_mw_status() -> Vec<u8> {
    vec![0x12, 0x05, 0x0B, 0x03]
}

// ---------------------------------------------------------------------------
// Fault memory (DTC) — DS2 frames extracted from the FS_* jobs in
// DDE30DS0.prg (see `tools/ds_frames.md`, `tools/disasm.py`). The German
// texts and freeze scaling live in `crate::dtc` (generated from the same
// .prg). Wire layouts are derived from the disassembly and are pending
// confirmation against real hardware.
// ---------------------------------------------------------------------------

/// One stored fault: location (ORT), fault type (ART) and raw freeze values.
#[derive(Clone, Debug, Default)]
pub struct Dtc {
    pub ort: u8,
    pub art: u8,
    /// Raw freeze values from the entry reply (big-endian u16 pairs),
    /// scaled by the ORT row's UW_1/UW_2 ids (see `crate::dtc::ort_uw_ids`).
    pub uw: Vec<u16>,
    /// Raw shadow (freeze-frame) values (big-endian u16 pairs), one per
    /// FUmweltTexte row (see `crate::dtc::UMWELT`).
    pub shadow: Vec<u16>,
}

impl Dtc {
    /// Human-readable description using the text tables in `crate::dtc`.
    pub fn describe(&self) -> String {
        let (u1, u2) = crate::dtc::ort_uw_ids(self.ort);
        let mut s = format!(
            "ORT 0x{:02X} {}   |   ART 0x{:02X} {}",
            self.ort,
            crate::dtc::ort_text(self.ort),
            self.art,
            crate::dtc::fart_text(self.art)
        );
        for (i, raw) in self.uw.iter().enumerate() {
            let id = match i {
                0 => u1,
                1 => u2,
                _ => 0,
            };
            match crate::dtc::uw_info(id) {
                Some(u) => {
                    s.push_str(&format!(
                        "\n      {} = {:.2} {}",
                        u.name.trim(),
                        crate::dtc::uw_value(id, *raw),
                        u.unit
                    ));
                }
                None => s.push_str(&format!("\n      UW{} raw 0x{:04X}", i + 1, raw)),
            }
        }
        if !self.shadow.is_empty() {
            s.push_str("\n      freeze frame:");
            for (i, raw) in self.shadow.iter().enumerate() {
                let id = crate::dtc::UMWELT.get(i).map(|r| r.0).unwrap_or(0xff);
                match crate::dtc::uw_info(id) {
                    Some(u) if u.id != 0x00 && u.id != 0xff => {
                        s.push_str(&format!(
                            " {} {:.2} {} (raw {})",
                            u.name.trim(),
                            crate::dtc::uw_value(id, *raw),
                            u.unit,
                            raw
                        ));
                    }
                    _ => s.push_str(&format!(" [{}] 0x{:04X}", i, raw)),
                }
            }
        }
        s
    }
}

/// Fault-memory count/start (FS_QUICK_LESEN / FS_LESEN): `12 05 14 01`.
/// Reply: `92 05 A0 <anz>`.
pub fn frame_dtc_count() -> Vec<u8> {
    vec![0x12, 0x05, 0x14, 0x01]
}

/// Read the next fault entry (FS_LESEN): `12 05 04 00`.
/// Reply: `92 05 A0 <ort> <art> <uw1:2> <uw2:2>`.
pub fn frame_dtc_entry() -> Vec<u8> {
    vec![0x12, 0x05, 0x04, 0x00]
}

/// Read the shadow (freeze frame) of the current fault (FS_SHADOW_LESEN):
/// `12 05 04 01`. Reply: `92 05 A0 <u16 BE ...>` (one per FUmweltTexte row).
pub fn frame_dtc_shadow() -> Vec<u8> {
    vec![0x12, 0x05, 0x04, 0x01]
}

/// DS2 fault-memory clear (FS_LOESCHEN): `12 04 05`. Reply: `92 04 A0`.
pub fn frame_dtc_clear() -> Vec<u8> {
    vec![0x12, 0x04, 0x05]
}

// ---------------------------------------------------------------------------
// K-line connection
// ---------------------------------------------------------------------------

/// Max BUSY (0xA1) resends before giving up.
const MAX_BUSY_RETRIES: u32 = 50;

pub struct Kline {
    port: Box<dyn SerialPort>,
    path: String,
    parser: FrameParser,
    pending: VecDeque<Frame>,
    last_sent: Option<Frame>,
    logs: Vec<String>,
    echo: bool,
    silence: Duration,
    write_delay: Duration,
    sim: Option<SimEcu>,
}

impl Kline {
    pub fn open(path: &str, baud: u32, echo: bool) -> Result<Self> {
        let port = serialport::new(path, baud)
            .data_bits(DataBits::Eight)
            .parity(Parity::Even)
            .stop_bits(StopBits::One)
            .timeout(Duration::from_millis(10))
            .open()
            .with_context(|| format!("cannot open {path}"))?;
        Ok(Self {
            port,
            path: path.to_string(),
            parser: FrameParser::default(),
            pending: VecDeque::new(),
            last_sent: None,
            logs: Vec::new(),
            echo,
            silence: Duration::from_millis(10),
            write_delay: Duration::from_millis(3),
            sim: None,
        })
    }

    /// Enable the in-process ECU simulator (see `src/sim.rs`). The real
    /// frame is still transmitted on the wire and the bus echo is verified;
    /// the ECU reply cannot come from the line because the monitor adapter
    /// is a passive tap that cannot drive the bus.
    pub fn set_sim(&mut self, sim: SimEcu) {
        self.sim = Some(sim);
    }

    pub fn sim_enabled(&self) -> bool {
        self.sim.is_some()
    }

    pub fn set_silence(&mut self, ms: u64) {
        self.silence = Duration::from_millis(ms);
    }

    pub fn set_write_delay(&mut self, ms: u64) {
        self.write_delay = Duration::from_millis(ms);
    }

    pub fn set_data_delay(&mut self, ms: u64) {
        self.write_delay = Duration::from_millis(ms);
    }

    pub fn set_baud(&mut self, baud: u32) -> Result<()> {
        self.port
            .set_baud_rate(baud)
            .with_context(|| format!("cannot set {} to {baud} baud", self.path))?;
        let _ = self.port.clear(ClearBuffer::All);
        thread::sleep(Duration::from_millis(100));
        self.parser = FrameParser::default();
        self.pending.clear();
        Ok(())
    }

    pub fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    fn log(&mut self, line: String) {
        self.logs.push(line);
    }

    fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        for (i, b) in frame.iter().enumerate() {
            self.port
                .write_all(&[*b])
                .with_context(|| format!("serial write failed on {} at byte {i}", self.path))?;
            self.port
                .flush()
                .with_context(|| format!("serial flush failed on {}", self.path))?;
            if !self.write_delay.is_zero() {
                thread::sleep(self.write_delay);
            }
        }
        Ok(())
    }

    fn read_chunk(&mut self) -> Result<Vec<u8>> {
        let start = Instant::now();
        let mut last = Instant::now();
        let mut out = Vec::new();
        while start.elapsed() < Duration::from_millis(250) {
            let mut buf = [0u8; 512];
            match self.port.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    out.extend_from_slice(&buf[..n]);
                    last = Instant::now();
                }
                Err(e) if e.kind() == ErrorKind::TimedOut => {}
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("serial read failed on {}", self.path))
                }
            }
            if !out.is_empty() && last.elapsed() >= self.silence {
                break;
            }
        }
        Ok(out)
    }

    /// Send a DS2 payload and wait for the reply payload.
    ///
    /// Reply handling: byte [2] is interpreted as the JOBRESULT status byte
    /// when it is a known status (0xA0/0xA1/0xA2/0xB0–0xB2/0xFF/0x00) —
    /// OKAY returns the reply, BUSY resends the request, anything else is an
    /// NRC. Any other byte at [2] means a data reply (e.g. flash read, MW
    /// live) and is returned as-is.
    pub fn request(&mut self, payload: &[u8], timeout: Duration) -> Result<Vec<u8>> {
        let frame = build_frame(payload);
        let sent = Frame {
            payload: payload.to_vec(),
        };
        let sid = payload.get(2).copied().unwrap_or(0);
        let mut busy = 0u32;

        loop {
            self.last_sent = Some(sent.clone());
            // Drop stale state from the previous transaction (e.g. the bus
            // echo that was consumed after its response).
            self.parser.clear();
            self.pending.clear();
            self.log(format!(
                "TX {}{}",
                hex(&frame),
                if busy == 0 { String::new() } else { format!("  (BUSY retry {busy})") }
            ));
            self.write_frame(&frame)?;

            // In-process ECU simulator: the reply cannot come from the wire
            // (the monitor adapter is a passive tap), so inject it after a
            // short pause that lets the bus echo land first.
            if let Some(sim) = self.sim.as_mut() {
                let r = sim.handle(payload);
                thread::sleep(Duration::from_millis(10));
                self.pending.push_back(Frame { payload: r.clone() });
                self.log(format!("SIM {}", hex(&r)));
            }

            let deadline = Instant::now() + timeout;
            let mut res: Option<Result<Vec<u8>>> = None;
            'recv: while res.is_none() {
                while let Some(f) = self.pending.pop_front() {
                    // Skip echo of our own transmission.
                    if f.payload == sent.payload {
                        continue;
                    }
                    self.log(format!("RX {}", hex(&f.payload)));
                    if f.payload.first() == Some(&(ECU_ADDR | 0x80)) {
                        match status_byte(&f.payload) {
                            Some(sb) if known_status_sb(sb) => {
                                match JobStatus::from_sb(sb, sid) {
                                    JobStatus::Okay => res = Some(Ok(f.payload)),
                                    JobStatus::Busy => {
                                        if busy >= MAX_BUSY_RETRIES {
                                            bail!("ECU BUSY for {MAX_BUSY_RETRIES} retries to service 0x{sid:02X}");
                                        }
                                        busy += 1;
                                        thread::sleep(Duration::from_millis(50));
                                        break 'recv; // resend in the outer loop
                                    }
                                    other => {
                                        res = Some(Err(Nrc { sid, code: sb }.into()));
                                        break;
                                    }
                                }
                            }
                            _ => res = Some(Ok(f.payload)), // data reply
                        }
                    } else {
                        res = Some(Ok(f.payload));
                    }
                }
                if res.is_some() {
                    break;
                }

                if Instant::now() > deadline {
                    bail!(
                        "no response to service 0x{sid:02X} ({}), link {}",
                        hex(payload),
                        self.path
                    );
                }
                let chunk = self.read_chunk()?;
                if chunk.is_empty() {
                    continue;
                }
                self.parser.feed(&chunk);
                if let Some(f) = self.parser.take_frame() {
                    self.pending.push_back(f);
                }
            }

            if res.is_none() {
                continue; // BUSY: resend the request
            }
            let reply = res.unwrap()?;
            // With the sim active, confirm the TX actually reached the bus
            // (echo visible on our own RX) before returning.
            if self.sim.is_some() {
                if self.verify_echo(&sent) {
                    self.log("ECHO confirmed (TX verified on bus)".to_string());
                } else {
                    self.log("ECHO missing (TX not seen on bus)".to_string());
                }
            }
            return Ok(reply);
        }
    }

    /// Wait up to 300 ms for the echo of `sent` on the bus (sim mode only).
    fn verify_echo(&mut self, sent: &Frame) -> bool {
        let deadline = Instant::now() + Duration::from_millis(300);
        loop {
            while let Some(f) = self.pending.pop_front() {
                if f.payload == sent.payload {
                    return true;
                }
            }
            if Instant::now() > deadline {
                return false;
            }
            let chunk = match self.read_chunk() {
                Ok(c) => c,
                Err(_) => return false,
            };
            if chunk.is_empty() {
                continue;
            }
            self.parser.feed(&chunk);
            while let Some(f) = self.parser.take_frame() {
                self.pending.push_back(f);
            }
        }
    }

    /// Send `12 04 00` ident and return the reply payload.
    pub fn identify(&mut self) -> Result<Vec<u8>> {
        let r = self.request(&[0x12, 0x04, 0x00], Duration::from_millis(500))?;
        Ok(r)
    }

    /// Best-effort return to the 9600 application link. Returns true when the ECU
    /// answers ident normally again.
    pub fn recover_link(&mut self) -> bool {
        self.recover_link_for(Duration::from_secs(8))
    }

    /// Like [`recover_link`], but waits up to `timeout` for a slow application boot.
    pub fn recover_link_for(&mut self, timeout: Duration) -> bool {
        let _ = self.port.set_baud_rate(9600);
        let _ = self.port.clear(ClearBuffer::All);
        thread::sleep(Duration::from_millis(100));
        self.parser.clear();
        self.pending.clear();
        let start = Instant::now();
        while start.elapsed() < timeout {
            if matches!(
                self.request(&[0x12, 0x04, 0x00], Duration::from_millis(800)),
                Ok(r) if ident_valid(&r)
            ) {
                return true;
            }
            thread::sleep(Duration::from_millis(500));
        }
        false
    }

    /// Read application data using STATUS_CODIER_CHECKSUMME (`12 04 0A`) as the
    /// closest DDE3 equivalent to DDE4's `read_app`. Returns the reply payload.
    pub fn read_app(&mut self, _off: usize, _len: usize) -> Result<Vec<u8>> {
        // DDE3 has no pointer-table read equivalent. Use checksum status to probe
        // the link; return its payload as a stand-in for software version bytes.
        match self.request(&[0x12, 0x04, 0x0A], Duration::from_millis(500)) {
            Ok(r) => Ok(r),
            Err(e) => Err(e),
        }
    }

    /// Read the full 256 KB flash image using `12 09 06 <addr> <count>` frames.
    pub fn read_full(
        &mut self,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(IMAGE_SIZE);
        let mut addr: u32 = 0;
        while addr < IMAGE_SIZE as u32 {
            let count = 0x80u32.min(IMAGE_SIZE as u32 - addr) as u8;
            let req = frame_flash_read(addr, count);
            let r = self.request(&req, Duration::from_secs(5))?;
            check_flash_read_reply(&r, count, addr)?;
            let data_start = 3;
            out.extend_from_slice(&r[data_start..data_start + count as usize]);
            addr += count as u32;
            progress(out.len(), IMAGE_SIZE);
        }
        Ok(out)
    }

    /// Read a range of flash using `12 09 06 <addr> <count>` frames.
    pub fn read_range(
        &mut self,
        start: usize,
        length: usize,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<u8>> {
        if start.checked_add(length).map_or(true, |end| end > IMAGE_SIZE) {
            bail!("range outside 256 KB image");
        }
        let mut out = Vec::with_capacity(length);
        let mut addr = start as u32;
        let end = (start + length) as u32;
        while addr < end {
            let count = 0x80u32.min(end - addr) as u8;
            let req = frame_flash_read(addr, count);
            let r = self.request(&req, Duration::from_secs(5))?;
            check_flash_read_reply(&r, count, addr)?;
            let data_start = 3;
            out.extend_from_slice(&r[data_start..data_start + count as usize]);
            addr += count as u32;
            progress(out.len(), length);
        }
        Ok(out)
    }

    /// Erase flash: sends `12 09 07 06 <addr> <count>`.
    pub fn erase_flash(&mut self, addr: u32, count: u8) -> Result<()> {
        let req = frame_flash_erase(addr, count);
        self.request(&req, Duration::from_secs(60))?;
        Ok(())
    }

    /// Write a block of data: sends `12 09 07 02 <addr> <data...>`.
    pub fn write_block(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        let req = frame_flash_write(addr, data);
        self.request(&req, Duration::from_secs(5))?;
        Ok(())
    }

    /// End a write sequence: sends `12 09 07 0F <addr> <00>`.
    pub fn write_end(&mut self, addr: u32) -> Result<()> {
        let req = frame_flash_write_end(addr);
        self.request(&req, Duration::from_secs(10))?;
        Ok(())
    }

    /// Full flash write: erase → write blocks → end.
    pub fn write_full(
        &mut self,
        image: &[u8],
        _baud: u32,
        data_delay_ms: u64,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<()> {
        if image.len() != IMAGE_SIZE {
            bail!("image must be exactly 256 KB");
        }
        self.set_write_delay(data_delay_ms);
        self.erase_flash(0x00000, 0x00)?;
        thread::sleep(Duration::from_millis(100));
        let mut done = 0usize;
        for start in (0..IMAGE_SIZE).step_by(0x80) {
            let len = 0x80.min(IMAGE_SIZE - start);
            self.write_block(start as u32, &image[start..start + len])?;
            done += len;
            progress(done, IMAGE_SIZE);
        }
        self.write_end(0x00000)?;
        Ok(())
    }

    /// Calibration flash write: erase → write blocks → end.
    pub fn write_calibration(
        &mut self,
        image: &[u8],
        _baud: u32,
        data_delay_ms: u64,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<()> {
        if image.len() != CAL_LEN {
            bail!("calibration image must be exactly 32 KB");
        }
        self.set_write_delay(data_delay_ms);
        self.erase_flash(CAL_START as u32, 0x00)?;
        thread::sleep(Duration::from_millis(100));
        let mut done = 0usize;
        for start in (0..CAL_LEN).step_by(0x80) {
            let len = 0x80.min(CAL_LEN - start);
            self.write_block(CAL_START as u32 + start as u32, &image[start..start + len])?;
            done += len;
            progress(done, CAL_LEN);
        }
        self.write_end(CAL_START as u32)?;
        Ok(())
    }

    /// Read the full DS2 fault memory (FS_LESEN sequence).
    ///
    /// `12 05 14 01` returns the count; for each fault `12 05 04 00` returns
    /// the entry and `12 05 04 01` the shadow/freeze frame (the ECU's read
    /// pointer advances with each request, as the FS_LESEN disassembly
    /// shows).
    pub fn read_dtcs(&mut self) -> Result<Vec<Dtc>> {
        let r = self.request(&frame_dtc_count(), Duration::from_secs(5))?;
        if r.first() != Some(&(ECU_ADDR | 0x80)) || r.get(2) != Some(&0xA0) {
            bail!("unexpected fault-count reply: {}", hex(&r));
        }
        let count = *r.get(3).unwrap_or(&0);
        let mut out = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let e = self.request(&frame_dtc_entry(), Duration::from_secs(5))?;
            if e.first() != Some(&(ECU_ADDR | 0x80)) || e.get(2) != Some(&0xA0) {
                bail!("unexpected fault-entry reply: {}", hex(&e));
            }
            let d = &e[3..];
            if d.len() < 2 {
                bail!("short fault-entry reply: {}", hex(&e));
            }
            let mut dtc = Dtc {
                ort: d[0],
                art: d[1],
                uw: Vec::new(),
                shadow: Vec::new(),
            };
            for pair in d[2..].chunks_exact(2) {
                dtc.uw.push(u16::from_be_bytes([pair[0], pair[1]]));
            }
            let s = self.request(&frame_dtc_shadow(), Duration::from_secs(5))?;
            if s.first() != Some(&(ECU_ADDR | 0x80)) || s.get(2) != Some(&0xA0) {
                bail!("unexpected fault-shadow reply: {}", hex(&s));
            }
            for pair in s[3..].chunks_exact(2) {
                dtc.shadow.push(u16::from_be_bytes([pair[0], pair[1]]));
            }
            out.push(dtc);
        }
        Ok(out)
    }

    /// Clear the DS2 fault memory (FS_LOESCHEN): `12 04 05`.
    pub fn clear_faults_ds2(&mut self) -> Result<()> {
        let r = self.request(&frame_dtc_clear(), Duration::from_secs(5))?;
        if r.first() != Some(&(ECU_ADDR | 0x80)) || r.get(2) != Some(&0xA0) {
            bail!("unexpected fault-clear reply: {}", hex(&r));
        }
        Ok(())
    }

    /// Live data polling via MW_SELECT_LESEN (`12 05 0B 10 <id bytes>`).
    ///
    /// ⚠️ UNRESOLVED: The exact response layout for measurement values is not yet
    /// confirmed against real hardware. This implementation sends the request and
    /// parses big-endian u16 pairs starting at response offset 5.
    pub fn live_values(
        &mut self,
        addrs: &[(u8, u8)],
        _redefine: bool,
    ) -> Result<Vec<u16>> {
        let ids: Vec<u16> = addrs
            .iter()
            .map(|(hi, lo)| ((*hi as u16) << 8) | (*lo as u16))
            .collect();
        let req = frame_mw_select(&ids);
        let r = self.request(&req, Duration::from_millis(800))?;
        if r.len() < 6 || r[0] != 0x92 || r[4] != 0xA0 {
            bail!(
                "unexpected MW live reply: status={:02X} ({})",
                r.get(4).copied().unwrap_or(0),
                hex(&r)
            );
        }
        let mut out = Vec::new();
        for pair in r[5..].chunks_exact(2) {
            out.push(u16::from_be_bytes([pair[0], pair[1]]));
        }
        Ok(out)
    }
}

/// A healthy `92 04` reply starts with `0x92` (ECU_ADDR|0x80) and carries data.
pub fn ident_valid(reply: &[u8]) -> bool {
    reply.first() == Some(&(ECU_ADDR | 0x80)) && reply.len() >= 3
}

/// Validate a flash-read reply: `92 <len> A0 <data...>` — status byte at
/// [2], data from [3] (per the decompiled FLASH_LESEN: `move B0,S1[2]` for
/// the status, `move S4,S1[3]L1` for the data).
fn check_flash_read_reply(r: &[u8], count: u8, addr: u32) -> Result<()> {
    if r.first() != Some(&(ECU_ADDR | 0x80)) || r.get(2) != Some(&0xA0) {
        bail!("unexpected flash read reply at 0x{addr:06X}: {}", hex(r));
    }
    if r.len() < 3 + count as usize {
        bail!("short flash read reply at 0x{addr:06X}: {}", hex(r));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        // Confirmed wire format: payload + trailing XOR, no B8/F1/LEN header.
        let bytes = build_frame(&[0x12, 0x04, 0x0D]);
        assert_eq!(bytes, vec![0x12, 0x04, 0x0D, 0x1B]);
        let mut parser = FrameParser::default();
        parser.feed(&bytes);
        let f = parser.take_frame().expect("frame");
        assert_eq!(f.payload, vec![0x12, 0x04, 0x0D]);
    }

    #[test]
    fn captured_winkfp_frames() {
        // Byte-exact frames from the 2026-11-09 passive capture of WinKFP.
        assert_eq!(build_frame(&[0x12, 0x04, 0x0D]), vec![0x12, 0x04, 0x0D, 0x1B]);
        assert_eq!(build_frame(&[0x12, 0x04, 0x00]), vec![0x12, 0x04, 0x00, 0x16]);
        assert_eq!(
            build_frame(&[0x12, 0x05, 0x90, 0x56]),
            vec![0x12, 0x05, 0x90, 0x56, 0xD1]
        );
    }

    #[test]
    fn parser_keeps_partial_frame_across_reads() {
        // WinKFP sends the trailing XOR ~50 ms after the payload.
        let mut parser = FrameParser::default();
        parser.feed(&[0x12, 0x04, 0x00]);
        assert!(parser.take_frame().is_none());
        parser.feed(&[0x16]);
        let f = parser.take_frame().expect("frame");
        assert_eq!(f.payload, vec![0x12, 0x04, 0x00]);
    }

    #[test]
    fn parser_skips_garbage_and_resyncs() {
        let mut parser = FrameParser::default();
        parser.feed(&[0x00, 0xFF]);
        assert!(parser.take_frame().is_none());
        parser.feed(&build_frame(&[0x12, 0x04, 0x00]));
        let f = parser.take_frame().expect("frame");
        assert_eq!(f.payload, vec![0x12, 0x04, 0x00]);
    }

    #[test]
    fn fixed_frames_match_vm_extraction() {
        // UPROG_EIN: 12 04 0D (frames.md @0x308)
        assert_eq!(frame_uprog_ein(), vec![0x12, 0x04, 0x0D]);
        // IDENT: 12 04 00 (frames.md @0xf3d5)
        assert_eq!(frame_ident(), vec![0x12, 0x04, 0x00]);
        // Checksum status: 12 04 0A (frames.md @0xc0a1)
        assert_eq!(frame_checksum_status(), vec![0x12, 0x04, 0x0A]);
        // DIAGNOSE_ENDE: 12 04 9F (frames.md @0x106dc)
        assert_eq!(frame_diagnose_ende(), vec![0x12, 0x04, 0x9F]);
        // SEED_KEY: 12 08 90 42 4D 57 05 <Z> (HANDOVER §15.3)
        assert_eq!(
            frame_seed_key(0x05),
            vec![0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, 0x05, 0x05]
        );
        // Fault clear: 12 09 07 0E 42 41 50 00 (frames.md @0xe55e)
        assert_eq!(
            frame_fault_clear(),
            vec![0x12, 0x09, 0x07, 0x0E, 0x42, 0x41, 0x50, 0x00]
        );
    }

    #[test]
    fn parameterized_frames_match_vm_extraction() {
        // FLASH_LESEN {1: 0x38000, 2: 0x80} -> 12 09 06 00 80 03 80 (frames.md)
        assert_eq!(
            frame_flash_read(0x38000, 0x80),
            vec![0x12, 0x09, 0x06, 0x00, 0x80, 0x03, 0x80]
        );
        // FLASH_LOESCHEN -> 12 09 07 06 00 00 00 00 (frames.md @0xcaea)
        assert_eq!(
            frame_flash_erase(0x000000, 0x00),
            vec![0x12, 0x09, 0x07, 0x06, 0x00, 0x00, 0x00, 0x00]
        );
        // FLASH_SCHREIBEN with one data byte -> 12 09 07 02 00 00 00 00 (frames.md @0xd379)
        // (empty data produces 7 bytes; the 8th byte is patched from the response)
        assert_eq!(
            frame_flash_write(0x000000, &[0x00]),
            vec![0x12, 0x09, 0x07, 0x02, 0x00, 0x00, 0x00, 0x00]
        );
        // FLASH_SCHREIBEN_ENDE -> 12 09 07 0F 00 00 00 00 (frames.md @0xda85)
        assert_eq!(
            frame_flash_write_end(0x000000),
            vec![0x12, 0x09, 0x07, 0x0F, 0x00, 0x00, 0x00, 0x00]
        );
    }

    #[test]
    fn dtc_frames_match_vm_extraction() {
        // FS_QUICK_LESEN start: 12 05 14 01 (ds_frames.md @0x88b8)
        assert_eq!(frame_dtc_count(), vec![0x12, 0x05, 0x14, 0x01]);
        // FS_LESEN entry: 12 05 04 00 (ds_frames.md @0x9935)
        assert_eq!(frame_dtc_entry(), vec![0x12, 0x05, 0x04, 0x00]);
        // FS_SHADOW_LESEN: 12 05 04 01 (ds_frames.md @0x136b0)
        assert_eq!(frame_dtc_shadow(), vec![0x12, 0x05, 0x04, 0x01]);
        // FS_LOESCHEN: 12 04 05 (ds_frames.md @0x1354e)
        assert_eq!(frame_dtc_clear(), vec![0x12, 0x04, 0x05]);
    }

    #[test]
    fn dtc_describe_uses_text_tables() {
        let d = Dtc {
            ort: 0x05,
            art: 0x01,
            uw: vec![100, 200],
            shadow: vec![],
        };
        let s = d.describe();
        assert!(s.contains("Nadelbewegungsfuehler"), "ORT text: {s}");
        assert!(s.contains("Signal Kurzschluss nach B+"), "ART text: {s}");
    }

    #[test]
    fn jobstatus_lookup() {
        assert!(JobStatus::from_sb(0xA0, 0x0D).is_okay());
        assert_eq!(JobStatus::from_sb(0xA0, 0x0D).as_str(), "OKAY");
        assert!(!JobStatus::from_sb(0xA1, 0x0D).is_okay());
        assert_eq!(JobStatus::from_sb(0xFF, 0x0D).as_str(), "ERROR_SG_NACK");
        assert!(matches!(
            JobStatus::from_sb(0xAA, 0x0D),
            JobStatus::ErrorSgUnknown(_)
        ));
    }
}
