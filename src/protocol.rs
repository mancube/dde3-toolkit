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

/// ECU reply convention (**2026-11-09 real M47 capture**, `dde3 toolkit
/// log.txt`): the ECU answers with its OWN address `0x12` — NOT the DS2
/// `0x92` response prefix the toolkit first assumed (a WinKFP `frames.md`
/// assumption, wrong for this ECU) — the JOBRESULT status byte at offset 2,
/// data from offset 3:
///
/// ```text
/// <12> <len> <SB> [<data ...>]  + trailing XOR on the wire
/// ```
///
/// MW live data (`0B 10`) replies `12 <len> A0 10 00 <u16 BE per id ...>`
/// (data from offset 5); IDENT replies `12 <len> A0 <ascii ident string>` —
/// same envelope as everything else (2026-11-10 real-car capture: `12 2E A0
/// 37 37 38 36 …`, see [`Kline::identify`]). The ECU also emits a periodic
/// ~16-byte live block (`12 10 A0 10 …`) on the K-line that interleaves with
/// request/reply — see [`is_not_live_broadcast`].
/// [`request`] treats byte [2] as the JOBRESULT status byte when it is a
/// known status (A0/A1/A2/B0–B2/FF/00): OKAY → Ok, BUSY → retry, else NRC.
/// Any other byte at [2] means a data reply and is returned as-is.
pub fn status_byte(reply: &[u8]) -> Option<u8> {
    reply.get(2).copied()
}

pub fn known_status_sb(sb: u8) -> bool {
    matches!(sb, 0xA0 | 0xA1 | 0xA2 | 0xB0 | 0xB1 | 0xB2 | 0xFF | 0x00)
}

/// Maximum plausible buffer before resync. Must comfortably hold an echo
/// (≤ ~40 B) plus a full reply (≤ ~130 B) that arrive back-to-back on the
/// half-duplex K-line.
const MAX_BUF: usize = 192;

#[derive(Default)]
pub struct FrameParser {
    buf: Vec<u8>,
}

impl FrameParser {
    /// Accumulate bytes. Nothing is emitted until [`take_frame`] succeeds, so
    /// frames that arrive across several reads are handled correctly.
    pub fn feed(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        if self.buf.len() > MAX_BUF {
            // Last-resort resync for persistent garbage: keep the tail half.
            self.buf.drain(0..self.buf.len() - MAX_BUF / 2);
        }
    }

