//! Break-entered service protocol (FINDINGS §4).
//!
//! The ECU's S0 UART has a **second, faster protocol world** beyond the 9600
//! DS2 link. It is entered by a line **break** + handshake, re-rates the port
//! to ~10.4 k (`S0BG = 0x2F`), and — after a second handshake byte — may go
//! to ~38.4 k (`S0BG = 0x0C`) for byte-addressed RAM / EEPROM / fault-memory
//! access and PEC bulk transfer.
//!
//! # State machine (disasm `out/disasm.txt`, S0 ISR at 0x1E904, state var C749)
//!
//! | state | addr  | action |
//! |-------|-------|--------|
//! | 3 | 0x1EBF4→0x1EC06 | on break: `S0BG = 0x2F` (~10.4 k), re-enable RX, RX counter `C6C0 = 0`, → state 4 |
//! | 4 | 0x1EC42 | wait for a byte `C6C4 & 0xF0 == 0xA0` (the 0xA5 handshake); **abort to state 0 unless the DS2 session is unlocked** (`C0DF.0` set and `C3A9 == 0xA5`, checks at 0x1EC70–0x1EC90) → state 5 |
//! | 5 | 0x1ECB6 | second byte; latched into the handshake word `C332` (high byte) and the PEC register `FEB0 = C332 \| 0x100` → state 6 |
//! | 6 | 0x1ECFC | command loop. First idle pass (0x1ED44): if the second handshake byte is **`0xA6`** → `S0BG = 0x0C` (~38.4 k), otherwise stay at 10.4 k; arms the PEC block channel (`FEC8 = 0x020C`, `FDF2 = &C74E`, `FDF0 = &FEB2`) |
//!
//! Exit back to 9600 DS2 happens via the "exit word" path at 0x1EFAE
//! (handshake word `C332 == 0xFF1C`) and by the 0x1E8EE re-rate routine
//! (`S0BG = 0x33`). The 0xA6 / 0xFF1C words are what a hardware capture must
//! confirm on the wire (FINDINGS §4.3); the mailbox-level behaviour below is
//! grounded in the disassembly.
//!
//! # Command format (interpreter task at 0x1F6D2, jump table 0x1F724, 0x51 entries)
//!
//! Messages are raw 4-byte blocks (no DS2 length byte, no trailing XOR):
//!
//! ```text
//! request : [0x01, OPC, xx, DATA]   first byte MUST be 0x01 (gate at 0x1F6EC)
//! reply   : [0x00, TAG, RESP, HI]   TAG = 0x02 normal, 0x01 PEC-not-ready,
//!                                        0x05 busy (PEC bulk in progress)
//!                                  HI  = 0x01 for data replies, 0x00 for the
//!                                        two "unknown opcode" defaults
//! ```
//!
//! Mailbox mapping (verified): request bytes land in `C32E..C331` → the
//! interpreter copies them to `C0FA..C0FD` with opcode = 2nd byte (clamped to
//! 0x50) and data = 4th byte. The 3rd request byte is ignored by every
//! handler. The reply is written to `C0FA..C0FD` (0x1FC26) and mirrored back
//! to the mailbox.
//!
//! # Opcode map (index = jump-table slot, all verified against 0x1F724–0x1F864)
//!
//! | opc | handler | action (registers as in the disassembly) |
//! |-----|---------|-------------------------------------------|
//! | 0x02 | 0x1F868 | echo `DATA` and add it to checksum `C887` |
//! | 0x04 | 0x1F886 | return address low byte `C804`; `C887 += C804` |
//! | 0x07 | 0x1F8A4 | return address high byte `C805`; `C887 += C805` |
//! | 0x0B / 0x0E | 0x1F8C2 | read RAM at `C804` (word-aware, see below); `C804 -= 1` |
//! | 0x0D | 0x1F8CA | read RAM at `C804 + C1B8` (offset word at RAM 0x01B8); `C804 -= 1` |
//! | 0x10 | 0x1F8DC | read block-buffer byte `C80E[C804]` if `C804 < C802` else `0xEE`; `C804 -= 1`; `C887 += byte` |
//! | 0x13 | 0x1F90E | read fault memory: RAM at `C804 + 0xCE7A` (the 13-byte DTC records); `C804 -= 1` |
//! | 0x18 | 0x1F91A | read 16-bit value at `C0B8 + 2·C804 − 2` (EEPROM mirror); `C804 == 0` answers `8`; `C804 -= 1` |
//! | 0x1B | 0x1F938 | return checksum accumulator `C887` |
//! | 0x25 | 0x1F952 | `C804 <- DATA` (low byte); answer `DATA XOR OPC` |
//! | 0x26 | 0x1F976 | `C805 <- DATA` (high byte); answer `DATA XOR OPC` |
//! | 0x2A | 0x1F99A | block handshake/reset: `DATA` must be `0x11` (answer `0x2A`, else `0xE1`); resets `C802 = 0x12`, `C887 = 0`, fills the 0x50-byte buffer at `C80E` with the `1C FF` pattern, `C806 = 0` |
//! | 0x31 | 0x1FA0E | store `DATA` into `C80E[C804]` if `C804 < C802` else answer `0xEE`; `C804 -= 1`; answer `DATA XOR OPC` |
//! | 0x3E | 0x1FA46 | PEC bulk transfer: start (arms `FEC8`, waits for the `0x01AA` pattern, answers `0x0155`), then streams the buffer via S0 block |
//! | 0x4F | 0x1FB76 | `DATA` must be `0x3D`/`0x3E` else `0xE1`; sets block size `C802 = 2·[RAM 0x0E9A]` |
//!
//! Word-aware RAM read (shared routine 0x1FBE6): the address register is the
//! 24-bit value at RAM `C804..C806` (bytes: low, mid, high — `0x25`/`0x26`
//! write only the low/mid bytes, so the high byte is never set by an opcode).
//! The **partner cache** is the 24-bit value at `C806..C808`, i.e. it SHARES
//! the address-high byte `C806` with the register. At an **even** address the
//! ECU fetches the whole 16-bit word, caches the high byte in `C88A`, stores
//! the partner address `C806..C808 ← addr − 0x0FF1E` (24-bit) and answers the
//! low byte; a later read at the partner address answers the cached high
//! byte; a miss clears the partner cache. Every read does `C804..C806 -= 1`
//! (24-bit decrement — which ripples into the shared `C806` partner byte),
//! i.e. pre-decrement pointer semantics.
//!
//! NOTE: the `ADDR_LOW`/`ADDR_HIGH` handlers (0x1F886/0x1F8A4) are
//! byte-identical in the disassembly (both `MOV R0,[C804]; C0FC ← R0`); the
//! "low / high byte" semantics follow the routines summary.
//!
//! Unknown opcodes answer through two default slots: **0xED** (default A,
//! 0x1FBBA) or **0xEB** (default B, 0x1FBD0) depending on the table slot;
//! the per-slot A/B assignment is captured in `BreakSim::default_reply`.
//!
//! NOTE: `research/routines_part2.md` lists the checksum opcode as 0x1C and
//! the 16-bit read as 0x19 — the jump table at 0x1F724 places them at 0x1B
//! and 0x18 (slot = (table_addr − 0x1F724)/4). This module follows the table.
//!
//! # Precondition
//!
//! The DS2 session must already be unlocked (UPROG_EIN + SEED_KEY) — the
//! state-4 gate checks internal flags (`C0DF.0`, `C3A9 == 0xA5`), not a DS2
//! byte. If the handshake is refused, the ECU aborts to state 0 and 9600 DS2
//! continues.

