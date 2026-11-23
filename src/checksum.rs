//! DDE3 flash checksum — Bosch **TDI V3.1**, ported line-for-line from the
//! verified `edc15m-checksum-tool/edc15m_checksum.py`.
//!
//! Naming note: **EDC15M and DDE3 are the same ECU** — Bosch EDC15M is the
//! family name, DDE 3.0 the hardware revision this toolkit talks to.
//!
//! The flash is split into 32 kB blocks.  A block that carries a checksum
//! repeats a 2-byte id at block+0xB7A and the same id 6 bytes in front of
//! the checksum slot (i.e. 2 bytes in front of the checksum value).  The
//! checksum is a 32-bit little-endian value covering [block + 0xB80, slot):
//!
//!   256 kB full read : calibration slot 0x3FBFC (covers [0x38B80, 0x3FC00)),
//!                      code slot 0x13FFC (covers [0x10000, 0x14000))
//!    32 kB partial    : calibration slot 0x7BFC  (covers [0x0B80, 0x7C00))
//!
//! Both use the same seeded 16-bit rolling algorithm with different seeds.
//! A full read's code slot exists only while its 4-byte field is not blank
//! (0x00000000 / 0xFFFFFFFF): a 32 kB read padded out to 256 kB has no code
//! area at all, and blank flash is not a checksum that went stale.
//!
//! A full read carries a **third, different** code-area check: the cold-start
//! program chain. The boot self-test (0x11708-0x11736) walks DPP2 pages 4..0xD
//! (0x10000-0x37FFF) and then 0x38000-0x38B7F through the 16-bit window,
//! carrying the TDI step across pages, and resets (log 0x1F, code 9) unless the
//! state ends at `PROG_TARGET`. The two balancing words at 0x38B7C make it end
//! there; they sit right after the calibration block id 0x23A6 at 0x38B7A.
//! Any edit in 0x10000-0x38B7F changes the chain — including the TDI code slot
//! at 0x13FFC, which is inside that range — so the chain is always solved
//! **after** the TDI slots (see `correct`). This is the trap that makes a
//! hand-patched image boot into a reset loop with both TDI checksums valid.
//! Ported from the verified `progsum.py`; see `docs/FINDINGS_flash_and_fast_ds2.md`.

pub const BLOCK: usize = 0x8000; // block size, also the step of the block scan
pub const ID_OFF: usize = 0xB7A; // offset of the 2-byte block id inside a block
pub const SLOT_LO: usize = 0x7000; // earliest checksum slot inside a block
pub const SLOT_STEP: usize = 0x80;
pub const CS_START: usize = 0xB80; // a checksum covers from block + CS_START
pub const FULL_SIZE: usize = 0x40000; // a full 256 kB read
const M16: u16 = 0xFFFF;

/// Block geometry constants (shared with the GUI/bench).
pub const IMAGE_SIZE: usize = FULL_SIZE; // a full 256 KB read
pub const CAL_START: usize = 0x38000; // calibration: 0x38000-0x3FFFF
pub const CAL_LEN: usize = 0x8000; // 32 KB

// Cold-start program chain (boot self-test over the program area).
/// File offset of the 2-word (4-byte) program-chain balancing field.
pub const PROG_CHAIN_POS: usize = 0x38B7C;
/// File offset of the calibration block id that introduces the chain word.
pub const PROG_CHAIN_ID_POS: usize = 0x38B7A;
/// Calibration block id carried by every EDC15M/DDE3 release (31 BMW variants).
pub const PROG_CHAIN_ID: [u8; 2] = [0x23, 0xA6];
/// Required final (h, g) state of the boot self-test.
pub const PROG_TARGET: (u16, u16) = (0x8631, 0xEFCD);
const PROG_PAGE: usize = 0x4000; // one DPP2 page
const PROG_FIRST_PAGE: usize = 4; // pages 4..0xD are hashed
const PROG_LAST_PAGE: usize = 0xE; // exclusive
const PROG_PAGE_Q: u16 = 0x8000; // window base for the page walk
const PROG_CAL_Q: u16 = 0x0000; // window base for the 0x38000 tail

/// Calibration block seeds: (h at entry, g at entry, subtracted from h, xored into g)
const CAL_SEEDS: (u16, u16, u16, u16) = (0x8631, 0xEFCD, 0x8631, 0xDF9B);
/// Code block seeds
const CODE_SEEDS: (u16, u16, u16, u16) = (0x0000, 0x0000, 0x8631, 0xDF9B);

/// File offset of the TDI code checksum slot in a full read.
pub const CODE_CHECKSUM_POS: usize = 0x13FFC;
/// File offset of the TDI calibration checksum slot in a full read.
pub const CAL_CHECKSUM_POS: usize = 0x3FBFC;

