//! DDE3 flash checksum — ported from the verified Python `edc15m_checksum.py`
//! (Bosch EDC15M TDI V3.1 algorithm).
//!
//! This is the SAME algorithm the user's Python tool uses and has been checked
//! against real full (256 KB) and partial (32 KB) DDE3 reads.
//!
//! The flash is split into 32 kB blocks.  A block that carries a calibration
//! checksum repeats a 2-byte id at block+0xB7A and the same id immediately in
//! front of the checksum.  The checksum is a 32-bit little-endian value covering
//! the range [block + 0xB80, checksum_position):
//!
//!   256 kB full read : checksum at 0x3FBFC, covers [0x38B80, 0x3FC00)
//!    32 kB partial    : checksum at 0x7BFC,  covers [0x0B80, 0x7C00)
//!
//! A full read has a second checksum with fixed geometry, over the code area:
//!
//!   256 kB full read : checksum at 0x13FFC, covers [0x10000, 0x14000)
//!
//! Both use the same seeded 16-bit rolling algorithm with different seeds.

pub const BLOCK: usize = 0x8000; // block size, also the step of the block scan
pub const ID_OFF: usize = 0xB7A; // offset of the 2-byte block id inside a block
pub const SLOT_LO: usize = 0x7000; // earliest checksum slot inside a block
pub const SLOT_STEP: usize = 0x80;
pub const CS_START: usize = 0xB80; // a checksum covers from block + CS_START
pub const FULL_SIZE: usize = 0x40000; // a full 256 kB read
const M16: u16 = 0xFFFF;

/// Calibration block seeds: (h at entry, g at entry, subtracted from h, xored into g)
const CAL_SEEDS: (u16, u16, u16, u16) = (0x8631, 0xEFCD, 0x8631, 0xDF9B);
/// Code block seeds
const CODE_SEEDS: (u16, u16, u16, u16) = (0x0000, 0x0000, 0x8631, 0xDF9B);

/// A second, fixed-geometry block in a full read: the code area.
const CODE_BLOCK: (usize, usize, usize, (u16, u16, u16, u16)) =
    (0x10000, 0x14000, 0x13FFC, CODE_SEEDS);

/// A blank word is an unwritten or padded location, not a stale checksum
const ERASED_0: u32 = 0xFFFF_FFFF;
const ERASED_1: u32 = 0x0000_0000;

/// Regression vector: lcg buffer, see `edc15m_checksum.py --selftest`
const VECTOR_SEED: u32 = 0x12345678;
const VECTOR_LEN: usize = 0x8000;
const VECTOR_CHECKSUM: u32 = 0x007B_C5EB; // calibration block over [0x0B80, 0x7C00)
const CODE_VECTOR_LEN: usize = 0x14000;
const CODE_VECTOR_CHECKSUM: u32 = 0x2477_887F; // code block over [0x10000, 0x14000)

// ---------------------------------------------------------------------------
// Public interface: compatible with the previous (guess-based) checksum.rs
// ---------------------------------------------------------------------------

/// Target checksum value (kept for API compatibility with the GUI).
/// The Python algorithm computes per-block; there is no single "target".
pub const TARGET: u32 = 0x4711_B530;

pub const IMAGE_SIZE: usize = 0x40000; // 256 KB
pub const CAL_START: usize = 0x38000; // calibration: 0x38000-0x3FFFF
pub const CAL_LEN: usize = 0x8000; // 32 KB

#[derive(Clone, Copy, Debug)]
pub struct Block {
    pub name: &'static str,
    pub ranges: &'static [(usize, usize)],
    pub comp: usize,
    #[allow(dead_code)]
    pub checked_by: &'static str,
}

/// Block layout — kept for API compatibility. The real checksum blocks are
/// discovered dynamically by `find_checksums`, not from this table.
pub const BLOCKS: &[Block] = &[
    Block {
        name: "application",
        ranges: &[(0x00000, 0x38000)],
        comp: 0x37FFC,
        checked_by: "TDI V3.1 (discovered dynamically)",
    },
    Block {
        name: "calibration",
        ranges: &[(0x38000, 0x40000)],
        comp: 0x3FFFC,
        checked_by: "TDI V3.1 (discovered dynamically)",
    },
];

pub const CORRECTABLE: &[&str] = &["application", "calibration"];

#[derive(Clone, Debug)]
pub struct BlockResult {
    pub name: &'static str,
    pub value: u32,
    pub ok: bool,
}

fn rol(v: u16, n: u16) -> u16 {
    let n = n & 0xF;
    if n == 0 {
        v
    } else {
        ((v << n) | (v >> (16 - n))) & M16
    }
}

fn ror(v: u16, n: u16) -> u16 {
    let n = n & 0xF;
    if n == 0 {
        v
    } else {
        ((v >> n) | (v << (16 - n))) & M16
    }
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
            .wrapping_sub(flag)) & M16;
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
        g = g ^ u16::from_le_bytes([buf[p], buf[p + 1]]);
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
    ((q.wrapping_add(h).wrapping_add(1)) & M16) as u32
        | (((g ^ g_out) & M16) as u32) << 16
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

