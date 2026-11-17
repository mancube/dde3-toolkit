//! In-process DDE3 ECU simulator.
//!
//! Hardware topology (2026-11-09): the FT232R (`/dev/ttyUSB1`) drives the
//! K-line and its RX sees its own transmission (bus echo). The UART-LPT
//! (`/dev/ttyUSB0`) is a **passive tap** — it sees exactly what the ECU
//! receives but cannot drive the bus, so an ECU-side reply cannot come from
//! the serial line. The simulator therefore runs inside the toolkit:
//! `Kline::request()` writes the real frame to the FT232, reads back the
//! bus echo (TX path verified), and injects the simulated ECU reply.
//!
//! Reply convention (2026-11-09 real M47 capture): the ECU replies with its
//! OWN address (0x12) — not the DS2 0x92 response prefix the toolkit first
//! assumed — the JOBRESULT status byte at offset 2, data from offset 3:
//!
//! ```text
//! <12> <len> <SB> [<data ...>]   + trailing XOR on the wire
//! ```
//!
//! `SB` is the JOBRESULT status byte (0xA0 OKAY, 0xA1 BUSY, 0xFF NACK, …).
//! MW live data (`0B 10`) replies `12 <len> A0 10 00 <u16 BE per id ...>`
//! (data from offset 5), and IDENT replies `12 <len> A0 <ascii string>` —
//! the normal envelope, confirmed on the live ECU (2026-11-10 capture:
//! `12 2E A0 37 37 38 36 …` = `7786887 5201 … 0281001445 G521 …`).
//! The ECU also emits a periodic ~16-byte live block on the K-line; the
//! toolkit's `is_not_live_broadcast` skips it for the small fault/battery
//! replies.

use std::collections::BTreeMap;

use anyhow::{Context, Result};

use crate::protocol::{ECU_ADDR, IMAGE_SIZE};

const SB_OK: u8 = 0xA0;

/// AIF programming record: 46 bytes at cal 0x7CB2 = flash 0x3FCB2 (the
/// calibration area is the last 0x8000 of the 256 KB flash) — the first of 14
/// 46-byte slots. The M47 firmware's DS2 cmd-0x02 handler (0x026AFE) scans
/// the slot table (MCU address 0x7CB2 — an MCU-space address, NOT the flash
/// offset; that's where the 0x7CB2 confusion came from), then copies the slot
/// via the 0x0026A2 memory reader with
/// R14 = 0x2E and bumps the reply LEN by 0x2E — so the reply is
/// `12 32 A0 <46 B> <XOR>`, 50 bytes on the wire. Bytes from the in-car tune
/// (tuning/testing/tunerpro 16-11-2026.bin @ 0x7CB2 — the real car's record).
const AIF_SLOT: usize = 0x3FCB2; // cal 0x7CB2 = 0x38000 + 0x7CB2
const AIF_LEN: usize = 46;
const AIF_RECORD: [u8; 46] = [
    0x20, 0x2C, 0xA3, 0x9E, 0x14, 0x10, 0x00, 0x00, 0xC4, 0xC1, 0x04, 0x00, 0x48, 0x00, 0x99, 0x03,
    0x00, 0x76, 0xDC, 0x5B, 0x03, 0x4A, 0x00, 0x76, 0xD1, 0x7D, 0x00, 0x76, 0xDC, 0x5A, 0x00, 0x34,
    0x32, 0x33, 0x32, 0x34, 0x00, 0x30, 0x21, 0x00, 0x00, 0x00, 0x31, 0xC5, 0x00, 0x00
];

pub struct SimEcu {
    flash: Vec<u8>,
    in_prog: bool,
    /// Seed/key (DS2 `0x90`): the last issued expected 4-byte key. `None` =
    /// no pending seed request. The ECU side is implemented per the M47
    /// decomp (`disasm.txt` `sub_028B5E`) + the DDE3 SEED_KEY job's key
    /// reconstruction: a seed request answers a 46-byte seed telegram
    /// (`12 2E A0 <42 data>`); the key the ECU will accept is what the
    /// tester's formula yields on those exact bytes, `K[i] = resp[0x12+i] +
    /// resp[Z+i] + resp[0x29+i]` (byte adds) — computed here on the final
    /// reply so every Z (0..0x28) is consistent by construction. A wrong key
    /// answers `B0`. There is deliberately NO "already unlocked" latch: every
    /// seed request regenerates a fresh seed telegram + fresh expected key,
    /// because the flashing tool retries the whole exchange and expects a
    /// fresh seed each time (a short ack made it send a garbage key + stall).
    pending_key: Option<[u8; 4]>,
    /// Contiguous span covered by peer data writes (see `written_span`).
    written_span: Option<(u32, u32)>,
    /// Stored fault memory as full 50-byte fault records (FS_LESEN).
    faults: Vec<[u8; 50]>,
    /// Index of the fault currently being read (FS_LESEN pointer).
    fault_pos: usize,
    /// FS_LESEN step: 0=count, 1=entry, 2=shadow, 3=tail.
    fault_step: u8,
    /// Test-stamp (Pruefstempel) bytes — DS2 cmd `0x0E` read / `0x0F` write.
    test_stamp: [u8; 3],
    /// Adjustment values by item id (DS2 cmd `0x40` read / `0x41` set /
    /// `0x42` store; ids are the `NN` bytes of those jobs).
    adjustments: BTreeMap<u8, u16>,
}

impl Default for SimEcu {
    fn default() -> Self {
        Self::new()
    }
}

impl SimEcu {
    /// The sim's fault records (shared by all constructors).
    fn default_faults() -> Vec<[u8; 50]> {
        vec![
            Self::record(0, 0x26, 0x02, 0x012C, 0x0096), // Luftmassenmesser, Signal Unterbrechung
            Self::record(1, 0x05, 0x01, 0x04D2, 0x021F), // Nadelbewegungsfuehler, KS B+
            Self::record(2, 0x36, 0x04, 0x02BC, 0x03A5), // Ladedruckfuehler, nicht plausibel
        ]
    }

    /// Fresh sim with a deterministic 256 KB pattern (not zeros, so an
    /// un-erased flash is distinguishable from a written one).
    pub fn new() -> Self {
        let mut flash: Vec<u8> = (0..IMAGE_SIZE)
            .map(|i| {
                let v = i.wrapping_mul(7).wrapping_add(0x5A);
                (v ^ (i >> 8)) as u8
            })
            .collect();
        // Seed the AIF slot (flash 0x3FCB2 = cal 0x7CB2) with the real car's
        // record (see AIF_RECORD); flash writes/erases there update it like
        // on the real ECU.
        flash[AIF_SLOT..AIF_SLOT + AIF_LEN].copy_from_slice(&AIF_RECORD);
        Self {
            flash,
            in_prog: false,
            pending_key: None,
            written_span: None,
            faults: Self::default_faults(),
            fault_pos: 0,
            fault_step: 0,
            test_stamp: [0, 0, 0],
            adjustments: BTreeMap::new(),
        }
    }

