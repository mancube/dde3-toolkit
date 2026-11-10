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
//! Reply convention (derived from `frames.md`: "Generic OK response fed to
//! each job: `92 04 A0`" and the FLASH_LESEN listing in
//! `decompiled_winkfp.txt`, which reads the status byte at reply offset 2
//! and copies the data from reply offset 3):
//!
//! ```text
//! <92> <len echo> <SB> [<data ...>]   + trailing XOR on the wire
//! ```
//!
//! `SB` is the JOBRESULT status byte (0xA0 OKAY, 0xA1 BUSY, 0xFF NACK, …).
//! MW live data (`0x0B`) uses the echo style
//! `92 05 0B <sub> A0 <u16 values...>` to match `Kline::live_values`.

use crate::protocol::{ECU_ADDR, IMAGE_SIZE};

const SB_OK: u8 = 0xA0;

pub struct SimEcu {
    flash: Vec<u8>,
    in_prog: bool,
    /// Stored fault memory: `(ORT, ART, UW1 raw, UW2 raw)`.
    faults: Vec<(u8, u8, u16, u16)>,
    /// Read pointer into the fault memory (advanced by entry/shadow
    /// requests, like the real ECU's FS_LESEN pointer).
    fault_pos: usize,
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
                (0x26, 0x02, 0x012C, 0x0096), // Luftmassenmesser, Signal Unterbrechung
                (0x05, 0x01, 0x04D2, 0x021F), // Nadelbewegungsfuehler, KS B+
                (0x36, 0x04, 0x02BC, 0x03A5), // Ladedruckfuehler, nicht plausibel
            ],
            fault_pos: 0,
        }
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
            // IDENT: `12 04 00` — reply carries the ECU name.
            0x00 => {
                let mut r = Self::status(p, len_echo, SB_OK);
                r.extend_from_slice(b"DDE3.0 SIM ECU");
                r
            }
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
                    Self::status(p, len_echo, SB_OK)
                }
                _ => Self::nack(p),
            },
            // MW live data: `12 05 0B <sub> [ids...]`.
            0x0B => self.mw(p),
            // FS_LESEN count/start: `12 05 14 01` → `92 05 A0 <anz>`.
            0x14 if p.get(3) == Some(&0x01) => {
                self.fault_pos = 0;
                let mut r = Self::status(p, len_echo, SB_OK);
                r.push(self.faults.len() as u8);
                r
            }
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
                Self::status(p, len_echo, SB_OK)
            }
            _ => Self::nack(p),
        }
    }

    /// FS_LESEN entry: `12 05 04 00` — the next fault in memory.
    fn fault_entry(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        match self.faults.get(self.fault_pos) {
            Some((ort, art, uw1, uw2)) => {
                self.fault_pos += 1;
                let mut r = Self::status(p, len_echo, SB_OK);
                r.extend_from_slice(&[*ort, *art]);
                r.extend_from_slice(&uw1.to_be_bytes());
                r.extend_from_slice(&uw2.to_be_bytes());
                r
            }
            None => Self::status(p, len_echo, 0xB2), // ERROR_SG_NUMBER
        }
    }

    /// FS_SHADOW_LESEN: `12 05 04 01` — the freeze frame of the fault that
    /// was just read (11 big-endian u16 values, one per FUmweltTexte row).
    fn fault_shadow(&mut self, p: &[u8], len_echo: u8) -> Vec<u8> {
        let idx = self.fault_pos.saturating_sub(1);
        let base = self.faults.get(idx).map(|f| f.2).unwrap_or(0);
        let mut r = Self::status(p, len_echo, SB_OK);
        for i in 0..11u16 {
            let v = if i == 0 {
                0
            } else {
                base.wrapping_add(i.wrapping_mul(37)).wrapping_add(100)
            };
            r.extend_from_slice(&v.to_be_bytes());
        }
        r
    }

    // --- helpers ---------------------------------------------------------

    fn status(_p: &[u8], len_echo: u8, sb: u8) -> Vec<u8> {
        vec![ECU_ADDR | 0x80, len_echo, sb]
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

    /// MW_SELECT_LESEN `12 05 0B 10 <id hi lo ...>` and MW status
    /// `12 05 0B 03`. Reply: `92 05 0B <sub> A0 <u16 BE per id>`.
    fn mw(&mut self, p: &[u8]) -> Vec<u8> {
        let sub = *p.get(3).unwrap_or(&0x00);
        let mut r = vec![ECU_ADDR | 0x80, p[1], 0x0B, sub, SB_OK];
        if sub == 0x10 {
            let mut ids = p[4..].chunks_exact(2);
            for ch in &mut ids {
                let id = ((ch[0] as u16) << 8) | ch[1] as u16;
                let v = Self::mw_value(id);
                r.extend_from_slice(&v.to_be_bytes());
            }
        } else {
            // Fixed block: a few representative values.
            for id in [1u16, 2, 3, 4] {
                r.extend_from_slice(&Self::mw_value(id).to_be_bytes());
            }
        }
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
        let r = s.handle(&[0x12, 0x04, 0x00]);
        assert_eq!(&r[..3], &[0x92, 0x04, 0xA0]);
        let r = s.handle(&[0x12, 0x04, 0x9F]);
        assert_eq!(&r[..3], &[0x92, 0x04, 0xA0]);
        let r = s.handle(&[0x12, 0x04, 0x0D]);
        assert_eq!(&r[..3], &[0x92, 0x04, 0xA0]);
        // unknown service → NACK
        let r = s.handle(&[0x12, 0x04, 0x55]);
        assert_eq!(&r[..3], &[0x92, 0x04, 0xFF]);
    }

    #[test]
    fn flash_read_round_trip() {
        let mut s = SimEcu::new();
        let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x80]);
        assert_eq!(&r[..3], &[0x92, 0x09, 0xA0]);
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
        assert_eq!(r[0], 0x92);
        assert_eq!(r[4], 0xA0);
        let v = u16::from_be_bytes([r[5], r[6]]);
        assert_eq!(v, SimEcu::mw_value(1));
        // battery voltage: plausible ~12.6 V at the DDE3 scale
        let r = s.handle(&[0x12, 0x05, 0x0B, 0x10, 0x0F, 0x65]);
        let v = u16::from_be_bytes([r[5], r[6]]);
        let volt = v as f64 * 0.02037243;
        assert!((11.0..14.0).contains(&volt), "battery {volt} V");
    }

    #[test]
    fn fault_memory_sequence() {
        let mut s = SimEcu::new();
        // count: 3 seeded faults
        let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        assert_eq!(&r[..4], &[0x92, 0x05, 0xA0, 3]);
        // entry 1
        let r = s.handle(&[0x12, 0x05, 0x04, 0x00]);
        assert_eq!(&r[..5], &[0x92, 0x05, 0xA0, 0x26, 0x02]);
        let uw1 = u16::from_be_bytes([r[5], r[6]]);
        assert_eq!(uw1, 0x012C);
        // shadow of the fault just read: 11 u16 values
        let r = s.handle(&[0x12, 0x05, 0x04, 0x01]);
        assert_eq!(&r[..3], &[0x92, 0x05, 0xA0]);
        assert_eq!(r.len(), 3 + 22);
        // entry 2 and 3
        let r = s.handle(&[0x12, 0x05, 0x04, 0x00]);
        assert_eq!(&r[3..5], &[0x05, 0x01]);
        s.handle(&[0x12, 0x05, 0x04, 0x01]);
        let r = s.handle(&[0x12, 0x05, 0x04, 0x00]);
        assert_eq!(&r[3..5], &[0x36, 0x04]);
        // exhausted → ERROR_SG_NUMBER
        let r = s.handle(&[0x12, 0x05, 0x04, 0x00]);
        assert_eq!(r[2], 0xB2);
        // DS2 clear
        let r = s.handle(&[0x12, 0x04, 0x05]);
        assert_eq!(&r[..3], &[0x92, 0x04, 0xA0]);
        let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        assert_eq!(r[3], 0);
    }
}