use std::time::Duration;

use anyhow::{bail, Result};

use crate::protocol::{hex, Kline};

// ---------------------------------------------------------------------------
// Baud ladder (disasm.txt, FINDINGS §1)
// ---------------------------------------------------------------------------

/// The three S0BG values and their rates.
#[allow(dead_code)]
pub mod s0bg {
    /// 9600 — the DS2 application link default.
    pub const BAUD_9600: u16 = 0x0033;
    /// ~10.4 k — the intermediate handshake rate (state 3).
    pub const BAUD_10_4K: u16 = 0x002F;
    /// ~38.4 k — the fast bulk-access rate (state 6).
    pub const BAUD_38_4K: u16 = 0x000C;
}

/// Host-side baud rates matching `s0bg`.
#[allow(dead_code)]
pub const BAUD_9600: u32 = 9_600;
/// Intermediate handshake rate.
#[allow(dead_code)]
pub const BAUD_10_4K: u32 = 10_400;
/// Fast bulk-access rate.
#[allow(dead_code)]
pub const BAUD_38_4K: u32 = 38_400;

// ---------------------------------------------------------------------------
// Handshake bytes (states 4/5)
// ---------------------------------------------------------------------------

/// First handshake byte (state 4). High nibble `0xA` (gate at 0x1EC60);
/// `0xA5` matches the `C3A9 == 0xA5` unlock check at 0x1EC8C.
pub const HANDSHAKE_BYTE: u8 = 0xA5;
/// Second handshake byte (state 5) that latches the 38.4 k path: the state-6
/// idle block at 0x1ED58–0x1ED66 re-rates to `S0BG = 0x0C` only when the
/// latched byte equals `0xA6`. Any other byte leaves the link at 10.4 k.
pub const HANDSHAKE_FAST_BYTE: u8 = 0xA6;