    /// Extract the next complete frame from the front of the buffer.
    ///
    /// A frame is a run of bytes whose final byte equals the XOR of the
    /// preceding bytes. Three cases, in order:
    ///  1. the buffer starts with the ECU address (0x12): the frame is the
    ///     shortest XOR-terminated prefix — this is what splits our TX echo
    ///     from the ECU reply (and any interleaved periodic live block) that
    ///     follow each other back-to-back on the line;
    ///  2. otherwise, skip leading garbage until a 0x12-anchored frame
    ///     appears;
    ///  3. otherwise the whole silence-delimited buffer is one raw reply
    ///     (e.g. the IDENT string, which carries no 0x12 anchor).
    ///
    /// Returns the payload (trailing XOR byte stripped), or `None` when no
    /// complete frame is present yet — the partial frame stays buffered.
    pub fn take_frame(&mut self) -> Option<Frame> {
        // 1) Envelope frame anchored at the front.
        if self.buf.first() == Some(&ECU_ADDR) {
            if let Some(k) = xor_end(&self.buf) {
                let payload = self.buf[..k].to_vec();
                self.buf.drain(0..=k);
                return Some(Frame { payload });
            }
        }
        // 2) Skip leading garbage until a 0x12-anchored frame appears.
        for i in 1..self.buf.len() {
            if self.buf[i] != ECU_ADDR {
                continue;
            }
            if let Some(k) = xor_end(&self.buf[i..]) {
                let payload = self.buf[i..i + k].to_vec();
                self.buf.drain(0..i + k + 1);
                return Some(Frame { payload });
            }
        }
        // 3) No 0x12 anchor: a raw reply is the whole buffer, XOR-terminated.
        if self.buf.len() >= 2
            && xor_cs(&self.buf[..self.buf.len() - 1]) == self.buf[self.buf.len() - 1]
        {
            let payload = self.buf[..self.buf.len() - 1].to_vec();
            self.buf.clear();
            return Some(Frame { payload });
        }
        None
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

/// Index of a frame's trailing XOR byte: the shortest `k >= 2` with
/// `xor(s[..k]) == s[k]`. Returns `None` when no such position exists (the
/// frame is incomplete).
fn xor_end(s: &[u8]) -> Option<usize> {
    for k in 2..s.len() {
        if xor_cs(&s[..k]) == s[k] {
            return Some(k);
        }
    }
    None
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

    #[allow(dead_code)]
    pub fn is_okay(self) -> bool {
        matches!(self, JobStatus::Okay)
    }

    #[allow(dead_code)]
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
#[allow(dead_code)]
pub fn frame_checksum_status() -> Vec<u8> {
    vec![0x12, 0x04, 0x0A]
}

/// Leave / end session: `12 04 9F`.
pub fn frame_diagnose_ende() -> Vec<u8> {
    vec![0x12, 0x04, 0x9F]
}

/// DS2 service 0x91 (baud rate change) as implemented by the stock tester job
/// `BAUDRATEN_UMSTELLUNG` (10DDE30.prg @ 0x121C):
/// `12 08 91 00 <baud 24-bit BE> <p2>`. Not implemented in stock firmware
/// (returns `12 04 FF`); works against the 0x91 patch (see
/// ~/Desktop/edc15m_out/baud91_patch/PATCH_0x91.md). The baud field is
/// honored only by patch variant V2; v1 always switches to 38.4 kbaud.
pub fn frame_baud(baud: u32, p2: u8) -> Vec<u8> {
    vec![
        0x12, 0x08, 0x91, 0x00,
        ((baud >> 16) & 0xFF) as u8,
        ((baud >> 8) & 0xFF) as u8,
        (baud & 0xFF) as u8,
        p2,
    ]
}

/// The baud the tester must reopen the port at after a `0x91` switch. The ECU's
/// S0BG divisor for a requested 125000 yields 124800 (−0.16 %, within tolerance);
/// 38400/19200 are exact. Pure fn so it is unit-testable without hardware.
pub fn reopen_divisor(requested: u32) -> u32 {
    match requested {
        125000 => 124_800,
        other => other,
    }
}

/// Seed/Key init: `12 08 90 42 4D 57 05 <Z>`.
pub fn frame_seed_key(z: u8) -> Vec<u8> {
    vec![0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, 0x05, z]
}

/// Flash read (FLASH_LESEN, service 0x06): `12 09 06 00 <A_hi A_mid A_lo> <count> [XOR]`.
///
/// Verified against the WinKFP `10DDE30.prg` job VM (over-range count probe) and
/// the job bytecode (`docs/FINDINGS_flash_and_fast_ds2.md`):
///   - LEN (`[1]`) = 0x09 = total wire bytes incl. the trailing XOR (8 payload + XOR);
///   - `[3]` = 0x00, a constant (the unused high byte of the address field);
///   - `[4..6]` = 24-bit address, **big-endian** (`[4]` = MSB);
///   - `[7]` = count, a **1-byte** byte-count (only the low byte reaches the wire).
///
/// The earlier builder was little-endian, omitted `[3]`, and kept LEN=0x09 for a
/// 7-byte payload — so the real ECU waited for a 9th byte and never replied
/// (confirmed: `captures/dde3newlog.txt` `12 09 06 00 80 03 80 …` → no response).
pub fn frame_flash_read(addr: u32, count: u8) -> Vec<u8> {
    vec![
        0x12,
        0x09,
        0x06,
        0x00,
        (addr >> 16) as u8,
        (addr >> 8) as u8,
        addr as u8,
        count,
    ]
}

/// Flash erase (FLASH_LOESCHEN, service 0x07 subfn 0x06):
/// `12 09 07 06 <A_hi A_mid A_lo> 00 [XOR]`. The frame carries no count — the
/// erased length comes back in the reply. (`_count` kept for call-site ABI.)
pub fn frame_flash_erase(addr: u32, _count: u8) -> Vec<u8> {
    vec![
        0x12,
        0x09,
        0x07,
        0x06,
        (addr >> 16) as u8,
        (addr >> 8) as u8,
        addr as u8,
        0x00,
    ]
}

/// Flash write block (FLASH_SCHREIBEN, service 0x07 subfn 0x02):
/// `12 <LEN> 07 02 <A_hi A_mid A_lo> <NN> <data…> [XOR]`, LEN = 0x09 + NN,
/// NN = data length (1-byte count), address big-endian.
pub fn frame_flash_write(addr: u32, data: &[u8]) -> Vec<u8> {
    // LEN = 0x09 + N must fit one byte, so N <= 0xF6. Callers chunk at 0x80.
    assert!(data.len() <= 0xF6, "flash write block must be <= 246 bytes (LEN=9+N)");
    let n = data.len() as u8;
    let mut p = Vec::with_capacity(8 + data.len());
    p.extend_from_slice(&[
        0x12,
        0x09 + n, // LEN = 9 + NN
        0x07,
        0x02,
        (addr >> 16) as u8,
        (addr >> 8) as u8,
        addr as u8,
        n, // data-length byte
    ]);
    p.extend_from_slice(data);
    p
}

/// Flash write end (FLASH_SCHREIBEN_ENDE, service 0x07 subfn 0x0F):
/// `12 09 07 0F <A_hi A_mid A_lo> 00 [XOR]`, address big-endian.
pub fn frame_flash_write_end(addr: u32) -> Vec<u8> {
    vec![
        0x12,
        0x09,
        0x07,
        0x0F,
        (addr >> 16) as u8,
        (addr >> 8) as u8,
        addr as u8,
        0x00,
    ]
}

/// Fault clear (K-line BAP): `12 09 07 0E 42 41 50 00`.
pub fn frame_fault_clear() -> Vec<u8> {
    vec![0x12, 0x09, 0x07, 0x0E, 0x42, 0x41, 0x50, 0x00]
}

/// Max MW ids per batched telegram. The DS2 length byte is capped at 0x80
/// (FINDINGS §2.1) so a `0B 10` frame holds at most (0x80-5)/2 = 61 ids.
/// 50 keeps a safety margin below the boundary.
pub const MW_BATCH_SIZE: usize = 50;

/// MW live data (selectable): `12 <LEN> 0B 10 <id_hi id_lo> ...`.
///
/// Batched form — this is exactly what the tested WinKFP logger job
/// `MW_SELECT_LESEN_NORM` sends (see `testo customjobs.txt` and the disasm at
/// `0x5FE5F`: the length byte is computed as `slen(arg)+5`, i.e. the 4-byte
/// prefix `12 LEN 0B 10` plus the packed id bytes plus the trailing XOR).
/// The length byte is the *total* wire byte count (DS2 rule, FINDINGS §2.1):
///
/// ```text
/// LEN = 4 (prefix) + 2·n (n ids) + 1 (XOR) = 5 + 2n
/// ```
///
/// A u8 length is capped at 0x80, so one telegram holds up to 118 ids — send
/// the whole selected live set in a single frame, not one id per telegram.
pub fn frame_mw_select(ids: &[u16]) -> Vec<u8> {
    if ids.len() > MW_BATCH_SIZE {
        panic!("MW select: too many ids in one telegram ({} > {})", ids.len(), MW_BATCH_SIZE);
    }
    let mut p = Vec::with_capacity(4 + ids.len() * 2);
    p.push(0x12);
    p.push((5 + ids.len() * 2) as u8);
    p.push(0x0B);
    p.push(0x10);
    for &id in ids {
        p.push((id >> 8) as u8);
        p.push(id as u8);
    }
    p
}


// ---------------------------------------------------------------------------
// Fault memory (DTC) — DS2 frames extracted from the FS_* jobs in
// DDE30DS0.prg (see `tools/ds_frames.md`, `tools/disasm.py`). The German
// texts and freeze scaling live in `crate::dtc` (generated from the same
// .prg). Wire layouts are derived from the disassembly and are pending
// confirmation against real hardware.
// ---------------------------------------------------------------------------

/// One stored fault: location (ORT), active fault types (ART), frequency and
/// raw freeze values — parsed from the 50-byte fault record the ECU streams
/// over the FS_LESEN sequence (see `read_dtcs` and HANDOVER §22).
#[derive(Clone, Debug, Default)]
pub struct Dtc {
    /// Fault location code, record byte 0 (ORT table).
    pub ort: u8,
    /// Primary FArt code (first active slot) — kept for compatibility.
    pub art: u8,
    /// ART slot mask byte, record byte 1 (bit i = slot A(i+1) active).
    pub art_mask: u8,
    /// Error frequency (Fehlerfrequenz), record byte 2.
    pub freq: u8,
    /// FArt codes of all active slots, in slot order (see `dtc::active_arts`).
    pub arts: Vec<u8>,
    /// Raw freeze values, record bytes 3..11 as big-endian u16 pairs
    /// (scaled by the ORT row's UW_1/UW_2 ids, see `crate::dtc::ort_uw_ids`).
    pub uw: Vec<u16>,
    /// Legacy field, unused by the DS2 FS_LESEN flow.
    pub shadow: Vec<u16>,
    /// The full raw fault record as received (50 bytes when read completely).
    pub record: Vec<u8>,
}

impl Dtc {
    /// Parse a fault from the assembled raw record.
    ///
    /// Layout (verified against the M47 reference report):
    ///   [0]=ORT  [1]=ART slot mask  [2]=error frequency  [3..11]=freeze u16 BE
    pub fn from_record(rec: &[u8]) -> Dtc {
        let ort = rec.first().copied().unwrap_or(0);
        let mask = rec.get(1).copied().unwrap_or(0);
        let freq = rec.get(2).copied().unwrap_or(0);
        let arts = crate::dtc::active_arts(ort, mask);
        let uw: Vec<u16> = rec
            .get(3..11)
            .unwrap_or(&[])
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_be_bytes(*p))
            .collect();
        Dtc {
            ort,
            art: arts.first().copied().unwrap_or(0),
            art_mask: mask,
            freq,
            arts,
            uw,
            shadow: Vec::new(),
            record: rec.to_vec(),
        }
    }

    /// Human-readable description using the text tables in `crate::dtc`.
    pub fn describe(&self) -> String {
        let mut s = format!(
            "ORT 0x{:02X} {}   |   Fehlerfreq {}   |   ART mask 0x{:02X}",
            self.ort,
            crate::dtc::ort_text(self.ort),
            self.freq,
            self.art_mask
        );
        let legacy = [self.art];
        let codes: &[u8] = if self.arts.is_empty() { &legacy } else { &self.arts };
        if !(self.arts.is_empty() && self.art == 0) {
            for a in codes {
                s.push_str(&format!(
                    "\n      ART 0x{:02X} {}",
                    a,
                    crate::dtc::fart_text(*a)
                ));
            }
        }
        if !self.uw.is_empty() {
            s.push_str("\n      freeze (BE u16): ");
            for raw in &self.uw {
                s.push_str(&format!("{raw:04X} "));
            }
            s.pop();
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
        if !self.record.is_empty() {
            s.push_str("\n      record: ");
            for b in &self.record {
                s.push_str(&format!("{:02X} ", b));
            }
            s.pop();
        }
        s
    }
}

/// Fault-memory count/start (FS_QUICK_LESEN / FS_LESEN): `12 05 14 01`.
///
/// First reply: `12 L A0 <anz> <rec 0..22>` — the count plus the first 22
/// bytes of the current fault record. Subsequent replies: `12 L A0 <anz'>
/// <rec 37..50> [<next rec 0..22>]` — the 13-byte tail of the fault just
/// read plus, when more faults remain, the header of the next one.
pub fn frame_dtc_count() -> Vec<u8> {
    vec![0x12, 0x05, 0x14, 0x01]
}

/// Read the entry chunk of the current fault (FS_LESEN): `12 05 04 00`.
/// Reply: `12 L A0 <pos> <rec 22..26>` (4 bytes).
pub fn frame_dtc_entry() -> Vec<u8> {
    vec![0x12, 0x05, 0x04, 0x00]
}

/// Read the shadow chunk of the current fault (FS_LESEN): `12 05 04 01`.
/// Reply: `12 L A0 <pos> <rec 26..37>` (11 bytes).
pub fn frame_dtc_shadow() -> Vec<u8> {
    vec![0x12, 0x05, 0x04, 0x01]
}

/// DS2 fault-memory clear (FS_LOESCHEN): `12 04 05`. Reply: `12 04 A0`.
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
    logs: Vec<String>,
    silence: Duration,
    write_delay: Duration,
    sim: Option<SimEcu>,
    /// Wall clock of the last TX/RX log line — used to stamp every log line
    /// with an absolute time and the gap to the previous line, so a capture
    /// shows where time goes (TX pacing, ECU reply latency, silence waits).
    last_log: Option<Instant>,
}

impl Kline {
    pub fn open(path: &str, baud: u32) -> Result<Self> {
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
            logs: Vec::new(),
            silence: Duration::from_millis(10),
            write_delay: Duration::from_millis(3),
            sim: None,
            last_log: None,
        })
    }