/// The fixed-geometry code block in a full read.
const CODE_BLOCK: (usize, usize, usize, (u16, u16, u16, u16)) =
    (0x10000, 0x14000, CODE_CHECKSUM_POS, CODE_SEEDS);

/// A blank word is an unwritten or padded location, not a stale checksum
const ERASED_0: u32 = 0xFFFF_FFFF;
const ERASED_1: u32 = 0x0000_0000;

/// Regression vectors: LCG buffer, see `edc15m_checksum.py --selftest`
#[allow(dead_code)]
const VECTOR_SEED: u32 = 0x12345678;
#[allow(dead_code)]
const VECTOR_LEN: usize = 0x8000;
#[allow(dead_code)]
const VECTOR_CHECKSUM: u32 = 0x007B_C5EB; // calibration block over [0x0B80, 0x7C00)
#[allow(dead_code)]
const CODE_VECTOR_LEN: usize = 0x14000;
#[allow(dead_code)]
const CODE_VECTOR_CHECKSUM: u32 = 0x2477_887F; // code block over [0x10000, 0x14000)

// ---------------------------------------------------------------------------
// Public interface
// ---------------------------------------------------------------------------

/// A discovered checksum slot: `(pos, start, end, seeds)`, offsets into the
/// buffer that was verified.
type FoundChecksum = (usize, usize, usize, (u16, u16, u16, u16));

/// One discovered checksum, checked. `pos` is the offset of the 4-byte value
/// in the buffer that was verified.
#[derive(Clone, Copy, Debug)]
pub struct Checksum {
    /// "calibration", "code" or "program chain"
    pub name: &'static str,
    /// File offset of the 4-byte checksum value
    pub pos: usize,
    /// Value currently stored at `pos`
    pub stored: u32,
    /// Value recomputed from the image
    pub computed: u32,
    /// stored == computed
    pub ok: bool,
}

fn name_for(pos: usize) -> &'static str {
    if pos == CODE_BLOCK.2 {
        "code"
    } else {
        "calibration"
    }
}

fn rol(v: u16, n: u16) -> u16 {
    v.rotate_left(u32::from(n))
}

fn ror(v: u16, n: u16) -> u16 {
    v.rotate_right(u32::from(n))
}

/// 32-bit EDC15M TDI V3.1 checksum over buf[start:end].
fn checksum(buf: &[u8], start: usize, end: usize, seeds: (u16, u16, u16, u16)) -> u32 {
    let (h_in, g_in, h_out, g_out) = seeds;
    let mut h = h_in;
    let mut g = g_in;
    let mut p = start;
    let mut q = (start as u16) & M16;
    if (q >> 15) == 1 {
        q = (q.wrapping_sub(0x8000)) & M16;
    } else if q == 0 {
        q = q.wrapping_add(0x8000) & M16;
    }
    let last = end - 4;
    loop {
        h = (h ^ u16::from_le_bytes([buf[p], buf[p + 1]])) & M16;
        p = p.wrapping_add(2);
        q = q.wrapping_add(2) & M16;
        let mut flag = 0;
        if g & 0xF != 0 {
            h = rol(h, g & 0xF);
            flag = h & 1;
        }
        g = (g
            .wrapping_sub(u16::from_le_bytes([buf[p], buf[p + 1]]))
            .wrapping_sub(flag))
            & M16;
        p = p.wrapping_add(2);
        q = q.wrapping_add(2) & M16;
        g = (g ^ h) & M16;
        if p == last {
            break;
        }
        h = (h.wrapping_sub(u16::from_le_bytes([buf[p], buf[p + 1]]))) & M16;
        p = p.wrapping_add(2);
        q = q.wrapping_add(2) & M16;
        h = (q.wrapping_add(h).wrapping_sub(1)) & M16;
        g ^= u16::from_le_bytes([buf[p], buf[p + 1]]);
        p = p.wrapping_add(2);
        q = q.wrapping_add(2) & M16;
        if h & 0xF != 0 {
            g = ror(g, h & 0xF);
        }
        if p == last {
            break;
        }
    }
    h = h.wrapping_sub(h_out) & M16;
    ((q.wrapping_add(h).wrapping_add(1)) & M16) as u32 | (((g ^ g_out) & M16) as u32) << 16
}

const MARKER: [u8; 4] = [0xC3, 0x3C, 0x55, 0xAA];

fn find_marker(buf: &[u8]) -> Option<usize> {
    let n = buf.len();
    let mut m = 0;
    while m + 0x100 < n {
        if buf[m..m + 4] == MARKER && m + 0x3FFC < n {
            return Some(m);
        }
        m += 0x100;
    }
    None
}