// ---------------------------------------------------------------------------
// Command opcodes (interpreter at 0x1F6D2, jump table 0x1F724)
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub mod opcode {
    /// Echo `DATA`, add it to the checksum accumulator. (0x1F868)
    pub const ECHO: u8 = 0x02;
    /// Return address low byte `C804`. (0x1F886)
    pub const ADDR_LOW: u8 = 0x04;
    /// Return address high byte `C805`. (0x1F8A4)
    pub const ADDR_HIGH: u8 = 0x07;
    /// Read RAM at `C804` (word-aware), `C804 -= 1`. (0x1F8C2)
    pub const READ: u8 = 0x0B;
    /// Alias of [`READ`] (same handler). (0x1F8C2)
    pub const READ_ALT: u8 = 0x0E;
    /// Read RAM at `C804 + C1B8`. (0x1F8CA)
    pub const READ_OFFSET: u8 = 0x0D;
    /// Read block-buffer byte at index `C804`. (0x1F8DC)
    pub const READ_BLOCK: u8 = 0x10;
    /// Read fault memory (DTC records) at `C804 + 0xCE7A`. (0x1F90E)
    pub const READ_FAULT: u8 = 0x13;
    /// Read 16-bit value from the EEPROM mirror at `C0B8 + 2·C804 − 2`. (0x1F91A)
    pub const READ_EEPROM_WORD: u8 = 0x18;
    /// Return the checksum accumulator `C887`. (0x1F938)
    pub const CHECKSUM: u8 = 0x1B;
    /// Block handshake/reset — `DATA` must be [`HANDSHAKE_DATA`]. (0x1F99A)
    pub const HANDSHAKE: u8 = 0x2A;
    /// Required `DATA` byte for [`HANDSHAKE`].
    pub const HANDSHAKE_DATA: u8 = 0x11;
    /// Set address low byte `C804 <- DATA`. (0x1F952)
    pub const SET_ADDR_LOW: u8 = 0x25;
    /// Set address high byte `C805 <- DATA`. (0x1F976)
    pub const SET_ADDR_HIGH: u8 = 0x26;
    /// Store `DATA` into the block buffer at index `C804`. (0x1FA0E)
    pub const STORE_BLOCK: u8 = 0x31;
    /// PEC bulk transfer (start / status / stream). (0x1FA46)
    pub const PEC_BULK: u8 = 0x3E;
    /// Set block size `C802 = 2·[RAM 0x0E9A]`; `DATA` must be `0x3D`/`0x3E`. (0x1FB76)
    pub const SET_BLOCK_SIZE: u8 = 0x4F;
    /// `DATA` gate value for [`SET_BLOCK_SIZE`].
    pub const SET_BLOCK_SIZE_DATA: u8 = 0x3D;

    /// "Unknown opcode" default reply, slot A. (0x1FBBA)
    pub const DEFAULT_A: u8 = 0xED;
    /// "Unknown opcode" default reply, slot B. (0x1FBD0)
    pub const DEFAULT_B: u8 = 0xEB;
    /// Out-of-range block index answer (both ends of the buffer). (0x1F8F6)
    pub const OUT_OF_RANGE: u8 = 0xEE;
    /// PEC-not-ready / mismatch answer. (0x1F9DC)
    pub const REJECT: u8 = 0xE1;
}

/// Build a 4-byte request frame: `[0x01, opc, 0x00, data]`.
///
/// The 3rd byte (`C0FC` request slot) is ignored by every ECU handler, so it
/// is sent as 0x00.
pub fn request(opc: u8, data: u8) -> [u8; 4] {
    [0x01, opc, 0x00, data]
}

/// A parsed 4-byte reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reply {
    /// `0x02` normal, `0x01` PEC-not-ready, `0x05` busy (PEC bulk in progress).
    pub tag: u8,
    /// The `RESP` byte (or a 16-bit word read, low byte here; see `hi`).
    pub data: u8,
    /// High byte of the reply word. `0x01` on data replies, `0x00` on the
    /// default (unknown-opcode) replies.
    pub hi: u8,
}

impl Reply {
    /// True for the two "unknown opcode" defaults (0xED / 0xEB, hi = 0).
    #[allow(dead_code)]
    pub fn is_unknown(self) -> bool {
        self.hi == 0x00 && (self.data == opcode::DEFAULT_A || self.data == opcode::DEFAULT_B)
    }
    /// True when the block buffer was exhausted / out of range.
    #[allow(dead_code)]
    pub fn is_out_of_range(self) -> bool {
        self.data == opcode::OUT_OF_RANGE
    }
}

/// Parse a 4-byte reply. Returns an error if the frame does not match the
/// `[0x00, TAG, RESP, HI]` shape at all.
pub fn parse_reply(bytes: &[u8]) -> Result<Reply> {
    if bytes.len() < 4 || bytes[0] != 0x00 {
        bail!("malformed secondary reply: {}", hex(bytes));
    }
    if !matches!(bytes[1], 0x01 | 0x02 | 0x05) {
        bail!("secondary reply with unknown tag 0x{:02X}: {}", bytes[1], hex(bytes));
    }
    Ok(Reply {
        tag: bytes[1],
        data: bytes[2],
        hi: bytes[3],
    })
}

// ---------------------------------------------------------------------------
// Entry + raw/command transport handle
// ---------------------------------------------------------------------------

/// A K-line handle in break-entered service mode, at the intermediate or fast
/// rate. The DS2 framing (length byte / trailing XOR) does NOT apply here —
/// the secondary protocol is raw 4-byte blocks, so use [`BreakHandle::command`]
/// (or [`raw_write`](BreakHandle::raw_write) / [`raw_read`](BreakHandle::raw_read)).
pub struct BreakHandle<'a> {
    kline: &'a mut Kline,
}

impl<'a> BreakHandle<'a> {
    /// Write raw bytes at the current (break-mode) rate.
    pub fn raw_write(&mut self, bytes: &[u8]) -> Result<()> {
        self.kline.raw_write(bytes)
    }

    /// Read up to `n` raw bytes or `timeout`.
    pub fn raw_read(&mut self, n: usize, timeout: Duration) -> Result<Vec<u8>> {
        self.kline.raw_read(n, timeout)
    }

    /// Send one 4-byte command and return the parsed 4-byte reply.
    ///
    /// The reply is read back byte-wise with the same 4-byte timeout budget
    /// the entry handshake uses; a short or malformed frame is an error.
    pub fn command(&mut self, opc: u8, data: u8) -> Result<Reply> {
        let frame = request(opc, data);
        self.kline.raw_write(&frame)?;
        let r = self.kline.raw_read(4, Duration::from_millis(300))?;
        parse_reply(&r)
    }

    /// Block handshake/reset (opcode 0x2A with DATA 0x11). Answers 0x2A on
    /// success, 0xE1 if the data byte is wrong, and resets the block buffer
    /// (`C802 = 0x12`, `C887 = 0`, buffer ← 1C FF pattern).
    pub fn reset_block(&mut self) -> Result<Reply> {
        self.command(opcode::HANDSHAKE, opcode::HANDSHAKE_DATA)
    }