    /// Sim backed by a real flash dump: the given `IMAGE_SIZE` bytes ARE the
    /// flash — `FLASH_LESEN` (06) and the AIF read (0x02, slot 0x3FCB2) serve
    /// them verbatim, so an external tool reading the fake ECU gets the
    /// exact content of the provided image.
    pub fn with_flash_image(image: Vec<u8>) -> Result<Self> {
        if image.len() != IMAGE_SIZE {
            anyhow::bail!(
                "flash image is {} bytes, expected {} (0x{:X})",
                image.len(),
                IMAGE_SIZE,
                IMAGE_SIZE
            );
        }
        Ok(Self {
            flash: image,
            in_prog: false,
            pending_key: None,
            written_span: None,
            faults: Self::default_faults(),
            fault_pos: 0,
            fault_step: 0,
            test_stamp: [0, 0, 0],
            adjustments: BTreeMap::new(),
        })
    }

    /// [`SimEcu::with_flash_image`] from a file on disk.
    pub fn from_flash_file(path: &std::path::Path) -> Result<Self> {
        let image = std::fs::read(path)
            .with_context(|| format!("cannot read flash image {}", path.display()))?;
        Self::with_flash_image(image)
            .with_context(|| format!("flash image {}", path.display()))
    }

    /// Synthesize a deterministic 50-byte fault record: `[0]=ORT [1]=ART mask
    /// [2]=freq [3..9]=freeze u16 BE` plus filler for the rest.
    fn record(idx: usize, ort: u8, mask: u8, uw1: u16, uw2: u16) -> [u8; 50] {
        let mut r = [0u8; 50];
        r[0] = ort;
        r[1] = mask;
        r[2] = 1;
        r[3..5].copy_from_slice(&uw1.to_be_bytes());
        r[5..7].copy_from_slice(&uw2.to_be_bytes());
        for (i, b) in r.iter_mut().enumerate().skip(9) {
            *b = (i as u8).wrapping_mul(3).wrapping_add(idx as u8).wrapping_add(0x11);
        }
        r
    }

    /// Handle a DS2 request payload (without the trailing XOR byte).
    /// Returns the reply payload.
    pub fn handle(&mut self, p: &[u8]) -> Vec<u8> {
        let mut r = self.handle_inner(p);
        // The ECU's LEN byte is the TOTAL wire length INCLUDING the trailing
        // XOR — verified on every reply in captures/dde3newlog.txt (IDENT
        // `12 2E …` = 45-byte payload + XOR, status `12 05 A0 00` = 4 + 1, …).
        // Foreign flashing tools validate LEN, so it must be exact.
        if r.len() > 1 {
            r[1] = (r.len() + 1) as u8;
        }
        r
    }