/// The checksum slot of one block, if it has one.
fn slot(
    buf: &[u8],
    base: usize,
    key: [u8; 2],
) -> Option<FoundChecksum> {
    if key[0] == key[1] {
        return None; // a repeated byte is filler, not a block id
    }
    let mut slot = base + SLOT_LO;
    while slot < base + BLOCK {
        if buf[slot - 6..slot - 4] == key {
            return Some((slot - 4, base + CS_START, slot, CAL_SEEDS));
        }
        slot += SLOT_STEP;
    }
    None
}

/// A checksum slot has a stored value.
fn has_checksum(buf: &[u8], pos: usize) -> bool {
    if pos + 4 > buf.len() {
        return false;
    }
    let stored = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
    stored != ERASED_0 && stored != ERASED_1
}

/// Find all checksum blocks in a buffer.
///
/// The calibration block is located by its 2-byte id: normally keyed off a
/// tagged block (`\xc3\x3c\x55\xaa`), and for a 32 kB partial read — which
/// has no tag — off the id the last block carries itself.  A full read
/// additionally gets the fixed-geometry code block, once the calibration
/// block has been recognised, and only while its field is not blank.
pub fn find_checksums(buf: &[u8]) -> Vec<FoundChecksum> {
    let n = buf.len();
    let block_count = n / BLOCK;
    let blocks: Vec<usize> = (0..block_count).map(|i| i * BLOCK).collect();
    if blocks.is_empty() {
        return Vec::new();
    }

    let mut found = Vec::new();
    let marker = find_marker(buf);
    if let Some(marker) = marker {
        // the id pair sits at the end of the marker block, contiguous at
        // marker+0x3FFA (Python: buf[m+0x3FFA : m+0x3FFC])
        let key = [buf[marker + 0x3FFA], buf[marker + 0x3FFB]];
        for &base in &blocks {
            if buf[base + ID_OFF..base + ID_OFF + 2] == key {
                if let Some(hit) = slot(buf, base, key) {
                    found.push(hit);
                }
            }
        }
    } else {
        let base = blocks[blocks.len() - 1];
        let key = [buf[base + ID_OFF], buf[base + ID_OFF + 1]];
        if let Some(hit) = slot(buf, base, key) {
            found.push(hit);
        }
    }

    if !found.is_empty() && n == FULL_SIZE {
        let (start, end, pos, seeds) = CODE_BLOCK;
        if has_checksum(buf, pos) {
            found.push((pos, start, end, seeds));
        }
    }

    found
}

/// Verify all checksums in a buffer.
///
/// A full image whose TDI code block is present also gets the cold-start
/// program-chain row. Its `computed` value is the balancing word that would
/// make the chain valid for this exact buffer, so `ok` is still simply
/// `stored == computed` — and it tells the truth about what the ECU's boot
/// self-test would do with the image as it stands.
pub fn verify(buf: &[u8]) -> Vec<Checksum> {
    let found = find_checksums(buf);
    let mut out: Vec<Checksum> = found
        .iter()
        .map(|&(pos, start, end, seeds)| {
            let stored = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
            let computed = checksum(buf, start, end, seeds);
            Checksum {
                name: name_for(pos),
                pos,
                stored,
                computed,
                ok: stored == computed,
            }
        })
        .collect();
    if has_program_area(buf, &found) {
        if let Some(chain) = program_chain(buf) {
            out.push(chain);
        }
    }
    out
}

/// Correct all discovered checksums in a buffer, returning a new buffer with
/// the fixes applied. A buffer without a recognised block is returned as-is.
///
/// Order matters: the TDI slots are written first, then the program chain is
/// solved over the corrected buffer, because the chain hashes the code slot
/// at 0x13FFC along with the rest of 0x10000-0x38B7F.
pub fn correct(buf: &[u8]) -> Vec<u8> {
    let found = find_checksums(buf);
    let mut out = buf.to_vec();
    for &(pos, start, end, seeds) in &found {
        out[pos..pos + 4].copy_from_slice(&checksum(buf, start, end, seeds).to_le_bytes());
    }
    if has_program_area(buf, &found) {
        if let Some(chain) = program_chain(&out) {
            out[chain.pos..chain.pos + 4].copy_from_slice(&chain.computed.to_le_bytes());
        }
    }
    out
}

/// Correct the single checksum at `pos` (only if `pos` is a discovered slot).
/// Returns `None` when no block lives at `pos`.
pub fn correct_at(buf: &[u8], pos: usize) -> Option<Vec<u8>> {
    let cs = verify(buf).into_iter().find(|c| c.pos == pos)?;
    let mut out = buf.to_vec();
    out[pos..pos + 4].copy_from_slice(&cs.computed.to_le_bytes());
    Some(out)
}

