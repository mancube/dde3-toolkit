//! DDE3 ECU simulator — the state machine behind the `respond` fake ECU.
//!
//! `SimEcu` answers DS2 frames exactly like the real ECU does (see
//! `src/respond.rs` for the wire front end: it feeds every received frame
//! into [`SimEcu::handle`] and transmits the returned reply). It is the
//! only simulator in the toolkit — the old in-process `--sim` flag that
//! injected replies into `Kline::request()` is gone.
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
/// NACK stub status of the flash services (0x06/0x07) outside the unlocked
/// programming session — the real ECU answers `12 04 A2` (2026-11-19
/// capture), not the generic 0xFF NACK.
const SB_NACK_FLASH: u8 = 0xA2;

/// The ECU's ident string — IDENT (0x00) and the SEED_KEY seed telegram (0x90)
/// both return these 42 bytes verbatim (real car, 2026-11-19 capture
/// `captures/dde3-20261119-222226.log`: seed reply == ident reply, byte for
/// byte; the key is derived from this string, Z = 0x07 → `9C A0 97 96`).
const IDENT_DATA: [u8; 42] = *b"77868875201124048010281001445G521000000000";

/// UPROG_EIN reply data — the ECU's region/segment descriptor table,
/// byte-for-byte from the real car (2026-11-19, `captures/actual ecu
/// UPROG_EIN response.txt`): `03 FF 60` + 42 × FF + the 27-byte tail `03 AE
/// BE 00 7F E4 … 03 FF 62`, whose last 24 bytes are the flash-region table
/// stored in flash at 0x3FC9A..0x3FCB2 (right before the AIF slot at
/// 0x3FCB2). The ECU assembles the reply (the FF padding and the `03 AE BE`
/// triple are not a pure flash window), so the sim serves the captured bytes
/// verbatim. Reply frame: `12 4C A0 <72 B>` (LEN 0x4C = 76 wire incl. XOR).
const UPROG_EIN_DATA: [u8; 72] = [
    0x03, 0xFF, 0x60, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x03, 0xAE, 0xBE,
    0x00, 0x7F, 0xE4, 0x03, 0xFF, 0x46, 0x03, 0xFF, 0x48, 0x03, 0x7F, 0xB0,
    0x03, 0xFC, 0xB2, 0x03, 0xFF, 0x47, 0x03, 0xFC, 0x4F, 0x03, 0xFF, 0x62,
];

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
    /// Test hook: override the AIF (hardware-ID) programming record served
    /// on the `FLASH_LESEN addr=0 count=46` read; `None` = serve from flash.
    aif_override: Option<[u8; 46]>,
    /// Seed/key accepted (the real ECU's flash-session unlock latch). The
    /// flash services (0x06/0x07) NACK with status 0xA2 until UPROG_EIN
    /// (`in_prog`) AND seed/key (`unlocked`) are both done — verified on the
    /// real car 2026-11-19 (`captures/actual ecu UPROG_EIN response.txt`:
    /// UPROG_EIN done, no seed/key → FLASH_LESEN → `12 04 A2`). Cleared by
    /// DIAGNOSE_ENDE (0x9F).
    unlocked: bool,
    /// 0x91 baud patch (the `respond --patched-baud` flag): the link rate
    /// the fake ECU switches to when it accepts a `0x91` — the patch's FIXED
    /// rate, already mapped through `protocol::reopen_divisor` (a 125000
    /// patch runs at 124800, the S0BG divisor). `None` = no patch: the 0x91
    /// stub ACK stays (so a plain `respond` still answers 0x91) and nothing
    /// switches. Like the shipped v1 patch images, the frame's baud field
    /// is ignored — only the patch's fixed rate is used.
    baud_patch: Option<u32>,
    /// Pending 0x91 link switch for the transport to apply: set by the 0x91
    /// handler, consumed once via [`SimEcu::take_baud_switch`]. `respond`
    /// reopens the port at this rate BEFORE transmitting the ACK, so the
    /// ACK rides the line at the new rate — the real patch's wire semantics
    /// (S0BG is written before the ACK is transmitted).
    baud_switch: Option<u32>,
}

impl Default for SimEcu {
    fn default() -> Self {
        Self::new()
    }
}