    fn handle_inner(&mut self, p: &[u8]) -> Vec<u8> {
        if p.len() < 3 || p[0] != ECU_ADDR {
            return Self::nack(p);
        }
        let len_echo = p[1];
        let sid = p[2];
        match sid {
            // IDENT: `12 04 00` — the real ECU wraps the ident string in the
            // normal envelope (2026-11-10 capture: `12 2E A0 37 37 38 36 …`).
            // The string is 42 chars: `12 2E A0` + 42 + trailing XOR = 46 =
            // LEN byte 0x2E (the old 43-char variant had counted the trailing
            // XOR byte 0x30 as a fourth '0' — foreign tools reject the frame).
            0x00 => {
                let s = b"77868875201124048010281001445G521000000000"; // 42 chars
                let mut r = vec![0x12, 0, SB_OK];
                r.extend_from_slice(s);
                r
            }
            // UPROG_EIN: `12 04 0D` — enter programming mode.
            0x0D => {
                self.in_prog = true;
                Self::status(p, len_echo, SB_OK)
            }
            // AIF_LESEN: `12 05 02 <param>` — the param byte is not used by
            // the ECU (it scans its 14-slot table itself; 0x00 and 0xFF both
            // work). Programmed: 46-byte record from flash 0x3FCB2 (cal 0x7CB2),
            // `12 32 A0 <46 B> <XOR>` (LEN = 4 + 46 = 50 = 0x32). Empty slot
            // (first byte 0xFF, e.g. after an erase): short `12 05 A0 FF` —
            // the job's "AIF_NICHT_PROGRAMMIERT" check is resp[3] == FF.
            0x02 => {
                let rec = &self.flash[AIF_SLOT..AIF_SLOT + AIF_LEN];
                if rec[0] == 0xFF {
                    // Empty slot: 1 data byte 0xFF — the job's
                    // "AIF_NICHT_PROGRAMMIERT" check is resp[3] == FF.
                    let mut r = Self::status(p, 0x05, SB_OK);
                    r.push(0xFF);
                    r
                } else {
                    let mut r = Self::status(p, 0x32, SB_OK);
                    r.extend_from_slice(rec);
                    r
                }
            }
            // STATUS_CODIER_CHECKSUMME: `12 04 0A` — the live M47 answers
            // `12 05 A0 00` (dde3newlog.txt): SB OKAY + one data byte 0x00,
            // i.e. wire `12 05 A0 00 <XOR>`, LEN 5. Matched byte for byte.
            0x0A => {
                let mut r = Self::status(p, len_echo, SB_OK);
                r.push(0x00);
                r
            }
            // DIAGNOSE_ENDE: `12 04 9F` — leave session.
            0x9F => {
                self.in_prog = false;
                Self::status(p, len_echo, SB_OK)
            }
            // SEED_KEY family: seed request `12 08 90 42 4D 57 Z` vs key
            // send `12 08 90 K0 K1 K2 K3` — see `seed_key` (M47 decomp
            // sub_028B5E, DDE3 SEED_KEY job reconstruction).
            0x90 => self.seed_key(p, len_echo),
            // BAUD change (0x91 patch): `12 08 91 00 <baud BE> <p2>` — ACK so a
            // `--sim` fast-baud switch verifies (real stock firmware NACKs this).
            0x91 => Self::status(p, len_echo, SB_OK),
            // FLASH_LESEN: `12 09 06 00 <A_hi A_mid A_lo> <count@7>` (addr big-endian).
            0x06 => self.flash_read(p, len_echo),
            // FLASH write family: `12 09 07 <sub> ...`.
            0x07 => match p.get(3).copied() {
                Some(0x06) => self.flash_erase(p, len_echo),
                Some(0x02) => self.flash_write(p, len_echo),
                Some(0x0F) => Self::status(p, len_echo, SB_OK),
                // DATENBEREICH_LOESCHEN_0E: `12 09 07 0E 42 41 50 00`.
                Some(0x0E) => {
                    self.faults.clear();
                    self.fault_pos = 0;
                    self.fault_step = 0;
                    Self::status(p, len_echo, SB_OK)
                }
                // STEUERN_TESTPLATZ (cmd 07 write-type; the job sends it after
                // an unlock) — status only, accepted if unlocked.
                Some(0x10) => {
                    if self.pending_key.is_none() {
                        Self::status(p, len_echo, SB_OK)
                    } else {
                        Self::nack(p)
                    }
                }
                _ => Self::nack(p),
            },
            // MW live data: `12 05 0B <sub> [ids...]`.
            0x0B => self.mw(p),
            // FS_LESEN count/tail: `12 05 14 01` (see HANDOVER §22).
            0x14 if p.get(3) == Some(&0x01) => self.fault_count(p, len_echo),
            // FS_LESEN entry `12 05 04 00` / FS_SHADOW_LESEN `12 05 04 01`.
            0x04 => match p.get(3).copied() {
                Some(0x00) => self.fault_entry(p, len_echo),
                Some(0x01) => self.fault_shadow(p, len_echo),
                _ => Self::nack(p),
            },
            // FS_LOESCHEN: `12 04 05` → `92 04 A0`.
            0x05 => {
                self.faults.clear();
                self.fault_pos = 0;
                self.fault_step = 0;
                Self::status(p, len_echo, SB_OK)
            }
            // ACTUATOR OVERRIDE ("Steuern", DS2 `0C`): `12 08 0C <idH> <idL>
            // <v> <m>` — status only (m = 00 set / 55 hold / AA release).
            // The selective-release (`…30 55`) and KWH-off (`…00 AA`) release
            // codes ride longer frames handled at the job level; the fake ECU
            // answers the override job with the plain JOBRESULT status.
            0x0C => Self::status(p, len_echo, SB_OK),
            // TEST STAMP read (DS2 `0E`, "Pruefstempel"): `12 04 0E` → status +
            // the 3 test-stamp bytes (`12 <len> A0 <b0> <b1> <b2> <XOR>`,
            // LEN = 7 = payload(6) + XOR).
            0x0E => {
                let mut r = Self::status(p, len_echo, SB_OK);
                r.push(self.test_stamp[0]);
                r.push(self.test_stamp[1]);
                r.push(self.test_stamp[2]);
                r
            }
            // TEST STAMP write (DS2 `0F`): status only, storing `b1 b2 b3` when
            // the frame carries them (job-style `12 07 0F <b1> <b2> <b3>`).
            0x0F => {
                if p.len() >= 6 {
                    self.test_stamp = [p[3], p[4], p[5]];
                }
                Self::status(p, len_echo, SB_OK)
            }
            // EWS SYNC START (DS2 `6C`): `12 05 6C 00` → status only.
            0x6C => Self::status(p, len_echo, SB_OK),
            // EWS SYNC STATUS (DS2 `6D`): `12 04 6D` → status byte(s).
            0x6D => Self::status(p, len_echo, SB_OK),
            // ADJUSTMENT VALUE READ (DS2 `40`): `12 05 40 <NN>` → status + the
            // value (`12 <len> A0 <val> <XOR>`, LEN = 6 = payload(5) + XOR).
            0x40 => {
                let nn = *p.get(3).unwrap_or(&0);
                let val = self.adjustments.get(&nn).copied().unwrap_or(0);
                let mut r = Self::status(p, len_echo, SB_OK);
                r.push((val >> 8) as u8);
                r.push(val as u8);
                r
            }
            // ADJUSTMENT VALUE SET, RAM copy (DS2 `41`): `12 07 41 <NN> <hi>
            // <lo>` → status only.
            0x41 => {
                let nn = *p.get(3).unwrap_or(&0);
                let hi = *p.get(4).unwrap_or(&0) as u16;
                let lo = *p.get(5).unwrap_or(&0) as u16;
                self.adjustments.insert(nn, (hi << 8) | lo);
                Self::status(p, len_echo, SB_OK)
            }
            // ADJUSTMENT VALUE STORE → EEPROM (DS2 `42`): `12 05 42 <NN>` →
            // status only.
            0x42 => Self::status(p, len_echo, SB_OK),
            // DS2 SESSION KEEP (9E): `12 04 9E` → status (refresh timer).
            0x9E => Self::status(p, len_echo, SB_OK),
            _ => Self::nack(p),
        }
    }

    /// FS_LESEN count/tail: `12 05 14 01`.
    ///
    /// At the start of a read (step 0): `A0 <anz> <rec 0..22>` — the count
    /// of remaining faults plus the first 22 bytes of the current record.
    /// After entry+shadow (step 3): `A0 <anz'> <rec 37..50>` plus, when more
    /// faults remain, the next record's 22-byte header (that is what
    /// advances the ECU's fault pointer).
    fn fault_count(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        if self.fault_step != 0 && self.fault_step != 3 {
            return Self::status(p, len_echo, 0xB2); // out-of-sequence
        }
        let mut r = Self::status(p, len_echo, SB_OK);
        match self.fault_step {
            0 if self.fault_pos >= self.faults.len() => r.push(0), // no faults
            0 => {
                let rec = &self.faults[self.fault_pos];
                r.push(self.faults.len() as u8 - self.fault_pos as u8);
                r.extend_from_slice(&rec[0..22]);
                self.fault_step = 1;
            }
            3 => {
                let rec = self.faults[self.fault_pos];
                let next = self.fault_pos + 1;
                if next < self.faults.len() {
                    r.push(self.faults.len() as u8 - next as u8);
                    r.extend_from_slice(&rec[37..50]);
                    r.extend_from_slice(&self.faults[next][0..22]);
                    self.fault_pos = next;
                    self.fault_step = 1;
                } else {
                    r.push(0);
                    r.extend_from_slice(&rec[37..50]);
                    self.fault_pos = next;
                    self.fault_step = 0;
                }
            }
            _ => unreachable!(),
        }
        r
    }

    /// FS_LESEN entry: `12 05 04 00` — record bytes 22..26 of the current
    /// fault, prefixed with the 1-based fault position.
    fn fault_entry(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        // Tools that open the fault read with a bare `04 00` (no `14 01`
        // count request first) get fault 1 from the top of the list; the
        // rest of the sequence then flows through the normal state machine.
        if self.fault_step != 1 {
            self.fault_pos = 0;
            self.fault_step = 1;
        }
        if self.fault_pos >= self.faults.len() {
            // no faults: the same `A0 00` marker as the count reply.
            let mut r = Self::status(p, len_echo, SB_OK);
            r.push(0);
            return r;
        }
        let rec = self.faults[self.fault_pos];
        let mut r = Self::status(p, len_echo, SB_OK);
        r.push(self.fault_pos as u8 + 1);
        r.extend_from_slice(&rec[22..26]);
        self.fault_step = 2;
        r
    }