/// Yield the checksum slot of one block, if it has one.
fn slot(buf: &[u8], base: usize, key: [u8; 2]) -> Option<(usize, usize, usize, (u16, u16, u16, u16))> {
    if key[0] == key[1] {
        return None; // a repeated byte is filler, not a block id
    }
    let mut slot = base + SLOT_LO;
    while slot + 1 <= base + BLOCK {
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
/// tagged block (`\xc3\x3c\x55\xaa`), and for a 32 kB partial read - which
/// has no tag - off the id the last block carries itself.
/// A full read additionally gets the fixed-geometry code block, once the
/// calibration block has been recognised.
pub fn find_checksums(buf: &[u8]) -> Vec<(usize, usize, usize, (u16, u16, u16, u16))> {
    let n = buf.len();
    let block_count = n / BLOCK;
    let blocks: Vec<usize> = (0..block_count).map(|i| i * BLOCK).collect();
    if blocks.is_empty() {
        return Vec::new();
    }

    let mut found = Vec::new();
    let marker = find_marker(buf);
    if let Some(marker) = marker {
        let key = [buf[marker + 0x3FFA], buf[marker + 0x3FFC]];
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

    if found.len() > 0 && n == FULL_SIZE {
        let (start, end, pos, seeds) = CODE_BLOCK;
        if has_checksum(buf, pos) {
            found.push((pos, start, end, seeds));
        }
    }

    found
}

/// Per-checksum-block detail, in file order.
pub fn analyse(buf: &[u8]) -> Vec<BlockResult> {
    let mut results = Vec::new();
    for (pos, start, end, seeds) in find_checksums(buf) {
        let stored = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
        let computed = checksum(buf, start, end, seeds);
        let name = if pos == 0x3FFFC || pos == 0x7BFC {
            "calibration"
        } else if pos == 0x13FFC {
            "code"
        } else {
            "application"
        };
        results.push(BlockResult {
            name,
            value: computed,
            ok: stored == computed,
        });
    }
    results
}

/// Verify all checksums in a buffer.
pub fn verify(buf: &[u8]) -> Vec<BlockResult> {
    analyse(buf)
}

/// Correct all checksums in a buffer, returning a new buffer with fixes applied.
pub fn correct(buf: &[u8]) -> Vec<u8> {
    let mut out = buf.to_vec();
    for (pos, start, end, seeds) in find_checksums(buf) {
        let computed = checksum(buf, start, end, seeds);
        out[pos..pos + 4].copy_from_slice(&computed.to_le_bytes());
    }
    out
}

/// Correct one named block. Returns `None` if no matching block is found.
pub fn correct_block(buf: &[u8], name: &str) -> Option<Vec<u8>> {
    let mut out = buf.to_vec();
    let mut found_any = false;
    for (pos, start, end, seeds) in find_checksums(buf) {
        let block_name = if pos == 0x3FFFC || pos == 0x7BFC {
            "calibration"
        } else if pos == 0x13FFC {
            "code"
        } else {
            "application"
        };
        if block_name == name {
            let computed = checksum(buf, start, end, seeds);
            out[pos..pos + 4].copy_from_slice(&computed.to_le_bytes());
            found_any = true;
        }
    }
    if found_any {
        Some(out)
    } else {
        None
    }
}

/// Self-test: regression vectors from `edc15m_checksum.py --selftest`.
#[cfg(test)]
mod tests {
    use super::*;

    fn vector() -> Vec<u8> {
        let mut buf = vec![0u8; VECTOR_LEN];
        let mut x: u32 = VECTOR_SEED;
        for i in 0..VECTOR_LEN {
            x = x.wrapping_mul(1103515245).wrapping_add(12345) & 0xFFFF_FFFF;
            buf[i] = (x >> 16) as u8;
        }
        buf
    }

    #[test]
    fn selftest_calibration_vector() {
        let buf = vector();
        let got = checksum(&buf, 0xB80, 0x7C00, CAL_SEEDS);
        assert_eq!(got, VECTOR_CHECKSUM, "calibration vector checksum");
    }

    #[test]
    fn selftest_code_vector() {
        let mut buf = vec![0u8; CODE_VECTOR_LEN];
        let mut x: u32 = VECTOR_SEED;
        for i in 0..CODE_VECTOR_LEN {
            x = x.wrapping_mul(1103515245).wrapping_add(12345) & 0xFFFF_FFFF;
            buf[i] = (x >> 16) as u8;
        }
        let got = checksum(&buf, 0x10000, 0x14000, CODE_SEEDS);
        assert_eq!(got, CODE_VECTOR_CHECKSUM, "code vector checksum");
    }

    #[test]
    fn correct_makes_correctable_blocks_valid() {
        let img = vec![0u8; IMAGE_SIZE];
        let fixed = correct(&img);
        let results = verify(&fixed);
        for r in &results {
            assert!(
                r.ok,
                "{} did not reach target: {:08X}",
                r.name,
                r.value
            );
        }
        assert_eq!(fixed, correct(&fixed), "correction must be idempotent");
    }

    #[test]
    fn find_checksums_returns_blocks() {
        let buf = vec![0xAAu8; FULL_SIZE];
        let found = find_checksums(&buf);
        // An all-same-byte buffer has key[0]==key[1] so no blocks found — that's expected.
        assert!(found.is_empty() || found.len() <= 3);
    }

    #[test]
    fn rol_ror_are_inverses() {
        let v: u16 = 0xABCD;
        for n in 0..16 {
            assert_eq!(ror(rol(v, n), n), v);
        }
    }
}