// ---------------------------------------------------------------------------
// Cold-start program chain (ported from the verified `progsum.py`)
// ---------------------------------------------------------------------------

/// Segments the boot self-test walks: DPP2 pages 4..0xD, then 0x38000..`end_cal`.
/// `q` is the 16-bit window base each segment starts from.
fn prog_segments(end_cal: usize) -> Vec<(usize, usize, u16)> {
    let mut segs: Vec<(usize, usize, u16)> = (PROG_FIRST_PAGE..PROG_LAST_PAGE)
        .map(|p| (p * PROG_PAGE, p * PROG_PAGE + PROG_PAGE, PROG_PAGE_Q))
        .collect();
    segs.push((CAL_START, end_cal, PROG_CAL_Q));
    segs
}

/// One pass of the boot self-test hash over `segments`. Returns `(h, g, q)`.
fn prog_run(buf: &[u8], segments: &[(usize, usize, u16)]) -> (u16, u16, u16) {
    let (mut h, mut g) = (0u16, 0u16);
    let mut q = 0u16;
    let mut phase = 0u8;
    for &(start, end, q_start) in segments {
        q = q_start;
        let mut p = start;
        while p + 1 < end {
            let w = u16::from_le_bytes([buf[p], buf[p + 1]]);
            p += 2;
            q = q.wrapping_add(2);
            match phase {
                0 => h ^= w,
                1 => {
                    let mut flag = 0;
                    if g & 0xF != 0 {
                        h = rol(h, g & 0xF);
                        flag = h & 1;
                    }
                    g = g.wrapping_sub(w).wrapping_sub(flag) ^ h;
                }
                2 => {
                    h = h.wrapping_sub(w);
                    h = q.wrapping_add(h).wrapping_sub(1);
                }
                _ => {
                    g ^= w;
                    if h & 0xF != 0 {
                        g = ror(g, h & 0xF);
                    }
                }
            }
            phase = (phase + 1) & 3;
        }
    }
    (h, g, q)
}

/// The four bytes that make the chain end at `PROG_TARGET`.
fn prog_solve(buf: &[u8]) -> [u8; 4] {
    let segs = prog_segments(PROG_CHAIN_POS);
    let (h0, g0, q0) = prog_run(buf, &segs);
    let words: usize = segs.iter().map(|(s, e, _)| (e - s) / 2).sum();
    debug_assert_eq!(
        words % 4,
        2,
        "the balancing words must land on hash phases 2 and 3"
    );
    let q1 = q0.wrapping_add(2);
    let (th, tg) = PROG_TARGET;
    // phase 2: h = q + (h - w) - 1  ->  h must become th
    let w1 = h0.wrapping_add(q1).wrapping_sub(1).wrapping_sub(th);
    // phase 3: g = ror(g ^ w, h & 0xF)  ->  g must become tg
    let w2 = g0 ^ rol(tg, th & 0xF);
    let mut out = [0u8; 4];
    out[..2].copy_from_slice(&w1.to_le_bytes());
    out[2..].copy_from_slice(&w2.to_le_bytes());
    out
}

/// Verify the cold-start program chain, if this buffer carries one.
///
/// The chain is only present when the calibration block id 0x23A6 sits at
/// 0x38B7A; `verify` additionally requires a discovered TDI code block so a
/// 32 kB calibration read padded with 0xFF is never mistaken for a full image.
pub fn program_chain(buf: &[u8]) -> Option<Checksum> {
    if buf.len() < PROG_CHAIN_POS + 4 {
        return None;
    }
    if buf[PROG_CHAIN_ID_POS..PROG_CHAIN_ID_POS + 2] != PROG_CHAIN_ID {
        return None;
    }
    let stored = u32::from_le_bytes([
        buf[PROG_CHAIN_POS],
        buf[PROG_CHAIN_POS + 1],
        buf[PROG_CHAIN_POS + 2],
        buf[PROG_CHAIN_POS + 3],
    ]);
    let computed = u32::from_le_bytes(prog_solve(buf));
    Some(Checksum {
        name: "program chain",
        pos: PROG_CHAIN_POS,
        stored,
        computed,
        ok: stored == computed,
    })
}

/// The chain is only meaningful for a full image that also has a real TDI code
/// block: a padded 32 kB calibration read carries the 0x23A6 id but no program
/// area, and a blank/foreign image has neither.
fn has_program_area(buf: &[u8], found: &[FoundChecksum]) -> bool {
    buf.len() == FULL_SIZE && found.iter().any(|f| f.0 == CODE_BLOCK.2)
}

// ---------------------------------------------------------------------------
// Self-test: regression vectors from `edc15m_checksum.py --selftest`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
#[cfg(test)]
mod marker_path;