    /// FS_LESEN shadow: `12 05 04 01` — record bytes 26..37 of the current
    /// fault, prefixed with the 1-based fault position.
    fn fault_shadow(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        if self.fault_step != 2 || self.fault_pos >= self.faults.len() {
            return Self::status(p, len_echo, 0xB2);
        }
        let rec = self.faults[self.fault_pos];
        let mut r = Self::status(p, len_echo, SB_OK);
        r.push(self.fault_pos as u8 + 1);
        r.extend_from_slice(&rec[26..37]);
        self.fault_step = 3;
        r
    }

    // --- helpers ---------------------------------------------------------

    fn status(_p: &[u8], len_echo: u8, sb: u8) -> Vec<u8> {
        vec![ECU_ADDR, len_echo, sb]
    }

    fn nack(p: &[u8]) -> Vec<u8> {
        Self::status(p, p.get(1).copied().unwrap_or(0), 0xFF)
    }

    /// 24-bit big-endian address at offsets 4,5,6 — the layout of BOTH the
    /// read frame (`12 09 06 00 <A_hi A_mid A_lo> <n>`) and the write family
    /// (`12 .. 07 <sub> <A_hi A_mid A_lo> ...`). Offset 3 is the const 0x00
    /// (read) or the subfunction (write); the address never lives there.
    fn addr_be(p: &[u8]) -> Option<u32> {
        let (hi, mid, lo) = (*p.get(4)?, *p.get(5)?, *p.get(6)?);
        Some((hi as u32) << 16 | (mid as u32) << 8 | lo as u32)
    }

    fn flash_read(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        // `12 09 06 00 <A_hi A_mid A_lo> <count@7>` — count is 1 byte at [7].
        if let (Some(addr), Some(count)) = (Self::addr_be(p), p.get(7).copied()) {
            if addr + count as u32 > IMAGE_SIZE as u32 {
                return Self::status(p, len_echo, 0xB0); // ERROR_SG_PARAMETER
            }
            let mut r = Self::status(p, len_echo, SB_OK);
            // AIF request via the flash-read path: other flashing tools ask
            // for the 46-byte AIF block ("hardware ID") as a 46-byte
            // FLASH_LESEN at address 0 instead of service 0x02 — serve the
            // same slot the 0x02 handler serves (erased slot = 46×FF, which
            // is what flash @0 holds on a real image anyway).
            if addr == 0 && count as u32 == AIF_LEN as u32 {
                r.extend_from_slice(&self.flash[AIF_SLOT..AIF_SLOT + AIF_LEN]);
            } else {
                r.extend_from_slice(
                    &self.flash[addr as usize..addr as usize + count as usize],
                );
            }
            r
        } else {
            Self::nack(p)
        }
    }

    fn flash_erase(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        // `12 09 07 06 <A_hi A_mid A_lo> 00` — no count in the frame; the
        // toolkit erases the whole writable area from `addr` to the end.
        if let Some(addr) = Self::addr_be(p) {
            if !self.in_prog {
                return Self::nack(p);
            }
            if addr >= IMAGE_SIZE as u32 {
                return Self::status(p, len_echo, 0xB0);
            }
            self.flash[addr as usize..IMAGE_SIZE].fill(0xFF);
            Self::status(p, len_echo, SB_OK)
        } else {
            Self::nack(p)
        }
    }

    fn flash_write(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        // `12 <LEN> 07 02 <A_hi A_mid A_lo> <N@7> <data@8..>` — 64-byte
        // blocks in the EDE/EDZ flash sequence. A bare (zero-data) write
        // block is a hand-shake and is ACKed even outside programming mode;
        // only real data writes require UPROG_EIN (`0D`) first.
        if let (Some(addr), Some(n)) = (Self::addr_be(p), p.get(7).copied()) {
            // In programming mode every write is allowed. Outside it, the
            // ECU still accepts zero-length control writes; only real data
            // writes require UPROG_EIN (`0D`) first.
            let data = p.get(8..8 + n as usize).unwrap_or(&[]);
            if data.is_empty() {
                return Self::status(p, len_echo, SB_OK);
            }
            if !self.in_prog {
                return Self::nack(p);
            }
            let end = addr as usize + data.len();
            if end > IMAGE_SIZE {
                return Self::status(p, len_echo, 0xB0);
            }
            self.flash[addr as usize..end].copy_from_slice(data);
            self.mark_written(addr, data.len());
            Self::status(p, len_echo, SB_OK)
        } else {
            Self::nack(p)
        }
    }

    /// DS2 `0x90` seed/key. See the struct field docs for the protocol
    /// source (M47 decomp `sub_028B5E` + DDE3 SEED_KEY job reconstruction).
    fn seed_key(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        let is_seed_request = p.get(3..6) == Some(&[0x42, 0x4D, 0x57][..]); // "BMW"
        if is_seed_request {
            // Always a fresh seed telegram + fresh expected key, even if a key
            // was already accepted this session (the tool retries the whole
            // exchange; the real ECU regenerates the seed per request).
            self.seed_request(p, len_echo)
        } else {
            // Key send: `12 08 90 K0 K1 K2 K3`.
            self.key_send(p, len_echo)
        }
    }

    /// Seed request → 46-byte seed telegram. The 42 data bytes are a
    /// deterministic pattern (seeded by Z so different Z give different
    /// material); the expected key is then computed ON the final wire frame
    /// (payload + trailing XOR) with the tester's formula, so it is
    /// consistent for every Z — including Z where the `FR[Z+i]` window
    /// reaches the trailing XOR byte (the ECU and the tester read the same
    /// bytes: `K[i] = FR[0x12+i] + FR[Z+i] + FR[0x29+i]`).
    fn seed_request(&mut self, p: &[u8], _len_echo: u8) -> Vec<u8> {
        let z = *p.get(6).unwrap_or(&0x05) as usize;
        // Reply payload (the transport appends the trailing XOR and `handle`
        // stamps LEN = payload + 1 = 46 = 0x2E): 12 2E A0 <42 data bytes>.
        let mut r: Vec<u8> = vec![ECU_ADDR, 0x2E, SB_OK];
        for i in 0..42usize {
            r.push((z as u8).wrapping_mul(31).wrapping_add(i as u8).wrapping_add(0x27));
        }
        assert_eq!(r.len(), 45, "42-byte seed table + 3 header");
        // Build the full 46-byte wire frame (payload + trailing XOR) — this
        // is what the tester receives and runs its key formula on.
        let xor = crate::protocol::xor_cs(&r);
        let mut frame = r.clone();
        frame.push(xor);
        assert_eq!(frame.len(), 46, "46-byte wire frame");
        // Expected key = tester's formula on the exact 46-byte wire frame
        // with modulo-46 wrapping (the ECU firmware uses 8-bit indices that
        // naturally wrap; the 46-byte frame size means indices wrap at 46):
        // K[i] = FR[(0x12+i)%46] + FR[(Z+i)%46] + FR[(0x29+i)%46] (byte adds).
        const FRAME_SIZE: usize = 46;
        let key: [u8; 4] = std::array::from_fn(|i| {
            let a = frame[(0x12 + i) % FRAME_SIZE];
            let b = frame[(z + i) % FRAME_SIZE];
            let c = frame[(0x29 + i) % FRAME_SIZE];
            a.wrapping_add(b).wrapping_add(c)
        });
        self.pending_key = Some(key);
        r
    }