    /// Enable the in-process ECU simulator (see `src/sim.rs`). The real
    /// frame is still transmitted on the wire and the bus echo is verified;
    /// the ECU reply cannot come from the line because the monitor adapter
    /// is a passive tap that cannot drive the bus.
    pub fn set_sim(&mut self, sim: SimEcu) {
        self.sim = Some(sim);
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

    /// Switch the DS2 K-line up via service `0x91` (needs the baud patch flashed —
    /// `../../patches/e46_baud{38400,125000}.bin`). Sends `12 08 91 00 <baud BE> 03`
    /// at `from_baud`, reopens the port at the ECU's actual divisor
    /// ([`reopen_divisor`]), drains the ACK (which arrives at the NEW rate — the
    /// patch writes S0BG before queuing it), and confirms with IDENT. On failure it
    /// falls back to `from_baud` to distinguish "never switched" (stock/unpatched)
    /// from "dead" (power-cycle). Returns the rate the port now runs at.
    ///
    /// No ECU unlock is needed: `0x91` hooks the dispatcher fall-through (0x293D0)
    /// with no session gate. Reply framing is unchanged at the new rate because the
    /// receiver completes a frame by byte-count (LEN), not by line-silence timing.
    pub fn switch_fast_baud(&mut self, requested: u32, from_baud: u32) -> Result<u32> {
        let reopen = reopen_divisor(requested);
        let frame = frame_baud(requested, 0x03);
        // No ACK at the old rate — only a short settle window.
        let _ = self.request(&frame, Duration::from_millis(100));
        thread::sleep(Duration::from_millis(100));
        self.set_baud(reopen)?;
        let drained = self.raw_read(256, Duration::from_millis(300))?;
        if !drained.is_empty() {
            self.log(format!("drained pending bytes @ {reopen}: {}", hex(&drained)));
        }
        let mut last_err = None;
        for _ in 0..3 {
            match self.request(&frame_ident(), Duration::from_millis(500)) {
                Ok(_) => return Ok(reopen),
                Err(e) => last_err = Some(e),
            }
            thread::sleep(Duration::from_millis(150));
        }
        // Did not answer at the new rate — is the ECU still at the old one?
        let _ = self.set_baud(from_baud);
        match self.request(&frame_ident(), Duration::from_millis(500)) {
            Ok(_) => bail!(
                "ECU still answers @ {from_baud} — service 0x91 not honored (stock firmware or baud patch not flashed)"
            ),
            Err(_) => bail!(
                "no reply @ {reopen} and silent @ {from_baud} — ECU unresponsive; power-cycle it ({})",
                last_err.map(|e| format!("{e}")).unwrap_or_default()
            ),
        }
    }

    /// Raw byte write (no DS2 framing / XOR) — for the break-entered service
    /// protocol (see `src/breakmode.rs`).
    pub fn raw_write(&mut self, bytes: &[u8]) -> Result<()> {
        self.port
            .write_all(bytes)
            .with_context(|| format!("raw write failed on {}", self.path))?;
        self.port
            .flush()
            .with_context(|| format!("raw flush failed on {}", self.path))?;
        Ok(())
    }

    /// Raw byte read (no DS2 framing) — up to `n` bytes or `timeout`.
    pub fn raw_read(&mut self, n: usize, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::with_capacity(n);
        let mut buf = [0u8; 256];
        while out.len() < n && Instant::now() < deadline {
            match self.port.read(&mut buf) {
                Ok(0) => {}
                Ok(k) => out.extend_from_slice(&buf[..k]),
                Err(e) if e.kind() == ErrorKind::TimedOut => {}
                Err(e) => return Err(e).with_context(|| "raw read failed"),
            }
        }
        Ok(out)
    }

    /// Hold the K-line low for a UART break (the break-entered protocol entry
    /// trigger, `src/breakmode.rs`).
    pub fn raw_break(&mut self) -> Result<()> {
        self.port.set_break()?;
        thread::sleep(Duration::from_millis(25));
        self.port.clear_break()?;
        self.port.flush()?;
        Ok(())
    }

    fn log(&mut self, line: String) {
        let now = Instant::now();
        let delta = self
            .last_log
            .map(|last| now.duration_since(last).as_secs_f64())
            .unwrap_or(0.0);
        self.last_log = Some(now);
        self.logs.push(format!("[+{delta:6.3}s] {line}"));
    }

    /// Transmit a full wire frame (payload + trailing XOR) with the
    /// configured per-byte pacing — the same path every request goes out on.
    /// Used by the `respond` command to send the simulated ECU replies on
    /// the real wire.
    pub fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
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
        self.do_request(payload, timeout, |_| true)
    }