    /// Set the protocol address register (C804 low, C805 high) via 0x25/0x26.
    #[allow(dead_code)]
    pub fn set_addr(&mut self, addr: u32) -> Result<(Reply, Reply)> {
        let lo = self.command(opcode::SET_ADDR_LOW, (addr & 0xFF) as u8)?;
        let hi = self.command(opcode::SET_ADDR_HIGH, ((addr >> 8) & 0xFF) as u8)?;
        Ok((lo, hi))
    }

    /// Read back the address register via 0x04/0x07.
    pub fn get_addr(&mut self) -> Result<u16> {
        let lo = self.command(opcode::ADDR_LOW, 0x00)?.data;
        let hi = self.command(opcode::ADDR_HIGH, 0x00)?.data;
        Ok(((hi as u16) << 8) | lo as u16)
    }

    /// Read the checksum accumulator `C887` (opcode 0x1B).
    pub fn checksum(&mut self) -> Result<u8> {
        Ok(self.command(opcode::CHECKSUM, 0x00)?.data)
    }

    /// Read one RAM byte at `addr` via 0x0B (word-aware on the ECU side; the
    /// low byte of the read word comes back, the high byte is cached for the
    /// partner address — see the module docs).
    #[allow(dead_code)]
    pub fn read_ram(&mut self, addr: u32) -> Result<u8> {
        let _ = self.set_addr(addr)?;
        Ok(self.command(opcode::READ, 0x00)?.data)
    }

    /// Re-arm both ends to a new rate (the ECU re-rates too, so the tool must
    /// match). Mirrors `Kline::set_baud`.
    #[allow(dead_code)]
    pub fn set_rate(&mut self, baud: u32) -> Result<()> {
        self.kline.set_baud(baud)
    }

    /// Return to the 9600 DS2 application link (best-effort).
    pub fn exit(self) {
        let _ = self.kline.set_baud(BAUD_9600);
    }
}

/// Enter the break-entered service protocol (FINDINGS §4.1).
///
/// `fast` selects the 38.4 k bulk rate (state 6): the second handshake byte
/// is sent as `0xA6`, which the state-6 idle block (0x1ED60) latches to
/// `S0BG = 0x0C`. Otherwise the second byte is `0xA5` and the link stays at
/// 10.4 k.
///
/// ⚠️ The ECU-side latch is grounded in the disassembly, but the on-wire
/// handshake (byte order, timing, whether the ECU echoes) must still be
/// confirmed against a hardware capture (FINDINGS §4.3) before this path is
/// trusted for bulk transfer.
pub fn enter(kline: &mut Kline, fast: bool) -> Result<BreakHandle<'_>> {
    // 1. Break + intermediate rate (state 3: S0BG = 0x2F).
    kline.raw_break()?;
    kline.set_baud(BAUD_10_4K)?;

    // 2. Handshake byte 1 (state 4): 0xA5, must be echoed back.
    kline.raw_write(&[HANDSHAKE_BYTE])?;
    let echo = kline.raw_read(1, Duration::from_millis(300))?;
    if echo.first() != Some(&HANDSHAKE_BYTE) {
        bail!(
            "break handshake failed: no 0x{:02X} echo (got {})",
            HANDSHAKE_BYTE,
            hex(&echo)
        );
    }

    // 3. Handshake byte 2 (state 5): 0xA6 → 38.4 k latch, else stay at 10.4 k.
    let second = if fast {
        HANDSHAKE_FAST_BYTE
    } else {
        HANDSHAKE_BYTE
    };
    kline.raw_write(&[second])?;
    let echo2 = kline.raw_read(1, Duration::from_millis(300))?;
    if echo2.first() != Some(&second) {
        bail!(
            "break handshake (byte 2) failed: no 0x{:02X} echo (got {})",
            second,
            hex(&echo2)
        );
    }

    // 4. Fast path (state 6): re-arm the tool to 38.4 k.
    if fast {
        kline.set_baud(BAUD_38_4K)?;
    }

    Ok(BreakHandle { kline })
}

// ---------------------------------------------------------------------------
// In-process sim of the secondary protocol (offline unit tests)
// ---------------------------------------------------------------------------

/// Simulates the ECU side of the break protocol for unit tests: the entry
/// handshake plus the 0x1F6D2 interpreter with its verified opcode set,
/// address/checksum/block-buffer registers and the two default replies.
///
/// The sim mirrors the disassembly at the mailbox level; wire-level details
/// (byte order on the K-line, PEC framing) are NOT modeled and remain
/// capture-pending (FINDINGS §4.3).
#[derive(Default)]
#[allow(dead_code)]
pub struct BreakSim {
    pub handshakes: u32,
    /// 64 KB of RAM (DPP2 `Cxxxx` space). Fault records live at 0xCE7A.
    pub ram: Vec<u8>,
    /// Shared register block RAM `C804..C808`: bytes 0..3 = 24-bit address
    /// register (low, mid, high), bytes 2..5 = 24-bit partner cache — byte 2
    /// (RAM C806) is shared, exactly as in the ECU.
    c804_block: [u8; 5],
    /// Cached high byte of the last word read (RAM C88A).
    c88a: u8,
    /// Checksum accumulator (C887).
    c887: u8,
    /// Block size (C802, default 0x12) and buffer (C80E, 0x50 bytes).
    c802: u16,
    pub block: Vec<u8>,
    /// RAM 0x0E9A — the block-size source read by opcode 0x4F.
    pub block_size_src: u8,
    /// PEC bulk armed flag (C7FE/FF70.7 model).
    pec_armed: bool,
}