    /// Key send: verify against the pending key. A match unlocks; a miss
    /// answers `B0` (the ECU stays locked) — matching the decomp's
    /// `CBFA = 0xB0` error exit.
    ///
    /// Galletto sends a key that is exactly 5 lower than the WinKFP formula
    /// on all four bytes (its Z indexing uses a fixed -5 index offset which
    /// on the linear seed table shows up as a constant -5 bias). Accept
    /// either so both tools can unlock:
    ///   - WinKFP:   K[i] = FR[0x12+i] + FR[Z+i] + FR[0x29+i]   (mod 46)
    ///   - Galletto: K[i] = FR[0x12+i] + FR[Z+i] + FR[0x29+i] - 5
    fn key_send(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        let key: [u8; 4] = match p.get(3..7) {
            Some(k) => k.try_into().unwrap_or([0; 4]),
            None => return Self::nack(p),
        };
        match self.pending_key {
            Some(expected) => {
                let galletto_key = expected.map(|b| b.wrapping_sub(5));
                if expected == key || galletto_key == key {
                    self.pending_key = None;
                    Self::status(p, len_echo, SB_OK)
                } else {
                    Self::status(p, len_echo, 0xB0)
                }
            }
            _ => Self::status(p, len_echo, 0xB0),
        }
    }

    /// The simulated flash image (256 KB) — the binary reconstructed from
    /// everything the peer wrote/erased. Exposed so `respond` mode can dump
    /// the flasher's image after a run.
    pub fn flash_image(&self) -> &[u8] {
        &self.flash
    }

    /// The contiguous written span (first written byte .. last written byte,
    /// exclusive) if any data write happened; `None` otherwise. Used by
    /// `respond` to bound the reconstructed-binary dump.
    pub fn written_span(&self) -> Option<(u32, u32)> {
        self.written_span
    }

    /// Mark a flash span as written (called from `flash_write`).
    fn mark_written(&mut self, start: u32, len: usize) {
        if len == 0 {
            return;
        }
        let (lo, hi) = match self.written_span {
            Some((a, b)) => (a, b),
            None => (start, start + len as u32),
        };
        let s = lo.min(start);
        let e = hi.max(start + len as u32);
        self.written_span = Some((s, e));
    }

    /// MW live data. Sub-services (DS2 cmd `0x0B`):
    ///   0x10 MW_SELECT_LESEN — batched `12 <LEN> 0B 10 <id hi lo…>`,
    ///       reply `12 <LEN> A0 10 00 <u16 BE per id ...>` (status at [2], a
    ///       two-byte `10 00` header at [3..5], one big-endian u16 per requested
    ///       id from [5]);
    ///   0x03 MW measurement block — `12 05 0B 03` → header byte at [3] +
    ///       25 × u16 BE from [4..54] (id order = cal 0x3AF04 list);
    ///   0x04 digital input block — `12 05 0B 04` → header byte at [3] +
    ///       4 data bytes at [4..8];
    ///   0x11 PSG value read — `12 05 0B 11` → 2-byte header `11 00` at
    ///       [3..5] + u16 BE value;
    ///   0x12 PSG value select — `12 06 0B 12 NN` → status only;
    ///   0x07 STEUERN_TESTPLATZ — status only (accepted if unlocked).
    fn mw(&mut self, p: &[u8]) -> Vec<u8> {
        let sub = *p.get(3).unwrap_or(&0x00);
        if sub == 0x10 {
            let mut vals: Vec<u16> = Vec::new();
            for ch in p[4..].as_chunks::<2>().0 {
                let id = ((ch[0] as u16) << 8) | ch[1] as u16;
                vals.push(Self::mw_value(id));
            }
            let mut r: Vec<u8> = vec![ECU_ADDR, 0, SB_OK, 0x10, 0x00];
            for v in &vals {
                r.extend_from_slice(&v.to_be_bytes());
            }
            r[1] = (5 + vals.len() * 2 + 1) as u8; // total wire length incl trailing XOR
            return r;
        }
        match sub {
            // MW measurement block (0x03).
            0x03 => {
                let mut data = vec![0x00u8]; // header byte at [3]
                for i in 0..25usize {
                    data.extend_from_slice(&Self::mw_block_value(i).to_be_bytes());
                }
                let mut r = vec![ECU_ADDR, 0, SB_OK];
                r.extend_from_slice(&data);
                r
            }
            // Digital input block (0x04).
            0x04 => {
                let mut data = vec![0x00u8]; // header byte at [3]
                data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
                let mut r = vec![ECU_ADDR, 0, SB_OK];
                r.extend_from_slice(&data);
                r
            }
            // PSG value read (0x11): 2-byte header `11 00` at [3..5] + u16 BE.
            0x11 => {
                let v = Self::mw_value(0x0F65);
                let mut r = vec![ECU_ADDR, 0, SB_OK];
                r.push(0x11);
                r.push(0x00);
                r.push((v >> 8) as u8);
                r.push(v as u8);
                r
            }
            // PSG value select (0x12): status only.
            0x12 => vec![ECU_ADDR, 0, SB_OK],
            // STEUERN_TESTPLATZ (0x07): status only, accepted if unlocked.
            0x07 => {
                if self.pending_key.is_none() {
                    vec![ECU_ADDR, 0, SB_OK]
                } else {
                    Self::nack(p)
                }
            }
            _ => Self::nack(p),
        }
    }

    /// Deterministic measurement-block values (25 words, id order = cal
    /// `0x3AF04` measurement list; the fake ECU has no engine, so values are
    /// placeholder-but-plausible deterministic quantities).
    fn mw_block_value(i: usize) -> u16 {
        (0x1000u16)
            .wrapping_add(i as u16)
            .wrapping_mul(7919u16)
    }

