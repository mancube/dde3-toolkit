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
//! (data from offset 5), and IDENT replies with a bare ASCII string.
//! The ECU also emits a periodic ~16-byte live block on the K-line; the
//! toolkit's `is_not_live_broadcast` skips it for the small fault/battery
//! replies.

use crate::protocol::{ECU_ADDR, IMAGE_SIZE};

const SB_OK: u8 = 0xA0;

pub struct SimEcu {
    flash: Vec<u8>,
    in_prog: bool,
    /// Stored fault memory as full 50-byte fault records (FS_LESEN).
    faults: Vec<[u8; 50]>,
    /// Index of the fault currently being read (FS_LESEN pointer).
    fault_pos: usize,
    /// FS_LESEN step: 0=count, 1=entry, 2=shadow, 3=tail.
    fault_step: u8,
}

impl Default for SimEcu {
    fn default() -> Self {
        Self::new()
    }
}

impl SimEcu {
    /// Fresh sim with a deterministic 256 KB pattern (not zeros, so an
    /// un-erased flash is distinguishable from a written one).
    pub fn new() -> Self {
        let flash: Vec<u8> = (0..IMAGE_SIZE)
            .map(|i| {
                let v = i.wrapping_mul(7).wrapping_add(0x5A);
                (v ^ (i >> 8)) as u8
            })
            .collect();
        Self {
            flash,
            in_prog: false,
            faults: vec![
                Self::record(0, 0x26, 0x02, 0x012C, 0x0096), // Luftmassenmesser, Signal Unterbrechung
                Self::record(1, 0x05, 0x01, 0x04D2, 0x021F), // Nadelbewegungsfuehler, KS B+
                Self::record(2, 0x36, 0x04, 0x02BC, 0x03A5), // Ladedruckfuehler, nicht plausibel
            ],
            fault_pos: 0,
            fault_step: 0,
        }
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
        if p.len() < 3 || p[0] != ECU_ADDR {
            return Self::nack(p);
        }
        let len_echo = p[1];
        let sid = p[2];
        match sid {
            // IDENT: `12 04 00` — the real ECU answers with a bare ASCII
            // string (no 0x12 envelope), e.g. `8875201` / `DDE 3.0 fuer M47`.
            0x00 => b"8875201".to_vec(),
            // UPROG_EIN: `12 04 0D` — enter programming mode.
            0x0D => {
                self.in_prog = true;
                Self::status(p, len_echo, SB_OK)
            }
            // STATUS_CODIER_CHECKSUMME: `12 04 0A` — TDI V3.1 app checksum.
            0x0A => {
                let mut r = Self::status(p, len_echo, SB_OK);
                let cs = crate::checksum::verify(&self.flash)
                    .into_iter()
                    .find(|b| b.name == "calibration")
                    .map(|b| b.stored)
                    .unwrap_or(0);
                r.extend_from_slice(&cs.to_be_bytes());
                r
            }
            // DIAGNOSE_ENDE: `12 04 9F` — leave session.
            0x9F => {
                self.in_prog = false;
                Self::status(p, len_echo, SB_OK)
            }
            // SEED_KEY: `12 08 90 42 4D 57 05 <Z>` — accept, unlock.
            0x90 => Self::status(p, len_echo, SB_OK),
            // FLASH_LESEN: `12 09 06 <A lo/mid/hi> <count>`.
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
        if self.fault_step != 1 || self.fault_pos >= self.faults.len() {
            return Self::status(p, len_echo, 0xB2);
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

    /// Address from a `12 09 06 <A lo/mid/hi> <n>` frame (FLASH_LESEN).
    fn addr_of(p: &[u8]) -> Option<u32> {
        let (a, b, c) = (*p.get(3)?, *p.get(4)?, *p.get(5)?);
        Some(a as u32 | (b as u32) << 8 | (c as u32) << 16)
    }

    /// Address from a `12 09 07 <sub> <A lo/mid/hi> ...` frame (the sub-code
    /// sits at p[3], so the address is shifted one byte right of FLASH_LESEN).
    fn addr_of7(p: &[u8]) -> Option<u32> {
        let (a, b, c) = (*p.get(4)?, *p.get(5)?, *p.get(6)?);
        Some(a as u32 | (b as u32) << 8 | (c as u32) << 16)
    }

    fn flash_read(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        if let (Some(addr), Some(count)) = (Self::addr_of(p), p.get(6).copied()) {
            if addr + count as u32 > IMAGE_SIZE as u32 {
                return Self::status(p, len_echo, 0xB0); // ERROR_SG_PARAMETER
            }
            let mut r = Self::status(p, len_echo, SB_OK);
            r.extend_from_slice(&self.flash[addr as usize..addr as usize + count as usize]);
            r
        } else {
            Self::nack(p)
        }
    }

    fn flash_erase(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        if let (Some(addr), Some(count)) = (Self::addr_of7(p), p.get(7).copied()) {
            if !self.in_prog {
                return Self::nack(p);
            }
            // count 0x00 = "erase from addr to end of image" (matches the
            // toolkit's write flow: erase(0,0) full, erase(0x38000,0) cal).
            let end = if count == 0 {
                IMAGE_SIZE as u32
            } else {
                (addr + count as u32).min(IMAGE_SIZE as u32)
            };
            if addr >= IMAGE_SIZE as u32 {
                return Self::status(p, len_echo, 0xB0);
            }
            self.flash[addr as usize..end as usize].fill(0xFF);
            Self::status(p, len_echo, SB_OK)
        } else {
            Self::nack(p)
        }
    }

    fn flash_write(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        if let Some(addr) = Self::addr_of7(p) {
            if !self.in_prog {
                return Self::nack(p);
            }
            let data = &p[7..];
            let end = addr as usize + data.len();
            if end > IMAGE_SIZE {
                return Self::status(p, len_echo, 0xB0);
            }
            self.flash[addr as usize..end].copy_from_slice(data);
            Self::status(p, len_echo, SB_OK)
        } else {
            Self::nack(p)
        }
    }

    /// MW_SELECT_LESEN `12 <len> 0B 10 <id hi lo ...>` and MW status
    /// `12 <len> 0B 03`. The real ECU replies
    /// `12 <len> A0 10 00 <u16 BE per id ...>` (status at [2], a two-byte
    /// `10 00` header at [3..5], one big-endian u16 per requested id from [5]).
    fn mw(&mut self, p: &[u8]) -> Vec<u8> {
        let sub = *p.get(3).unwrap_or(&0x00);
        let mut vals: Vec<u16> = Vec::new();
        if sub == 0x10 {
            for ch in p[4..].chunks_exact(2) {
                let id = ((ch[0] as u16) << 8) | ch[1] as u16;
                vals.push(Self::mw_value(id));
            }
        } else {
            // MW status (0x03): a few representative values.
            for id in [1u16, 2, 3, 4] {
                vals.push(Self::mw_value(id));
            }
        }
        let mut r: Vec<u8> = vec![ECU_ADDR, 0, SB_OK, 0x10, 0x00];
        for v in &vals {
            r.extend_from_slice(&v.to_be_bytes());
        }
        r[1] = (5 + vals.len() * 2 + 1) as u8; // total wire length incl trailing XOR
        r
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
        // IDENT replies with a bare ASCII string (no 0x12 envelope).
        let r = s.handle(&[0x12, 0x04, 0x00]);
        assert_eq!(&r[..], b"8875201");
        let r = s.handle(&[0x12, 0x04, 0x9F]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);
        let r = s.handle(&[0x12, 0x04, 0x0D]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);
        // unknown service → NACK
        let r = s.handle(&[0x12, 0x04, 0x55]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xFF]);
    }

    #[test]
    fn flash_read_round_trip() {
        let mut s = SimEcu::new();
        let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x80]);
        assert_eq!(&r[..3], &[0x12, 0x09, 0xA0]);
        assert_eq!(r.len(), 3 + 0x80);
        // write needs programming mode
        let r = s.handle(&[0x12, 0x09, 0x07, 0x02, 0x00, 0x00, 0x00, 0xAA, 0xBB]);
        assert_eq!(r[2], 0xFF);
        // enter prog, write, read back
        s.handle(&[0x12, 0x04, 0x0D]);
        let r = s.handle(&[0x12, 0x09, 0x07, 0x02, 0x00, 0x00, 0x00, 0xAA, 0xBB]);
        assert_eq!(r[2], 0xA0);
        let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x02]);
        assert_eq!(&r[3..5], &[0xAA, 0xBB]);
    }

    #[test]
    fn erase_and_faults() {
        let mut s = SimEcu::new();
        s.handle(&[0x12, 0x04, 0x0D]);
        let r = s.handle(&[0x12, 0x09, 0x07, 0x06, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(r[2], 0xA0);
        let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x01]);
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
        // count: 3 seeded faults + header (rec 0..22) of fault 1
        let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        assert_eq!(&r[..4], &[0x12, 0x05, 0xA0, 3]);
        assert_eq!(r.len(), 3 + 1 + 22);
        assert_eq!(&r[4..6], &[0x26, 0x02]); // ORT + ART mask of fault 1
        // entry 1: `A0 <pos> <rec 22..26>`
        let r = s.handle(&[0x12, 0x05, 0x04, 0x00]);
        assert_eq!(&r[..4], &[0x12, 0x05, 0xA0, 1]);
        assert_eq!(r.len(), 3 + 1 + 4);
        // shadow 1: `A0 <pos> <rec 26..37>`
        let r = s.handle(&[0x12, 0x05, 0x04, 0x01]);
        assert_eq!(&r[..4], &[0x12, 0x05, 0xA0, 1]);
        assert_eq!(r.len(), 3 + 1 + 11);
        // tail 1: `A0 <anz=2> <rec 37..50> <next rec 0..22>`
        let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        assert_eq!(&r[..4], &[0x12, 0x05, 0xA0, 2]);
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
        // last tail: `A0 <anz=0> <rec 37..50>` only
        let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        assert_eq!(&r[..4], &[0x12, 0x05, 0xA0, 0]);
        assert_eq!(r.len(), 3 + 1 + 13);
        // DS2 clear
        let r = s.handle(&[0x12, 0x04, 0x05]);
        assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);
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
}