#[allow(dead_code)]
impl BreakSim {
    pub fn new() -> Self {
        Self {
            ram: vec![0x00; 0x10000],
            block: vec![0x00; 0x50],
            ..Self::default()
        }
    }

    /// Fill the block buffer with the 1C FF pattern, like the ECU's 0x2A
    /// reset (0x1F9F6), and zero the partner cache + address high byte
    /// (`MOV C806h, ZEROS` at 0x1FA06 — this also clears RAM C806, the
    /// address-high byte).
    pub fn reset_block_state(&mut self) {
        self.c802 = 0x12;
        self.c887 = 0;
        self.block = (0..0x50).map(|i| if i & 1 == 0 { 0x1C } else { 0xFF }).collect();
        // Zero the 3 bytes at C806..C808 (partner cache + address high byte),
        // mirroring `MOV C806h, ZEROS`.
        for i in 2..5 {
            self.c804_block[i] = 0;
        }
    }

    /// The 24-bit address register (RAM C804..C806).
    fn addr(&self) -> u32 {
        (self.c804_block[0] as u32)
            | ((self.c804_block[1] as u32) << 8)
            | ((self.c804_block[2] as u32) << 16)
    }

    /// The 24-bit partner cache (RAM C806..C808) — shares byte C806 with the
    /// address register.
    fn partner(&self) -> u32 {
        (self.c804_block[2] as u32)
            | ((self.c804_block[3] as u32) << 8)
            | ((self.c804_block[4] as u32) << 16)
    }

    /// Store the 24-bit partner cache (`MOV C806h, R1` at 0x1FBF0).
    fn set_partner(&mut self, v: u32) {
        self.c804_block[2] = (v & 0xFF) as u8;
        self.c804_block[3] = ((v >> 8) & 0xFF) as u8;
        self.c804_block[4] = ((v >> 16) & 0xFF) as u8;
    }

    /// `SUB C804h, ONES` — 24-bit decrement of the address register (ripples
    /// into the shared C806 partner byte).
    fn decrement_addr(&mut self) {
        let v = self.addr().wrapping_sub(1) & 0xFFFFFF;
        self.c804_block[0] = (v & 0xFF) as u8;
        self.c804_block[1] = ((v >> 8) & 0xFF) as u8;
        self.c804_block[2] = ((v >> 16) & 0xFF) as u8;
    }

    /// Feed a handshake byte (state 4 gate): high nibble must be `0xA`.
    /// Returns the echoed byte or `None` when the gate fails.
    pub fn handshake(&mut self, b: u8) -> Option<u8> {
        if b & 0xF0 != 0xA0 {
            return None;
        }
        self.handshakes += 1;
        Some(b)
    }

    /// Per-slot unknown-opcode default (A = 0xED at 0x1FBBA, B = 0xEB at
    /// 0x1FBD0), transcribed from the jump table 0x1F724–0x1F864.
    pub fn default_reply(opc: u8) -> u8 {
        const SLOTS: &[u8] = &[
            // 0x00-0x0F
            opcode::DEFAULT_A, opcode::DEFAULT_A, 0x00 /*0x02*/, opcode::DEFAULT_A,
            0x00 /*0x04*/, opcode::DEFAULT_A, opcode::DEFAULT_A, 0x00 /*0x07*/,
            opcode::DEFAULT_B, opcode::DEFAULT_A, opcode::DEFAULT_A, 0x00 /*0x0B*/,
            opcode::DEFAULT_A, 0x00 /*0x0D*/, 0x00 /*0x0E*/, opcode::DEFAULT_A,
            // 0x10-0x1F
            0x00 /*0x10*/, opcode::DEFAULT_A, opcode::DEFAULT_A, 0x00 /*0x13*/,
            opcode::DEFAULT_A, opcode::DEFAULT_B, opcode::DEFAULT_B, opcode::DEFAULT_A,
            0x00 /*0x18*/, opcode::DEFAULT_B, opcode::DEFAULT_A, 0x00 /*0x1B*/,
            opcode::DEFAULT_A, opcode::DEFAULT_A, opcode::DEFAULT_B, opcode::DEFAULT_B,
            // 0x20-0x2F
            opcode::DEFAULT_A, opcode::DEFAULT_A, opcode::DEFAULT_B, opcode::DEFAULT_A,
            opcode::DEFAULT_B, 0x00 /*0x25*/, 0x00 /*0x26*/, opcode::DEFAULT_A,
            opcode::DEFAULT_A, opcode::DEFAULT_B, 0x00 /*0x2A*/, opcode::DEFAULT_A,
            opcode::DEFAULT_B, opcode::DEFAULT_A, opcode::DEFAULT_A, opcode::DEFAULT_B,
            // 0x30-0x3F
            opcode::DEFAULT_A, 0x00 /*0x31*/, opcode::DEFAULT_B, opcode::DEFAULT_A,
            opcode::DEFAULT_B, opcode::DEFAULT_A, opcode::DEFAULT_A, opcode::DEFAULT_B,
            opcode::DEFAULT_B, opcode::DEFAULT_A, opcode::DEFAULT_A, opcode::DEFAULT_B,
            opcode::DEFAULT_A, opcode::DEFAULT_B, 0x00 /*0x3E*/, opcode::DEFAULT_A,
            // 0x40-0x50
            opcode::DEFAULT_B, opcode::DEFAULT_A, opcode::DEFAULT_A, opcode::DEFAULT_B,
            opcode::DEFAULT_A, opcode::DEFAULT_B, opcode::DEFAULT_B, opcode::DEFAULT_A,
            opcode::DEFAULT_A, opcode::DEFAULT_B, opcode::DEFAULT_A, opcode::DEFAULT_A,
            opcode::DEFAULT_A, opcode::DEFAULT_A, opcode::DEFAULT_A, 0x00 /*0x4F*/,
            opcode::DEFAULT_A, // 0x50 (clamp)
        ];
        let opc = opc.min(0x50);
        SLOTS[opc as usize].max(opcode::DEFAULT_B)
    }