impl SimEcu {
    /// The simulated fault memory: exactly ONE stored fault.
    ///
    /// The M47 the tools were built against ran with a single live error —
    /// the real one, per the INPA reference read (`analysis/actual error.txt` +
    /// `protocol::dtc_from_record_reference_m47`): ORT `0x08` (Gluehanlage),
    /// ART mask `0xC8`, error frequency `60`, Errorcode `08 C8 3C …`. The
    /// fake ECU therefore serves that record verbatim; the FS_LESEN sequence
    /// (count/entry/shadow/tail) walks it cleanly and the DS2 clear (`12 04
    /// 05`) empties the memory again (next read -> `12 05 A0 <anz=0>`).
    fn default_faults() -> Vec<[u8; 50]> {
        vec![[
            0x08, 0xC8, 0x3C, 0x00, 0x38, 0x9C, 0xE3, 0x00, 0x37, 0x9D, 0xDE, 0xC4, 0xC1, 0x04,
            0x00, 0x48, 0x00, 0x99, 0x03, 0x00, 0x76, 0xDC, 0x5B, 0x03, 0x4A, 0x00, 0x76, 0xD1,
            0x7D, 0x00, 0x76, 0xDC, 0x5A, 0x00, 0x34, 0x32, 0x33, 0x32, 0x34, 0x00, 0x30, 0x21,
            0x00, 0x00, 0x00, 0x31, 0xC5, 0x00, 0x00, 0x22,
        ]]
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
            aif_override: None,
            unlocked: false,
            baud_patch: None,
            baud_switch: None,
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
            aif_override: None,
            unlocked: false,
            baud_patch: None,
            baud_switch: None,
        })
    }

    /// Test hook: set the AIF (hardware-ID) programming record served on the
    /// `FLASH_LESEN addr=0 count=46` read (content-matching tests against
    /// foreign tools such as MPPS).
    #[allow(dead_code)]
    pub fn set_aif(&mut self, aif: [u8; 46]) {
        self.aif_override = Some(aif);
    }

    /// Configure the sim like the patched real ECU (`respond
    /// --patched-baud`): a `0x91` then switches the link to `rate` and the
    /// ACK goes out at the NEW rate (the transport applies the switch via
    /// [`SimEcu::take_baud_switch`] before transmitting the reply). `rate`
    /// is the patch's fixed rate — the ECU's actual divisor, not the
    /// nominal request (125000 → 124800), as [`crate::protocol::reopen_divisor`
    /// computes it.
    pub fn set_baud_patch(&mut self, rate: u32) {
        self.baud_patch = Some(rate);
    }

    /// The 0x91 link switch pending since the last [`SimEcu::handle`] —
    /// `None` if the last frame did not switch the baud. Consumed once; the
    /// transport (respond) reopens its port at the rate before sending the
    /// ACK, mirroring the real patch (S0BG write, then ACK at the new rate).
    pub fn take_baud_switch(&mut self) -> Option<u32> {
        self.baud_switch.take()
    }

    /// [`SimEcu::with_flash_image`] from a file on disk.
    pub fn from_flash_file(path: &std::path::Path) -> Result<Self> {
        let image = std::fs::read(path)
            .with_context(|| format!("cannot read flash image {}", path.display()))?;
        Self::with_flash_image(image)
            .with_context(|| format!("flash image {}", path.display()))
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
                let mut r = vec![0x12, 0, SB_OK];
                r.extend_from_slice(&IDENT_DATA);
                r
            }
            // UPROG_EIN: `12 04 0D` — enter programming mode. Real car
            // replies `12 4C A0` + the 72-byte region/segment descriptor
            // table (2026-11-19 capture) — not a bare status.
            0x0D => {
                self.in_prog = true;
                let mut r = vec![0x12, 0, SB_OK];
                r.extend_from_slice(&UPROG_EIN_DATA);
                r
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
            // DIAGNOSE_ENDE: `12 04 9F` — leave session (and re-lock the
            // flash services).
            0x9F => {
                self.in_prog = false;
                self.unlocked = false;
                Self::status(p, len_echo, SB_OK)
            }
            // SEED_KEY family: seed request `12 08 90 42 4D 57 Z` vs key
            // send `12 08 90 K0 K1 K2 K3` — see `seed_key` (M47 decomp
            // sub_028B5E, DDE3 SEED_KEY job reconstruction).
            0x90 => self.seed_key(p, len_echo),
            // BAUD change (0x91 patch): `12 08 91 00 <baud BE> <p2>`.
            // With `set_baud_patch` (respond --patched-baud) the fake ECU
            // behaves like the patched real ECU: the link switches to the
            // patch's fixed rate and the ACK goes out at the NEW rate (the
            // patch writes S0BG before the ACK is transmitted) — the switch
            // is reported to the transport via `take_baud_switch()`. The
            // frame's baud field is ignored, like the shipped v1 patches.
            // Without the patch the stub ACK is kept (real stock firmware
            // NACKs this).
            0x91 => {
                if let Some(rate) = self.baud_patch {
                    self.baud_switch = Some(rate);
                }
                Self::status(p, len_echo, SB_OK)
            },
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

    /// True when the real ECU's flash session is open: UPROG_EIN done AND
    /// seed/key accepted. Until then 0x06/0x07 are NACK stubs (0xA2) —
    /// verified on the real car 2026-11-19 (UPROG_EIN done, no seed/key →
    /// FLASH_LESEN → `12 04 A2`).
    fn flash_session_open(&self) -> bool {
        self.in_prog && self.unlocked
    }

    fn flash_read(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        // `12 09 06 00 <A_hi A_mid A_lo> <count@7>` — count is 1 byte at [7].
        if !self.flash_session_open() {
            return Self::status(p, 4, SB_NACK_FLASH);
        }
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
                let content: &[u8] = self
                    .aif_override
                    .as_ref()
                    .map(|o| o.as_slice())
                    .unwrap_or(&self.flash[AIF_SLOT..AIF_SLOT + AIF_LEN]);
                r.extend_from_slice(content);
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
        if !self.flash_session_open() {
            return Self::status(p, 4, SB_NACK_FLASH);
        }
        if let Some(addr) = Self::addr_be(p) {
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
        if !self.flash_session_open() {
            return Self::status(p, 4, SB_NACK_FLASH);
        }
        if let (Some(addr), Some(n)) = (Self::addr_be(p), p.get(7).copied()) {
            let data = p.get(8..8 + n as usize).unwrap_or(&[]);
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
        // Real ECU: the seed telegram is the ident string, verbatim
        // (2026-11-19 capture — seed reply == ident reply; the expected key
        // below is computed on these exact bytes, so Z = 0x07 → 9C A0 97 96
        // like the real car).
        r.extend_from_slice(&IDENT_DATA);
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
                    self.unlocked = true;
                    // Real car: key check OK is `12 05 A0 00` (status + one
                    // 0x00 data byte, 2026-11-19 capture).
                    let mut r = Self::status(p, len_echo, SB_OK);
                    r.push(0x00);
                    r
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
mod tests;