    /// Like [`request`], but only a reply that satisfies `accept` is returned;
    /// other ECU frames (e.g. the periodic live broadcast) are logged and
    /// skipped. Needed for the small fault/battery replies, which share the
    /// K-line with the ECU's ~16-byte periodic live block.
    pub fn request_filtered<F>(&mut self, payload: &[u8], timeout: Duration, accept: F) -> Result<Vec<u8>>
    where
        F: Fn(&[u8]) -> bool,
    {
        self.do_request(payload, timeout, accept)
    }

    fn do_request<F>(&mut self, payload: &[u8], timeout: Duration, mut accept: F) -> Result<Vec<u8>>
    where
        F: FnMut(&[u8]) -> bool,
    {
        let frame = build_frame(payload);
        let sent = Frame {
            payload: payload.to_vec(),
        };
        let sid = payload.get(2).copied().unwrap_or(0);
        let mut busy = 0u32;

        loop {
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
                    if !accept(&f.payload) {
                        self.log(format!("SKIP (not the requested reply) {}", hex(&f.payload)));
                        continue;
                    }
                    if f.payload.first() == Some(&ECU_ADDR) {
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
                                    _ => {
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
                while let Some(f) = self.parser.take_frame() {
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

    /// Send `12 04 00` ident and return the ident **string** (envelope
    /// stripped, so the result is plain ASCII — see [`ident_valid`],
    /// [`parse_ident`]).
    ///
    /// The real ECU answers with the normal `12 <len> A0 <ascii…>` envelope
    /// (e.g. the captured `12 2E A0 37 37 38 36 …`); the in-process sim used
    /// to answer with a bare string, which is still accepted here for
    /// backwards compatibility.
    pub fn identify(&mut self) -> Result<Vec<u8>> {
        let r = self.request(&[0x12, 0x04, 0x00], Duration::from_millis(500))?;
        Ok(strip_reply_envelope(&r).to_vec())
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
            // `identify()` strips the reply envelope; the raw `request()`
            // result still carries `12 <len> A0` and would never pass
            // `ident_valid()` (the 0x12 is not printable).
            if matches!(self.identify(), Ok(r) if ident_valid(&r)) {
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
        self.read_flash(0, IMAGE_SIZE, progress)
    }

    /// Read a range of flash using `12 09 06 <addr> <count>` frames.
    pub fn read_range(
        &mut self,
        start: usize,
        length: usize,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<u8>> {
        if start.checked_add(length).is_none_or(|end| end > IMAGE_SIZE) {
            bail!("range outside 256 KB image");
        }
        self.read_flash(start, length, progress)
    }

    /// Shared read loop for [`Kline::read_full`] / [`Kline::read_range`]:
    /// 0x80-byte `12 09 06` chunks over `start..start+length`.
    ///
    /// On a non-link error (timeout, bad reply) the whole read is retried
    /// once after a 1 s pause with 20 ms inter-byte silence; a hard link
    /// error (port gone) is not retried. GUI and CLI both read through
    /// this, so both recover identically.
    fn read_flash(
        &mut self,
        start: usize,
        length: usize,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<u8>> {
        let attempt = |k: &mut Self, progress: &mut dyn FnMut(usize, usize)| -> Result<Vec<u8>> {
            let mut out = Vec::with_capacity(length);
            let mut addr = start as u32;
            let end = (start + length) as u32;
            while addr < end {
                let count = 0x80u32.min(end - addr) as u8;
                let req = frame_flash_read(addr, count);
                let r = k.request(&req, Duration::from_secs(5))?;
                check_flash_read_reply(&r, count, addr)?;
                out.extend_from_slice(&r[3..3 + count as usize]);
                addr += count as u32;
                progress(out.len(), length);
            }
            Ok(out)
        };
        let res = attempt(self, progress);
        if let Err(e) = &res {
            if !is_link_error(e) {
                thread::sleep(Duration::from_millis(1000));
                self.set_silence(20);
                return attempt(self, progress);
            }
        }
        res
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
        // Enter programming mode before touching flash.
        self.request(&frame_uprog_ein(), Duration::from_secs(3))?;
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
        // Enter programming mode before touching flash.
        self.request(&frame_uprog_ein(), Duration::from_secs(3))?;
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

    /// Read the full DS2 fault memory (FS_LESEN sequence, HANDOVER §22).
    ///
    /// The ECU streams each 50-byte fault record in four chunks that the
    /// PC software (FS_LESEN job) reassembles:
    ///
    ///   1. `12 05 14 01` -> `A0 <anz> <rec 0..22>`    (count + header)
    ///   2. `12 05 04 00` -> `A0 <pos> <rec 22..26>`   (entry, 4 bytes)
    ///   3. `12 05 04 01` -> `A0 <pos> <rec 26..37>`   (shadow, 11 bytes)
    ///   4. `12 05 14 01` -> `A0 <anz'> <rec 37..50> [<next rec 0..22>]`
    ///      (13-byte tail; the next fault's header when `anz' > 0`)
    ///
    /// Step 4 both finishes the current record and advances the ECU's fault
    /// pointer; skipping it (the old bug) re-reads the same fault forever and
    /// loses the 13 tail bytes of the 50-byte record.
    pub fn read_dtcs(&mut self) -> Result<Vec<Dtc>> {
        // The ECU keeps broadcasting a ~16-byte periodic live block on the K-
        // line even while we do a fault read; `is_not_live_broadcast` skips it
        // so the small fault replies (count/entry/shadow) are the ones kept.
        let r = self.request_filtered(&frame_dtc_count(), Duration::from_secs(5), is_not_live_broadcast)?;
        if r.first() != Some(&ECU_ADDR) || r.get(2) != Some(&0xA0) {
            bail!("unexpected fault-count reply: {}", hex(&r));
        }
        let data = &r[3..];
        let mut out: Vec<Dtc> = Vec::new();
        if data.is_empty() {
            return Ok(out); // ECU replied `A0` only: no faults
        }
        let remaining = data[0] as usize;
        if remaining == 0 || data.len() < 23 {
            return Ok(out); // count present but no fault record
        }
        let mut header: Vec<u8> = data[1..23].to_vec(); // rec 0..22 of fault 1

        let mut left = remaining;
        while left > 0 {
            let mut rec: Vec<u8> = header.clone();

            // 2. entry chunk: rec 22..26
            let e = self.request_filtered(&frame_dtc_entry(), Duration::from_secs(5), is_not_live_broadcast)?;
            if e.first() != Some(&ECU_ADDR) || e.get(2) != Some(&0xA0) {
                bail!("unexpected fault-entry reply: {}", hex(&e));
            }
            rec.extend_from_slice(e.get(4..8).unwrap_or(&[]));

            // 3. shadow chunk: rec 26..37
            let s = self.request_filtered(&frame_dtc_shadow(), Duration::from_secs(5), is_not_live_broadcast)?;
            if s.first() != Some(&ECU_ADDR) || s.get(2) != Some(&0xA0) {
                bail!("unexpected fault-shadow reply: {}", hex(&s));
            }
            rec.extend_from_slice(s.get(4..15).unwrap_or(&[]));

            // 4. tail count: rec 37..50 (+ next header when more faults remain)
            let r2 = self.request_filtered(&frame_dtc_count(), Duration::from_secs(5), is_not_live_broadcast)?;
            if r2.first() != Some(&ECU_ADDR) || r2.get(2) != Some(&0xA0) {
                bail!("unexpected fault-tail reply: {}", hex(&r2));
            }
            let d2 = &r2[3..];
            rec.extend_from_slice(d2.get(1..14).unwrap_or(&[]));
            let next = d2.first().copied().unwrap_or(0) as usize;

            out.push(Dtc::from_record(&rec));
            left -= 1;
            if next == 0 || left == 0 {
                break;
            }

            // Next fault's header: inline after the tail when present, else
            // ask the ECU for it with one more count telegram.
            if d2.len() >= 14 + 22 {
                header = d2[14..14 + 22].to_vec();
            } else {
                let r3 = self.request_filtered(&frame_dtc_count(), Duration::from_secs(5), is_not_live_broadcast)?;
                if r3.first() != Some(&ECU_ADDR) || r3.get(2) != Some(&0xA0) {
                    bail!("unexpected fault-count reply: {}", hex(&r3));
                }
                let d3 = &r3[3..];
                if d3.len() < 23 {
                    break; // ECU closed the sequence
                }
                header = d3[1..23].to_vec();
            }
        }
        Ok(out)
    }

    /// Clear the DS2 fault memory (FS_LOESCHEN): `12 04 05`.
    pub fn clear_faults_ds2(&mut self) -> Result<()> {
        let r = self.request_filtered(&frame_dtc_clear(), Duration::from_secs(5), is_not_live_broadcast)?;
        if r.first() != Some(&ECU_ADDR) || r.get(2) != Some(&0xA0) {
            bail!("unexpected fault-clear reply: {}", hex(&r));
        }
        Ok(())
    }

    /// Live data polling via MW_SELECT_LESEN (`12 <LEN> 0B 10 <id bytes>`).
    ///
    /// `addrs` is the *whole* selected set; it is sent in ONE batched telegram
    /// (the tested `MW_SELECT_LESEN_NORM` form, see [`frame_mw_select`]). The
    /// real ECU replies `12 <LEN> A0 10 00 <u16 BE per id ...>` (prefix 0x12,
    /// status 0xA0 at [2], two-byte header at [3..5]) — one big-endian u16 per
    /// requested id, in request order, from offset 5. Values are raw counts;
    /// the toolkit scales them via `src/live.rs` (FACT_A/FACT_B).
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
        // The real ECU reply is `12 <len> A0 10 00 <u16 BE per id ...>` with
        // `<len>` = 6 + 2*n (total wire length incl trailing XOR, n = #ids).
        // The ECU also emits a periodic 16-byte live block on the K-line; matching
        // the expected length selects our reply (and, for a 5-id poll, the
        // identical periodic block — same data). This keeps a single-id battery
        // read from being satisfied by the 16-byte broadcast.
        let expected_len = (6 + 2 * ids.len()) as u8;
        let r = self.request_filtered(&req, Duration::from_millis(800), |f| {
            f.first() == Some(&0x12)
                && f.get(2) == Some(&0xA0)
                && f.get(1) == Some(&expected_len)
        })?;
        if r.len() < 6 || r[0] != 0x12 || r[2] != 0xA0 {
            bail!(
                "unexpected MW live reply: status={:02X} ({})",
                r.get(2).copied().unwrap_or(0),
                hex(&r)
            );
        }
        let mut out = Vec::new();
        for pair in r[5..].as_chunks::<2>().0 {
            out.push(u16::from_be_bytes(*pair));
        }
        Ok(out)
    }
}

/// Strip the `12 <len> <SB>` reply envelope from a reply payload when present.
/// Only [`Kline::identify`] uses this: every other caller keeps the 3-byte
/// header and slices data from offset 3. A bare-ASCII ident string can never
/// collide with the check, since 0x12 is not printable.
fn strip_reply_envelope(r: &[u8]) -> &[u8] {
    if r.len() > 3 && r[0] == ECU_ADDR {
        &r[3..]
    } else {
        r
    }
}

/// True when `reply` is a plausible IDENT **string** — i.e. the envelope-free
/// payload as returned by [`Kline::identify`]. The real M47 ECU answers the
/// IDENT (`12 04 00`) with `12 <len> A0 <ascii>` where the string is
/// ~33+ characters of digits and letters (version, codes, Bosch part number —
/// see [`parse_ident`]); accept a printable string and reject envelope bytes.
pub fn ident_valid(reply: &[u8]) -> bool {
    (4..=48).contains(&reply.len())
        && reply
            .iter()
            .all(|b| matches!(*b, 0x20..=0x7E | 0x09))
}

/// Parsed fields of the IDENT string, as confirmed against the live M47 ECU
/// (2026-11-10 capture, 42-char reply zero-padded):
///
/// ```text
///  1–7   software version        "7786887"
///  8–11  ECU code (BMW docs)    "5201"  → 52/01
/// 12–15  unknown (not parsed)   "1240"
/// 16–19  software date          "4801"  → week 48 / 2001
/// 20–29  Bosch hardware number  "0281001445"
/// 30–33  G code                 "G521"
/// 34–…  zero padding
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentInfo {
    /// Software version, characters 1–7 (e.g. `7786887`).
    pub software: String,
    /// ECU code from the BMW docs, characters 8–11 (e.g. `5201` → 52/01).
    pub ecu_code: String,
    /// Software release date, characters 16–19 (e.g. `4801` → week 48 / 2001).
    pub date: String,
    /// Bosch hardware number, characters 20–29 (e.g. `0281001445`).
    pub hw_number: String,
    /// G code, characters 30–33 (e.g. `G521`).
    pub g_code: String,
}

/// Parse an IDENT payload (bare ASCII, envelope stripped) into its fields.
/// Returns `None` when the string is too short or non-ASCII — e.g. an
/// erased/corrupt flash typically answers with a truncated or garbled string.
pub fn parse_ident(reply: &[u8]) -> Option<IdentInfo> {
    if reply.len() < 33 {
        return None;
    }
    let s = std::str::from_utf8(reply).ok()?;
    Some(IdentInfo {
        software: s[0..7].to_string(),
        ecu_code: s[7..11].to_string(),
        date: s[15..19].to_string(),
        hw_number: s[19..29].to_string(),
        g_code: s[29..33].to_string(),
    })
}

/// Format a 4-char ident pair for display: `"5201"` → `"52/01"`.
pub fn format_pair(field: &str) -> String {
    if field.len() == 4 {
        format!("{}/{}", &field[..2], &field[2..])
    } else {
        field.to_string()
    }
}

/// True when `f` is NOT the ECU's periodic MW live broadcast block
/// (`12 10 A0 10 …`, 16 bytes). Used to skip that block when selecting the
/// small fault/battery replies that share the K-line with it.
pub fn is_not_live_broadcast(f: &[u8]) -> bool {
    !(f.get(1) == Some(&0x10) && f.get(3) == Some(&0x10))
}

/// Validate a flash-read reply: `12 <len> A0 <data...>` — status byte at
/// [2], data from [3] (per the decompiled FLASH_LESEN: `move B0,S1[2]` for
/// the status, `move S4,S1[3]L1` for the data).
fn check_flash_read_reply(r: &[u8], count: u8, addr: u32) -> Result<()> {
    if r.first() != Some(&ECU_ADDR) || r.get(2) != Some(&0xA0) {
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
    fn mw_select_batched_length() {
        // Tested logger (MW_SELECT_LESEN_NORM) rule: LEN = 5 + 2n (total wire
        // bytes, prefix `12 LEN 0B 10` + 2 bytes/id + trailing XOR).
        let f = frame_mw_select(&[0x0F65]);
        assert_eq!(f[0], 0x12);
        assert_eq!(f[1], 7, "single id length must be 5 + 2*1");
        assert_eq!(f[2..4], [0x0B, 0x10]);
        assert_eq!(f.len(), 6);
        // A batched set of 4 ids (SET_1 in `testo customjobs.txt`).
        let f = frame_mw_select(&[0x0F62, 0x0F00, 0x0F30, 0x0E81]);
        assert_eq!(f[1], 13, "4 ids -> 5 + 8 = 13");
        assert_eq!(f.len(), 12);
        // id order preserved, big-endian.
        assert_eq!(&f[4..6], [0x0F, 0x62]);
        assert_eq!(&f[10..12], [0x0E, 0x81]);
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

    // ---- Real-car (2026-11-09 M47) fixtures from `dde3 toolkit log.txt` ----

    #[test]
    fn real_car_live_reply_splits_echo() {
        // Merged wire bytes as captured on the K-line: the ECU reply (16 B incl
        // trailing XOR) immediately followed by our TX echo (15 B incl XOR). The
        // parser must split them back-to-back, reply first, echo second.
        let wire: Vec<u8> = vec![
            // reply: 12 10 A0 10 00 00 00 00 00 03 DE 0B 26 02 4A 0A
            0x12, 0x10, 0xA0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xDE, 0x0B, 0x26,
            0x02, 0x4A, 0x0A,
            // echo of our request: 12 0F 0B 10 0F 83 0F 10 0F 40 0F 00 0F 65 BF
            0x12, 0x0F, 0x0B, 0x10, 0x0F, 0x83, 0x0F, 0x10, 0x0F, 0x40, 0x0F, 0x00, 0x0F,
            0x65, 0xBF,
        ];
        let mut p = FrameParser::default();
        p.feed(&wire);
        let reply = p.take_frame().expect("reply frame");
        assert_eq!(
            reply.payload,
            vec![0x12, 0x10, 0xA0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xDE, 0x0B, 0x26, 0x02, 0x4A]
        );
        let echo = p.take_frame().expect("echo frame");
        assert_eq!(
            echo.payload,
            vec![0x12, 0x0F, 0x0B, 0x10, 0x0F, 0x83, 0x0F, 0x10, 0x0F, 0x40, 0x0F, 0x00, 0x0F, 0x65]
        );
        assert!(p.take_frame().is_none(), "buffer drained");
    }

    #[test]
    fn real_car_live_values_decode() {
        // The five u16 values in the live reply, in request order
        // [0F83 0F10 0F40 0F00 0F65], decode to the engine-off / ~12 deg C set:
        // PWG=0, N=0, boost=990 hPa, WTF=2854 (->12.3 deg C), batt=586 (->11.9 V).
        let payload = vec![
            0x12, 0x10, 0xA0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xDE, 0x0B, 0x26, 0x02, 0x4A,
        ];
        assert_eq!(payload[0], 0x12, "prefix must be the ECU address");
        assert_eq!(payload[2], 0xA0, "status at [2]");
        let vals: Vec<u16> = payload[5..].as_chunks::<2>().0.iter().map(|p| u16::from_be_bytes(*p)).collect();
        assert_eq!(vals, vec![0, 0, 990, 2854, 586]);
    }

    #[test]
    fn real_car_ident_reply_is_envelope_string() {
        // Wire (2026-11-10 capture, dde3-toolkit/captures/dde3newlog.txt):
        // the ECU wraps the ident string in the normal envelope,
        // `12 2E A0 37 37 38 36 …` — status A0, ASCII data from [3].
        // LEN byte 0x2E = 46 = 3 header + 42 data + trailing XOR (the
        // RX log line shows the 45-byte payload, XOR stripped by the parser).
        let s = b"77868875201124048010281001445G521000000000"; // 42 chars
        assert_eq!(s.len(), 42, "len byte 0x2E = 3 header + 42 data + XOR");
        let mut wire: Vec<u8> = vec![0x12, 0x04, 0x00, 0x16]; // echo
        wire.extend_from_slice(&[0x12, 0x2E, 0xA0]);
        wire.extend_from_slice(s);
        wire.push(xor_cs(&wire));
        let mut p = FrameParser::default();
        p.feed(&wire);
        let _echo = p.take_frame().expect("echo");
        let ident = p.take_frame().expect("ident reply");
        assert_eq!(&ident.payload[..3], &[0x12, 0x2E, 0xA0], "envelope form");
        // identify() contract: envelope stripped, bare string remains.
        let stripped = strip_reply_envelope(&ident.payload);
        assert_eq!(stripped, s);
        assert!(ident_valid(stripped));
        let info = parse_ident(stripped).expect("parses");
        assert_eq!(info.software, "7786887");
        assert_eq!(info.ecu_code, "5201");
        assert_eq!(info.date, "4801");
        assert_eq!(info.hw_number, "0281001445");
        assert_eq!(info.g_code, "G521");
    }

    #[test]
    fn ident_valid_accepts_plain_string() {
        assert!(ident_valid(b"8875201"));
        assert!(ident_valid(b"DDE 3.0 fuer M47"));
        assert!(!ident_valid(&[]), "empty rejected");
        assert!(!ident_valid(&[0x12, 0x04, 0x00]), "envelope is not a string");
        // Envelope-prefixed reply must NOT validate — this is what used to
        // trigger the spurious "ident looks erased/corrupt" warning.
        let enveloped: Vec<u8> = vec![0x12, 0x2E, 0xA0, b'7', b'7', b'8', b'6'];
        assert!(!ident_valid(&enveloped));
        assert!(ident_valid(strip_reply_envelope(&enveloped)));
    }

    #[test]
    fn parse_ident_fields() {
        let s = b"77868875201124048010281001445G521000000000";
        let i = parse_ident(s).expect("parses the captured string");
        assert_eq!(i.software, "7786887");
        assert_eq!(i.ecu_code, "5201");
        assert_eq!(i.date, "4801");
        assert_eq!(i.hw_number, "0281001445");
        assert_eq!(i.g_code, "G521");
        assert!(parse_ident(b"8875201").is_none(), "short string not parseable");
        assert!(parse_ident(&[0xFF; 40]).is_none(), "non-ASCII not parseable");
        assert_eq!(format_pair("5201"), "52/01");
        assert_eq!(format_pair("4801"), "48/01");
    }

    #[test]
    fn is_not_live_broadcast_distinguishes() {
        // The 16-byte periodic block is skipped; small fault/battery replies kept.
        let broadcast = vec![
            0x12, 0x10, 0xA0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xDE, 0x0B, 0x26, 0x02, 0x4A,
        ];
        let count_reply = vec![0x12, 0x05, 0xA0, 0x03]; // 3 faults
        let batt_reply = vec![0x12, 0x08, 0xA0, 0x10, 0x00, 0x02, 0x4A]; // 586 -> 11.9 V
        assert!(!is_not_live_broadcast(&broadcast));
        assert!(is_not_live_broadcast(&count_reply));
        assert!(is_not_live_broadcast(&batt_reply));
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
        // Verified against the WinKFP .prg VM (docs/FINDINGS_flash_and_fast_ds2.md):
        // FLASH_LESEN addr=0x38000 count=0x80 -> 12 09 06 00 03 80 00 80 (addr BE @4..6)
        assert_eq!(
            frame_flash_read(0x38000, 0x80),
            vec![0x12, 0x09, 0x06, 0x00, 0x03, 0x80, 0x00, 0x80]
        );
        // width probe: count is a single wire byte (high byte dropped)
        assert_eq!(
            frame_flash_read(0x12345, 0x34),
            vec![0x12, 0x09, 0x06, 0x00, 0x01, 0x23, 0x45, 0x34]
        );
        // FLASH_LOESCHEN addr=0x38000 -> 12 09 07 06 03 80 00 00 (no count)
        assert_eq!(
            frame_flash_erase(0x38000, 0x00),
            vec![0x12, 0x09, 0x07, 0x06, 0x03, 0x80, 0x00, 0x00]
        );
        // FLASH_SCHREIBEN 4 data bytes @0x38000 -> 12 0D 07 02 03 80 00 04 11 22 33 44
        assert_eq!(
            frame_flash_write(0x38000, &[0x11, 0x22, 0x33, 0x44]),
            vec![0x12, 0x0D, 0x07, 0x02, 0x03, 0x80, 0x00, 0x04, 0x11, 0x22, 0x33, 0x44]
        );
        // FLASH_SCHREIBEN_ENDE @0x38000 -> 12 09 07 0F 03 80 00 00
        assert_eq!(
            frame_flash_write_end(0x38000),
            vec![0x12, 0x09, 0x07, 0x0F, 0x03, 0x80, 0x00, 0x00]
        );
    }

    #[test]
    fn reopen_divisor_maps_125000_to_124800() {
        assert_eq!(reopen_divisor(125000), 124_800);
        assert_eq!(reopen_divisor(38400), 38_400);
        assert_eq!(reopen_divisor(9600), 9_600);
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
            ..Default::default()
        };
        let s = d.describe();
        assert!(s.contains("Nadelbewegungsfuehler"), "ORT text: {s}");
        assert!(s.contains("Signal Kurzschluss nach B+"), "ART text: {s}");
    }

    #[test]
    fn dtc_from_record_reference_m47() {
        // The M47 reference fault as printed in `actual error.txt` (the
        // reference tool's 50-byte "Errorcode" record).
        let rec: [u8; 50] = [
            0x08, 0xC8, 0x3C, 0x00, 0x38, 0x9C, 0xE3, 0x00, 0x37, 0x9D, 0xDE, 0xC4, 0xC1, 0x04,
            0x00, 0x48, 0x00, 0x99, 0x03, 0x00, 0x76, 0xDC, 0x5B, 0x03, 0x4A, 0x00, 0x76, 0xD1,
            0x7D, 0x00, 0x76, 0xDC, 0x5A, 0x00, 0x34, 0x32, 0x33, 0x32, 0x34, 0x00, 0x30, 0x21,
            0x00, 0x00, 0x00, 0x31, 0xC5, 0x00, 0x00, 0x22,
        ];
        let d = Dtc::from_record(&rec);
        assert_eq!(d.ort, 0x08, "ORT must be Gluehanlage, not Mengensteller");
        assert_eq!(crate::dtc::ort_text(d.ort), "Gluehanlage");
        assert_eq!(d.freq, 60);
        assert_eq!(d.art_mask, 0xC8);
        assert_eq!(d.arts, vec![0x32, 0x05, 0x07]);
        assert_eq!(d.record, rec.to_vec());
        let s = d.describe();
        assert!(s.contains("Gluehkerzen bzw. Gluehrelais defekt"), "{s}");
        assert!(s.contains("Fehler momentan vorhanden"), "{s}");
        assert!(s.contains("Fehler sporadisch"), "{s}");
        assert!(!s.contains("Mengensteller"), "{s}");
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