    /// Word-aware RAM read, mirroring the shared routine at 0x1FBE6/0x1FC06:
    /// even → word fetch + cache high byte + partner address `C806..C808 =
    /// addr − 0x0FF1E` (24-bit) and answer the low byte; odd at the partner
    /// address → answer the cached high byte; other odd → plain byte read and
    /// clear the partner cache. `C804 -= 1` (24-bit) and the checksum always
    /// advance.
    fn ram_read(&mut self, addr: u32) -> u8 {
        let reply: u16;
        if addr & 1 == 0 {
            let lo = self.ram.get(addr as usize & 0xFFFF).copied().unwrap_or(0);
            let hi = self.ram.get(((addr as usize & 0xFFFF) + 1) & 0xFFFF).copied().unwrap_or(0);
            reply = (hi as u16) << 8 | lo as u16;
            self.set_partner(addr.wrapping_sub(0x0FF1E) & 0xFFFFFF);
            self.c88a = hi;
        } else if addr == self.partner() {
            reply = self.c88a as u16;
            self.set_partner(0);
        } else {
            reply = self.ram.get(addr as usize & 0xFFFF).copied().unwrap_or(0) as u16;
            self.set_partner(0);
        }
        self.decrement_addr();
        self.c887 = self.c887.wrapping_add(reply as u8);
        reply as u8 // low byte travels in the reply; high rides on C88A
    }