    fn mw_value(id: u16) -> u16 {
        // Battery voltage (ADF id 0x0F65, scale 0.02037243 / LSB): a
        // plausible ~12.6 V instead of the generic pattern.
        if id == 0x0F65 {
            return 619;
        }
        (id as u32 * 7919u32 % 60000u32) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ident_and_status() {
        let mut s = SimEcu::new();
        // IDENT replies with the ident string in the normal envelope
        // (matches the live ECU: `12 2E A0 37 37 38 36 …`).
        // Wire = 3 header + 42 string + trailing XOR = 46 = LEN byte.
        let r = s.handle(&[0x12, 0x04, 0x00]);
        assert_eq!(&r[..3], &[0x12, 0x2E, 0xA0]);
        assert_eq!(r.len(), 45, "42-char string, LEN = payload + XOR");
        let info = crate::protocol::parse_ident(&r[3..]).expect("sim ident parses");
        assert_eq!(info.software, "7786887");
        assert_eq!(info.hw_number, "0281001445");
        let r = s.handle(&[0x12, 0x04, 0x9F]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);
        let r = s.handle(&[0x12, 0x04, 0x0D]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);
        // unknown service → NACK
        let r = s.handle(&[0x12, 0x04, 0x55]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xFF]);
    }

    #[test]
    fn seed_key_round_trip() {
        let mut s = SimEcu::new();
        // Seed request: `12 08 90 42 4D 57 Z`. Reply is the 45-byte payload
        // `12 2E A0 <42>` (LEN 0x2E = 46 wire incl. XOR).
        let z: u8 = 0x10;
        let seed_req: Vec<u8> = vec![0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, z];
        let r = s.handle(&seed_req);
        assert_eq!(r[1], 0x2E, "seed reply LEN = 46 wire");
        assert_eq!(r.len(), 45, "payload without trailing XOR");
        assert_eq!(r[2], 0xA0);

        // Compute expected key on the full 46-byte wire frame (payload + XOR)
        // with modulo-46 wrapping: K[i] = FR[(0x12+i)%46] + FR[(Z+i)%46] + FR[(0x29+i)%46]
        let xor = crate::protocol::xor_cs(&r);
        let mut frame = r.clone();
        frame.push(xor);
        assert_eq!(frame.len(), 46, "46-byte wire frame");
        const FRAME_SIZE: usize = 46;

        let mut key = [0u8; 4];
        for i in 0..4usize {
            let a = frame[(0x12 + i) % FRAME_SIZE];
            let b = frame[(z as usize + i) % FRAME_SIZE];
            let c = frame[(0x29 + i) % FRAME_SIZE];
            key[i] = a.wrapping_add(b).wrapping_add(c);
        }

        // Wrong key first: must be NACKed (3-byte status, byte at index 2)
        // and leave the ECU locked.
        let bad: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
        let wrong_req: Vec<u8> = [0x12, 0x08, 0x90, bad[0], bad[1], bad[2], bad[3]]
            .to_vec();
        let rw = s.handle(&wrong_req);
        assert_eq!(rw[2], 0xB0, "wrong key -> 0xB0");

        // Correct key unlocks.
        let key_req: Vec<u8> = [0x12, 0x08, 0x90, key[0], key[1], key[2], key[3]].to_vec();
        let rk = s.handle(&key_req);
        assert_eq!(rk[2], 0xA0, "correct key unlocks");

        // A further seed request (the tool retries the whole exchange)
        // answers a FRESH 46-byte seed telegram again — NOT a short ack — with
        // a fresh expected key, so a retried key send still validates.
        let r2 = s.handle(&seed_req);
        assert_eq!(r2[1], 0x2E, "retry seed request -> fresh 46-byte telegram");
        assert_eq!(r2.len(), 45);
        assert_eq!(r2[2], 0xA0);
        let xor2 = crate::protocol::xor_cs(&r2);
        let mut frame2 = r2.clone();
        frame2.push(xor2);
        let mut key2 = [0u8; 4];
        for i in 0..4usize {
            key2[i] = frame2[(0x12 + i) % FRAME_SIZE]
                .wrapping_add(frame2[(z as usize + i) % FRAME_SIZE])
                .wrapping_add(frame2[(0x29 + i) % FRAME_SIZE])
        }
        let key2_req: Vec<u8> = [0x12, 0x08, 0x90, key2[0], key2[1], key2[2], key2[3]].to_vec();
        let rk2 = s.handle(&key2_req);
        assert_eq!(rk2[2], 0xA0, "retried key unlocks");
    }

    #[test]
    fn aif_read_46_bytes_from_cal_7cb2() {
        let mut s = SimEcu::new();
        // The exact request the flashing tool sent: `12 05 02 00` (+XOR on
        // the wire). The param byte is unused by the ECU.
        let r = s.handle(&[0x12, 0x05, 0x02, 0x00]);
        // `12 32 A0 <46 B>`: LEN 0x32 = 50 = 3 header + 46 record + XOR.
        assert_eq!(&r[..3], &[0x12, 0x32, 0xA0]);
        assert_eq!(r.len(), 3 + AIF_LEN);
        // The in-car record, byte-for-byte (tunerpro 16-11-2026.bin @ 0x7CB2).
        assert_eq!(&r[3..], &AIF_RECORD);
        // The job's "AIF_NICHT_PROGRAMMIERT" check is resp[3] == 0xFF.
        assert_eq!(r[3], 0x20);
        // The DDE3 job's param-0xFF variant gives the same record.
        let r2 = s.handle(&[0x12, 0x05, 0x02, 0xFF]);
        assert_eq!(&r[1..], &r2[1..]);
        // Blank the slot in programming mode → empty-slot short reply.
        // Write to flash address 0x3FCB2 (= cal 0x7CB2, the AIF slot).
        s.handle(&[0x12, 0x04, 0x0D]);
        let mut w = vec![0x12, 0x0B, 0x07, 0x02, 0x03, 0xFC, 0xB2, AIF_LEN as u8];
        w.extend(std::iter::repeat(0xFF).take(AIF_LEN));
        assert_eq!(s.handle(&w)[2], 0xA0);
        let r5 = s.handle(&[0x12, 0x05, 0x02, 0x00]);
        assert_eq!(r5.len(), 4, "empty slot → 1 data byte");
        assert_eq!(r5[3], 0xFF, "the job's NICHT_PROGRAMMIERT byte");
    }