    /// Service one 4-byte request, mailbox model: the first byte must be
    /// 0x01 (gate at 0x1F6EC) or there is **no reply at all**.
    pub fn service(&mut self, req: &[u8]) -> Option<[u8; 4]> {
        if req.len() < 4 || req[0] != 0x01 {
            return None;
        }
        let opc = req[1].min(0x50);
        let data = req[3];

        // PEC-busy slot (FF70.7 path at 0x1F6F2): busy tag 0x05.
        if self.pec_armed {
            return Some([0x00, 0x05, 0x00, 0x00]);
        }

        let reply: u16 = match opc {
            opcode::ECHO => {
                self.c887 = self.c887.wrapping_add(data);
                data as u16
            }
            opcode::ADDR_LOW | opcode::ADDR_HIGH => {
                // Both handlers load the 24-bit address word and answer its
                // low byte (see the module NOTE); the routines summary reads
                // them as low/high, so the sim keeps that intent: 0x04 → low
                // byte, 0x07 → mid byte.
                let a = self.addr();
                let b: u16 = if opc == opcode::ADDR_LOW {
                    a as u16
                } else {
                    (a >> 8) as u16
                };
                self.c887 = self.c887.wrapping_add(b as u8);
                b
            }
            opcode::READ | opcode::READ_ALT => self.ram_read(self.addr()) as u16,
            opcode::READ_OFFSET => {
                let off = u16::from_le_bytes([
                    self.ram.get(0x01B8).copied().unwrap_or(0),
                    self.ram.get(0x01B9).copied().unwrap_or(0),
                ]) as u32;
                self.ram_read(self.addr().wrapping_add(off) & 0xFFFFFF) as u16
            }
            opcode::READ_BLOCK => {
                let idx = self.addr() as u16; // C804 used as the block index
                if idx < self.c802 {
                    let b = self.block[idx as usize];
                    self.decrement_addr();
                    self.c887 = self.c887.wrapping_add(b);
                    b as u16
                } else {
                    opcode::OUT_OF_RANGE as u16
                }
            }
            opcode::READ_FAULT => self.ram_read(self.addr().wrapping_add(0xCE7A) & 0xFFFFFF) as u16,
            opcode::READ_EEPROM_WORD => {
                let a = self.addr();
                if a == 0 {
                    self.decrement_addr();
                    8
                } else {
                    // C0B8 = 24-bit pointer at RAM 0x00B8 (word move there
                    // reads C0B8/C0B9).
                    let base = u16::from_le_bytes([
                        self.ram.get(0x00B8).copied().unwrap_or(0),
                        self.ram.get(0x00B9).copied().unwrap_or(0),
                    ]) as u32;
                    let p = (base + 2 * (a as u16) as u32 - 2) & 0xFFFF;
                    let lo = self.ram.get(p as usize).copied().unwrap_or(0) as u16;
                    let hi = self.ram.get((p + 1) as usize).copied().unwrap_or(0) as u16;
                    self.decrement_addr();
                    self.c887 = self.c887.wrapping_add(lo as u8);
                    (hi << 8) | lo
                }
            }
            opcode::CHECKSUM => self.c887 as u16,
            opcode::HANDSHAKE => {
                if data != opcode::HANDSHAKE_DATA {
                    // 0x1F9A4: answer 0xE1 WITHOUT touching the block state.
                    opcode::REJECT as u16
                } else {
                    self.reset_block_state();
                    opcode::HANDSHAKE as u16
                }
            }
            opcode::SET_ADDR_LOW => {
                self.c804_block[0] = data;
                (data as u16) ^ opc as u16
            }
            opcode::SET_ADDR_HIGH => {
                self.c804_block[1] = data;
                (data as u16) ^ opc as u16
            }
            opcode::STORE_BLOCK => {
                let idx = self.addr() as u16;
                if idx < self.c802 {
                    self.block[idx as usize] = data;
                    self.decrement_addr();
                    (data as u16) ^ opc as u16
                } else {
                    opcode::OUT_OF_RANGE as u16
                }
            }
            opcode::PEC_BULK => {
                // First pass (0x1FA46): arm the PEC channel; the mailbox
                // reply fields are not rewritten by the handler, so the sim
                // answers a neutral frame. The 0x01AA → 0x0155 handshake and
                // the stream itself are wire-level (capture-pending).
                self.pec_armed = true;
                0x00
            }
            opcode::SET_BLOCK_SIZE => {
                if data == 0x3D || data == 0x3E {
                    self.c802 = (self.block_size_src as u16) * 2;
                    (data as u16) ^ (self.block_size_src as u16)
                } else {
                    opcode::REJECT as u16
                }
            }
            _ => Self::default_reply(opc) as u16,
        };

        // Reply mailbox: C0FA = 0, C0FB = 2, C0FC = data | 0x0100 — EXCEPT
        // the two default replies, which carry hi = 0 (0x1FBBA/0x1FBD0 do not
        // set RH0).
        let is_default = reply & 0xFF == opcode::DEFAULT_A as u16 || reply & 0xFF == opcode::DEFAULT_B as u16;
        let hi: u8 = if is_default { 0x00 } else { 0x01 };
        Some([0x00, 0x02, (reply & 0xFF) as u8, hi])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baud_ladder_matches_disasm() {
        // The three S0BG values seen in the disassembly (FINDINGS §1).
        assert_eq!(s0bg::BAUD_9600, 0x0033);
        assert_eq!(s0bg::BAUD_10_4K, 0x002F);
        assert_eq!(s0bg::BAUD_38_4K, 0x000C);
        // The high-baud value must be the lowest register value (fastest).
        const {
            assert!(s0bg::BAUD_38_4K < s0bg::BAUD_10_4K && s0bg::BAUD_10_4K < s0bg::BAUD_9600);
        }
    }

    #[test]
    fn handshake_gate_requires_high_nibble_a() {
        let mut s = BreakSim::new();
        assert_eq!(s.handshake(HANDSHAKE_BYTE), Some(HANDSHAKE_BYTE));
        assert_eq!(s.handshakes, 1);
        // The fast byte passes the same gate.
        assert_eq!(s.handshake(HANDSHAKE_FAST_BYTE), Some(HANDSHAKE_FAST_BYTE));
        // A byte without high nibble 0xA fails the state-4 gate.
        assert_eq!(s.handshake(0x05), None);
        assert_eq!(s.handshake(0x50), None);
    }

    #[test]
    fn request_frame_shape() {
        let f = request(opcode::HANDSHAKE, opcode::HANDSHAKE_DATA);
        assert_eq!(f, [0x01, 0x2A, 0x00, 0x11]);
    }

    #[test]
    fn service_rejects_missing_first_byte() {
        let mut s = BreakSim::new();
        // First byte must be 0x01 — otherwise no reply at all (0x1F6EC).
        assert_eq!(s.service(&[0x00, opcode::ECHO, 0, 0x01]), None);
        let r = s.service(&[0x01, opcode::ECHO, 0, 0x42]).unwrap();
        assert_eq!(r, [0x00, 0x02, 0x42, 0x01]);
    }

    #[test]
    fn checksum_accumulates_and_reads_back() {
        let mut s = BreakSim::new();
        s.service(&request(opcode::ECHO, 0x10)).unwrap();
        s.service(&request(opcode::ECHO, 0x05)).unwrap();
        let r = s.service(&request(opcode::CHECKSUM, 0x00)).unwrap();
        assert_eq!(r[2], 0x15);
        assert_eq!(r[3], 0x01);
    }

    #[test]
    fn addr_set_get_roundtrip() {
        let mut s = BreakSim::new();
        let lo = s.service(&request(opcode::SET_ADDR_LOW, 0x7A)).unwrap();
        assert_eq!(lo[2], 0x7A ^ opcode::SET_ADDR_LOW);
        let hi = s.service(&request(opcode::SET_ADDR_HIGH, 0xCE)).unwrap();
        assert_eq!(hi[2], 0xCE ^ opcode::SET_ADDR_HIGH);
        let a = s.service(&request(opcode::ADDR_LOW, 0)).unwrap();
        let b = s.service(&request(opcode::ADDR_HIGH, 0)).unwrap();
        assert_eq!(((b[2] as u16) << 8) | a[2] as u16, 0xCE7A);
    }

    #[test]
    fn ram_read_even_word_caches_high_and_partner() {
        let mut s = BreakSim::new();
        s.ram[2] = 0x12;
        s.ram[3] = 0x34;
        // Address 0x000002 (even) → word read: answer low byte 0x12, cache
        // high byte 0x34, and store the partner address (24-bit wrap of
        // 0x2 − 0x0FF1E) = 0xFF00E4 into RAM C806..C808.
        s.service(&request(opcode::SET_ADDR_LOW, 0x02)).unwrap();
        let first = s.service(&request(opcode::READ, 0)).unwrap();
        assert_eq!(first[2], 0x12);
        assert_eq!(first[3], 0x01);
        assert_eq!(s.c88a, 0x34);
        // The partner mid/high bytes (RAM C807/C808) are untouched by the
        // post-read address decrement and carry 0x00 / 0xFF of 0xFF00E4.
        assert_eq!(s.c804_block[3], 0x00);
        assert_eq!(s.c804_block[4], 0xFF);
    }

    #[test]
    fn ram_read_partner_hit_returns_cached_high_byte() {
        let mut s = BreakSim::new();
        // Stage the exact register state the even-read path would leave so
        // that the address register equals the partner cache: block =
        // [01 00 01 00 01] → addr = 0x010001 = partner (odd).
        s.set_partner(0x010001);
        s.c88a = 0x55;
        s.c804_block[0] = 0x01;
        s.c804_block[1] = 0x00;
        // c804_block[2] is already 0x01 from set_partner; leave it.
        assert_eq!(s.addr(), s.partner());
        let r = s.service(&request(opcode::READ, 0)).unwrap();
        assert_eq!(r[2], 0x55);
        assert_eq!(s.partner(), 0);
    }

    #[test]
    fn ram_read_odd_miss_is_plain_byte_read() {
        let mut s = BreakSim::new();
        s.ram[0x101] = 0x69;
        s.set_partner(0xDEAD);
        s.c804_block[0] = 0x01;
        s.c804_block[1] = 0x01; // address 0x000101, odd, ≠ partner
        let r = s.service(&request(opcode::READ, 0)).unwrap();
        assert_eq!(r[2], 0x69);
        assert_eq!(s.partner(), 0);
    }

    #[test]
    fn block_store_read_and_reset() {
        let mut s = BreakSim::new();
        // 0x2A with the wrong data byte → 0xE1 and the block state is NOT
        // touched (0x1F9A4 skips the reset).
        s.c804_block[0] = 0x05;
        let rej = s.service(&request(opcode::HANDSHAKE, 0x00)).unwrap();
        assert_eq!(rej[2], opcode::REJECT);
        // A correct 0x2A/0x11 resets: C802 = 0x12, C887 = 0, buffer = the
        // 1C FF pattern, address high byte + partner zeroed.
        let ack = s.service(&request(opcode::HANDSHAKE, opcode::HANDSHAKE_DATA)).unwrap();
        assert_eq!(ack[2], opcode::HANDSHAKE);
        assert_eq!(s.block[0], 0x1C);
        assert_eq!(s.block[1], 0xFF);
        assert_eq!(s.c887, 0);
        assert_eq!(s.c802, 0x12);
        // Store burst from the top of the block: C804 = 0x11, index
        // decrements on each store (stack-pointer semantics).
        s.service(&request(opcode::SET_ADDR_LOW, 0x11)).unwrap();
        let s1 = s.service(&request(opcode::STORE_BLOCK, 0xA1)).unwrap();
        assert_eq!(s1[2], 0xA1 ^ opcode::STORE_BLOCK);
        let s2 = s.service(&request(opcode::STORE_BLOCK, 0xA2)).unwrap();
        assert_eq!(s2[2], 0xA2 ^ opcode::STORE_BLOCK);
        assert_eq!(s.block[0x11], 0xA1);
        assert_eq!(s.block[0x10], 0xA2);
        // Read-back: set the index to 0x10 and read 0xA2 (index then 0x0F).
        s.service(&request(opcode::SET_ADDR_LOW, 0x10)).unwrap();
        let r = s.service(&request(opcode::READ_BLOCK, 0)).unwrap();
        assert_eq!(r[2], 0xA2);
        assert_eq!(r[3], 0x01);
        // Exhaust the stack to the end: from index 0x0F, 16 reads are valid
        // (0x0F..0x00), the next one hits index 0xFFFF → 0xEE.
        for _ in 0..16 {
            let x = s.service(&request(opcode::READ_BLOCK, 0)).unwrap();
            assert!(!parse_reply(&x).unwrap().is_out_of_range());
        }
        let oor = s.service(&request(opcode::READ_BLOCK, 0)).unwrap();
        assert!(parse_reply(&oor).unwrap().is_out_of_range());
    }

    #[test]
    fn unknown_opcode_defaults() {
        let mut s = BreakSim::new();
        // Slot A (e.g. 0x01) answers 0xED with hi = 0.
        let a = s.service(&request(0x01, 0)).unwrap();
        assert_eq!(a, [0x00, 0x02, opcode::DEFAULT_A, 0x00]);
        assert!(parse_reply(&a).unwrap().is_unknown());
        // Slot B (e.g. 0x08) answers 0xEB with hi = 0.
        let b = s.service(&request(0x08, 0)).unwrap();
        assert_eq!(b, [0x00, 0x02, opcode::DEFAULT_B, 0x00]);
        assert!(parse_reply(&b).unwrap().is_unknown());
    }

    #[test]
    fn set_block_size_gate() {
        let mut s = BreakSim::new();
        s.block_size_src = 0x20;
        let rej = s.service(&request(opcode::SET_BLOCK_SIZE, 0x00)).unwrap();
        assert_eq!(rej[2], opcode::REJECT);
        let ok = s.service(&request(opcode::SET_BLOCK_SIZE, 0x3D)).unwrap();
        assert_eq!(s.c802, 0x40);
        assert_eq!(ok[2], 0x3D ^ 0x20);
    }
}