    #[test]
    fn flash_read_round_trip() {
        let mut s = SimEcu::new();
        // read 0x80 bytes @ addr 0: 12 09 06 00 <A_hi A_mid A_lo> <count@7>
        // LEN = payload + trailing XOR = 3 + 0x80 + 1 = 0x84.
        let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 0x80]);
        assert_eq!(&r[..3], &[0x12, 0x84, 0xA0]);
        assert_eq!(r.len(), 3 + 0x80);
        // write needs programming mode: 12 0B 07 02 <addr BE> <N=2> <AA BB>
        let r = s.handle(&[0x12, 0x0B, 0x07, 0x02, 0x00, 0x00, 0x00, 0x02, 0xAA, 0xBB]);
        assert_eq!(r[2], 0xFF);
        // enter prog, write, read back
        s.handle(&[0x12, 0x04, 0x0D]);
        let r = s.handle(&[0x12, 0x0B, 0x07, 0x02, 0x00, 0x00, 0x00, 0x02, 0xAA, 0xBB]);
        assert_eq!(r[2], 0xA0);
        let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 0x02]);
        assert_eq!(&r[3..5], &[0xAA, 0xBB]);
    }

    #[test]
    fn flash_read_serves_file_image() {
        // A real dump (e.g. captures/actual_flash.bin) is served verbatim:
        // FLASH_LESEN returns those bytes and the AIF slot comes from the
        // file, not from the built-in AIF_RECORD.
        let path =
            std::env::temp_dir().join(format!("dde3_sim_file_img_{}.bin", std::process::id()));
        let mut img = vec![0u8; IMAGE_SIZE];
        for (i, b) in img.iter_mut().enumerate() {
            *b = (i as u8) ^ 0xA5;
        }
        img[AIF_SLOT..AIF_SLOT + AIF_LEN].fill(0xC3);
        std::fs::write(&path, &img).unwrap();
        let mut s = SimEcu::from_flash_file(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        // The tool's actual FLASH_LESEN frame: 0x80 bytes @ 0.
        let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 0x80]);
        assert_eq!(&r[..3], &[0x12, 0x84, 0xA0]);
        assert_eq!(&r[3..3 + 0x80], &img[0x0000..0x0080]);
        // A mid-flash offset (the tool steps 0x80 per request).
        let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x03, 0x40, 0x00, 0x80]);
        assert_eq!(&r[3..3 + 0x80], &img[0x34000..0x34080]);
        // The AIF record now comes from the file.
        let r = s.handle(&[0x12, 0x05, 0x02, 0x00]);
        assert_eq!(&r[3..], &img[AIF_SLOT..AIF_SLOT + AIF_LEN]);
        assert_eq!(r[3], 0xC3);
    }

    #[test]
    fn flash_image_wrong_size_rejected() {
        let path = std::env::temp_dir().join(format!("dde3_sim_short_{}.bin", std::process::id()));
        std::fs::write(&path, vec![0u8; 4096]).unwrap();
        assert!(SimEcu::from_flash_file(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn erase_and_faults() {
        let mut s = SimEcu::new();
        s.handle(&[0x12, 0x04, 0x0D]);
        let r = s.handle(&[0x12, 0x09, 0x07, 0x06, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(r[2], 0xA0);
        let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 0x01]);
        assert_eq!(r[3], 0xFF);
        // fault clear is a plain OK
        let r = s.handle(&[0x12, 0x09, 0x07, 0x0E, 0x42, 0x41, 0x50, 0x00]);
        assert_eq!(r[2], 0xA0);
    }

    #[test]
    fn mw_live() {
        let mut s = SimEcu::new();
        let r = s.handle(&[0x12, 0x05, 0x0B, 0x10, 0x00, 0x01]);
        assert_eq!(r[0], 0x12);
        assert_eq!(r[2], 0xA0);
        let v = u16::from_be_bytes([r[5], r[6]]);
        assert_eq!(v, SimEcu::mw_value(1));
        // battery voltage: plausible ~12.6 V at the DDE3 scale
        let r = s.handle(&[0x12, 0x05, 0x0B, 0x10, 0x0F, 0x65]);
        let v = u16::from_be_bytes([r[5], r[6]]);
        let volt = v as f64 * 0.02037243;
        assert!((11.0..14.0).contains(&volt), "battery {volt} V");
    }

    #[test]
    fn mw_live_batch() {
        // Batched request: several ids in one telegram (the tested logger form).
        let mut s = SimEcu::new();
        let r = s.handle(&[0x12, 0x0D, 0x0B, 0x10, 0x0F, 0x63, 0x0F, 0x65, 0x0F, 0x10]);
        assert_eq!(r[0], 0x12);
        assert_eq!(r[2], 0xA0);
        assert_eq!(&r[3..5], &[0x10, 0x00]);
        // Three values, in request order, from offset 5.
        assert_eq!(r.len(), 5 + 3 * 2);
        let v0 = u16::from_be_bytes([r[5], r[6]]);
        let v1 = u16::from_be_bytes([r[7], r[8]]);
        let v2 = u16::from_be_bytes([r[9], r[10]]);
        assert_eq!(v0, SimEcu::mw_value(0x0F63));
        assert_eq!(v1, SimEcu::mw_value(0x0F65));
        assert_eq!(v2, SimEcu::mw_value(0x0F10));
    }

    #[test]
    fn fault_memory_sequence() {
        let mut s = SimEcu::new();
        // count: 3 seeded faults + header (rec 0..22) of fault 1.
        // LEN = payload + trailing XOR: 26 + 1 = 0x1B, exactly the real-car
        // DTC reply `12 1B A0 02 …` in captures/dde3newlog.txt.
        let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        assert_eq!(&r[..4], &[0x12, 0x1B, 0xA0, 3]);
        assert_eq!(r.len(), 3 + 1 + 22);
        assert_eq!(&r[4..6], &[0x26, 0x02]); // ORT + ART mask of fault 1
        // entry 1: `A0 <pos> <rec 22..26>` — LEN 8 + 1 = 9
        let r = s.handle(&[0x12, 0x05, 0x04, 0x00]);
        assert_eq!(&r[..4], &[0x12, 0x09, 0xA0, 1]);
        assert_eq!(r.len(), 3 + 1 + 4);
        // shadow 1: `A0 <pos> <rec 26..37>` — LEN 15 + 1 = 0x10
        let r = s.handle(&[0x12, 0x05, 0x04, 0x01]);
        assert_eq!(&r[..4], &[0x12, 0x10, 0xA0, 1]);
        assert_eq!(r.len(), 3 + 1 + 11);
        // tail 1: `A0 <anz=2> <rec 37..50> <next rec 0..22>` — LEN 39 + 1 = 0x28
        let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        assert_eq!(&r[..4], &[0x12, 0x28, 0xA0, 2]);
        assert_eq!(r.len(), 3 + 1 + 13 + 22);
        assert_eq!(&r[17..19], &[0x05, 0x01]); // fault 2 ORT + mask
        // fault 2 entry/shadow, tail announces fault 3
        s.handle(&[0x12, 0x05, 0x04, 0x00]);
        s.handle(&[0x12, 0x05, 0x04, 0x01]);
        let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        assert_eq!(r[3], 1);
        assert_eq!(&r[17..19], &[0x36, 0x04]); // fault 3 ORT + mask
        s.handle(&[0x12, 0x05, 0x04, 0x00]);
        s.handle(&[0x12, 0x05, 0x04, 0x01]);
        // last tail: `A0 <anz=0> <rec 37..50>` only — LEN 17 + 1 = 0x12
        let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        assert_eq!(&r[..4], &[0x12, 0x12, 0xA0, 0]);
        assert_eq!(r.len(), 3 + 1 + 13);
        // DS2 clear — LEN 3 + 1 = 4
        let r = s.handle(&[0x12, 0x04, 0x05]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);
        // empty count: `A0 <anz=0>` — LEN 4 + 1 = 5
        let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        assert_eq!(&r[..4], &[0x12, 0x05, 0xA0, 0]);
    }

    #[test]
    fn fault_replay_assembles_full_records() {
        // Walk the FS_LESEN sequence exactly like `Kline::read_dtcs` does
        // (same chunk slices) and verify each 50-byte record round-trips.
        let mut s = SimEcu::new();
        let c = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        let data = &c[3..];
        let count = data[0] as usize;
        let mut header = data[1..23].to_vec();
        let mut left = count;
        let mut assembled = Vec::new();
        while left > 0 {
            let mut rec = header.clone();
            let e = s.handle(&[0x12, 0x05, 0x04, 0x00]);
            rec.extend_from_slice(e.get(4..8).unwrap_or(&[]));
            let sh = s.handle(&[0x12, 0x05, 0x04, 0x01]);
            rec.extend_from_slice(sh.get(4..15).unwrap_or(&[]));
            let t = s.handle(&[0x12, 0x05, 0x14, 0x01]);
            let d2 = &t[3..];
            rec.extend_from_slice(d2.get(1..14).unwrap_or(&[]));
            let next = d2.first().copied().unwrap_or(0) as usize;
            assembled.push(rec);
            left -= 1;
            if next == 0 || left == 0 {
                break;
            }
            if d2.len() >= 14 + 22 {
                header = d2[14..14 + 22].to_vec();
            } else {
                break;
            }
        }
        let exp = [
            (0x26u8, 0x02u8, 0x012Cu16, 0x0096u16),
            (0x05, 0x01, 0x04D2, 0x021F),
            (0x36, 0x04, 0x02BC, 0x03A5),
        ];
        assert_eq!(assembled.len(), 3);
        for (i, rec) in assembled.iter().enumerate() {
            assert_eq!(rec.len(), 50);
            assert_eq!(rec[0], exp[i].0, "ORT {i}");
            assert_eq!(rec[1], exp[i].1, "ART mask {i}");
            assert_eq!(&rec[3..5], &exp[i].2.to_be_bytes(), "uw1 {i}");
            assert_eq!(&rec[5..7], &exp[i].3.to_be_bytes(), "uw2 {i}");
            let d = crate::protocol::Dtc::from_record(rec);
            assert_eq!(d.ort, exp[i].0);
            assert_eq!(d.uw[0], exp[i].2);
            assert_eq!(d.uw[1], exp[i].3);
        }
    }

    #[test]
    fn missing_ds2_responses_are_answered() {
        // Every DS2 service the foreign tools use must get a well-formed
        // reply (JOBRESULT status + LEN = payload + trailing XOR) instead of
        // falling through to the `12 04 FF` NACK — a NACK mid-session made
        // the flashing tool stall waiting for a response it will never get.
        let mut s = SimEcu::new();

        // DS2 session keep (9E) / end (9F): status only.
        assert_eq!(s.handle(&[0x12, 0x04, 0x9E])[2], 0xA0);
        assert_eq!(s.handle(&[0x12, 0x04, 0x9F])[2], 0xA0);

        // DS2 test stamp read (0E): status + 3 bytes, LEN = payload + XOR = 7.
        let r = s.handle(&[0x12, 0x04, 0x0E]);
        assert_eq!(&r[..3], &[0x12, 0x07, 0xA0]);
        assert_eq!(r.len(), 6);
        assert_eq!(&r[3..6], &[0, 0, 0]);

        // DS2 test stamp write (0F): status only.
        let r = s.handle(&[0x12, 0x04, 0x0F]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);

        // DS2 actuator override (0C): status only.
        let r = s.handle(&[0x12, 0x08, 0x0C, 0x0F, 0x65, 0x05, 0x00]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);

        // DS2 EWS sync start (6C) / status (6D): status only.
        assert_eq!(s.handle(&[0x12, 0x05, 0x6C, 0x00])[2], 0xA0);
        assert_eq!(s.handle(&[0x12, 0x04, 0x6D])[2], 0xA0);

        // DS2 adjustment value read (40): status + u16 BE value, LEN = 6.
        let r = s.handle(&[0x12, 0x05, 0x40, 0x07]);
        assert_eq!(&r[..3], &[0x12, 0x06, 0xA0]);
        assert_eq!(r.len(), 5);
        assert_eq!(&r[3..5], &[0x00, 0x00]);

        // DS2 adjustment value set (41): status only; the read returns the
        // stored value.
        assert_eq!(s.handle(&[0x12, 0x07, 0x41, 0x07, 0x00, 0x2A])[2], 0xA0);
        let r = s.handle(&[0x12, 0x05, 0x40, 0x07]);
        assert_eq!(&r[3..5], &[0x00, 0x2A]);
        // DS2 adjustment value store → EEPROM (42): status only.
        assert_eq!(s.handle(&[0x12, 0x05, 0x42, 0x07])[2], 0xA0);
    }

    #[test]
    fn mw_subservices_answered() {
        let mut s = SimEcu::new();

        // MW measurement block (0x03): LEN = payload(54) + XOR = 55 = 0x37,
        // header byte at [3] + 25 u16 BE from [4..54].
        let r = s.handle(&[0x12, 0x05, 0x0B, 0x03]);
        assert_eq!(&r[..3], &[0x12, 0x37, 0xA0]);
        assert_eq!(r.len(), 54);
        assert_eq!(r[3], 0x00);
        assert_eq!(r.len(), 3 + 1 + 25 * 2);

        // Digital input block (0x04): LEN = payload(8) + XOR = 9 = 0x09,
        // header byte at [3] + 4 data bytes at [4..8].
        let r = s.handle(&[0x12, 0x05, 0x0B, 0x04]);
        assert_eq!(&r[..3], &[0x12, 0x09, 0xA0]);
        assert_eq!(r.len(), 8);
        assert_eq!(r.len(), 3 + 1 + 4);

        // PSG value read (0x11): LEN = payload(7) + XOR = 8 = 0x08, 2-byte
        // header `11 00` at [3..5] + u16 BE value.
        let r = s.handle(&[0x12, 0x05, 0x0B, 0x11]);
        assert_eq!(&r[..3], &[0x12, 0x08, 0xA0]);
        assert_eq!(&r[3..5], &[0x11, 0x00]);

        // PSG value select (0x12): status only.
        let r = s.handle(&[0x12, 0x05, 0x0B, 0x12, 0x00]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);

        // STEUERN_TESTPLATZ: status only when unlocked (fresh sim has no
        // pending seed → unlocked). Frame: `12 0B 07 10 …` (addr 12, len 0B,
        // sid 07 write-type, sub 10).
        let r = s.handle(&[0x12, 0x0B, 0x07, 0x10, 0x00, 0x00, 0x00, 0x02, 0x4B, 0x9A]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);
    }
}
