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

/// Calibration block seeds: (h at entry, g at entry, subtracted from h, xored into g)
const CAL_SEEDS: (u16, u16, u16, u16) = (0x8631, 0xEFCD, 0x8631, 0xDF9B);
/// Code block seeds
const CODE_SEEDS: (u16, u16, u16, u16) = (0x0000, 0x0000, 0x8631, 0xDF9B);

/// The fixed-geometry code block in a full read.
const CODE_BLOCK: (usize, usize, usize, (u16, u16, u16, u16)) =
    (0x10000, 0x14000, 0x13FFC, CODE_SEEDS);

/// A blank word is an unwritten or padded location, not a stale checksum
const ERASED_0: u32 = 0xFFFF_FFFF;
const ERASED_1: u32 = 0x0000_0000;

/// Regression vectors: LCG buffer, see `edc15m_checksum.py --selftest`
const VECTOR_SEED: u32 = 0x12345678;
const VECTOR_LEN: usize = 0x8000;
const VECTOR_CHECKSUM: u32 = 0x007B_C5EB; // calibration block over [0x0B80, 0x7C00)
const CODE_VECTOR_LEN: usize = 0x14000;
const CODE_VECTOR_CHECKSUM: u32 = 0x2477_887F; // code block over [0x10000, 0x14000)

// ---------------------------------------------------------------------------
// Public interface
// ---------------------------------------------------------------------------

/// One discovered checksum, checked. `pos`/`start`/`end` are offsets into the
/// buffer that was verified.
#[derive(Clone, Copy, Debug)]
pub struct Checksum {
    /// "calibration" or "code"
    pub name: &'static str,
    /// File offset of the 4-byte checksum value
    pub pos: usize,
    /// Range the checksum covers: [start, end)
    pub start: usize,
    pub end: usize,
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
) -> Option<(usize, usize, usize, (u16, u16, u16, u16))> {
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
/// tagged block (`\xc3\x3c\x55\xaa`), and for a 32 kB partial read — which
/// has no tag — off the id the last block carries itself.  A full read
/// additionally gets the fixed-geometry code block, once the calibration
/// block has been recognised, and only while its field is not blank.
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

    if found.len() > 0 && n == FULL_SIZE {
        let (start, end, pos, seeds) = CODE_BLOCK;
        if has_checksum(buf, pos) {
            found.push((pos, start, end, seeds));
        }
    }

    found
}

/// Verify all checksums in a buffer.
pub fn verify(buf: &[u8]) -> Vec<Checksum> {
    find_checksums(buf)
        .into_iter()
        .map(|(pos, start, end, seeds)| {
            let stored = u32::from_le_bytes([buf[pos], buf[pos + 1], buf[pos + 2], buf[pos + 3]]);
            let computed = checksum(buf, start, end, seeds);
            Checksum {
                name: name_for(pos),
                pos,
                start,
                end,
                stored,
                computed,
                ok: stored == computed,
            }
        })
        .collect()
}

/// Correct all discovered checksums in a buffer, returning a new buffer with
/// the fixes applied. A buffer without a recognised block is returned as-is.
pub fn correct(buf: &[u8]) -> Vec<u8> {
    let mut out = buf.to_vec();
    for c in &verify(buf) {
        out[c.pos..c.pos + 4].copy_from_slice(&c.computed.to_le_bytes());
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
// Self-test: regression vectors from `edc15m_checksum.py --selftest`
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    fn vector(len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        let mut x: u32 = VECTOR_SEED;
        for i in 0..len {
            x = x.wrapping_mul(1103515245).wrapping_add(12345) & 0xFFFF_FFFF;
            buf[i] = (x >> 16) as u8;
        }
        buf
    }

    #[test]
    fn selftest_calibration_vector() {
        let buf = vector(VECTOR_LEN);
        let got = checksum(&buf, 0xB80, 0x7C00, CAL_SEEDS);
        assert_eq!(got, VECTOR_CHECKSUM, "calibration vector checksum");
    }

    #[test]
    fn selftest_code_vector() {
        let buf = vector(CODE_VECTOR_LEN);
        let got = checksum(&buf, 0x10000, 0x14000, CODE_SEEDS);
        assert_eq!(got, CODE_VECTOR_CHECKSUM, "code vector checksum");
    }

    /// A 32 kB block that carries an id pair, with a deliberately stale
    /// checksum at its slot.
    fn synthetic_block(id: [u8; 2], stale: u32) -> Vec<u8> {
        let mut buf = vec![0u8; BLOCK];
        buf[0xB7A..0xB7C].copy_from_slice(&id);
        // id six bytes in front of the slot: slot = 0x7080, id at 0x707A
        buf[0x707A..0x707C].copy_from_slice(&id);
        buf[0x707C..0x7080].copy_from_slice(&stale.to_le_bytes());
        buf
    }

    #[test]
    fn correct_fixes_a_stale_block_and_is_idempotent() {
        let img = synthetic_block([0x12, 0x34], 0xDEAD_BEEF);
        assert_eq!(verify(&img).len(), 1, "the synthetic block must be found");
        assert!(!verify(&img)[0].ok, "the stale value must fail first");
        let fixed = correct(&img);
        let results = verify(&fixed);
        assert_eq!(results.len(), 1);
        assert!(results[0].ok, "corrected block must verify");
        assert_eq!(fixed, correct(&fixed), "correction must be idempotent");
    }

    #[test]
    fn correct_at_touches_only_the_named_slot() {
        let img = synthetic_block([0x12, 0x34], 0xDEAD_BEEF);
        let pos = verify(&img)[0].pos;
        let fixed = correct_at(&img, pos).expect("slot is discovered");
        assert_eq!(verify(&fixed)[0].ok, true);
        assert!(
            correct_at(&img, 0x13FFC).is_none(),
            "no block at 0x13FFC in a 32 kB buffer"
        );
    }

    /// The GUI's partial-read path: a 32 kB calibration read padded with
    /// 0xFF into a 256 kB image must expose exactly the calibration block at
    /// 0x3FBFC (blank code field -> no code block).
    #[test]
    fn padded_partial_read_finds_only_calibration() {
        let mut img = vec![0xFFu8; FULL_SIZE];
        let cal = synthetic_block([0x12, 0x34], 0xDEAD_BEEF);
        // real reads keep the pair at id 0x7BFA + value 0x7BFC (slot 0x7C00)
        let cal = {
            let mut c = cal;
            // move the id+value pair from the synthetic 0x7080 slot to 0x7C00
            let pair = c[0x707A..0x7080].to_vec();
            for b in &mut c[0x707A..0x7080] {
                *b = 0;
            }
            c[0x7BFA..0x7C00].copy_from_slice(&pair);
            c
        };
        img[CAL_START..CAL_START + CAL_LEN].copy_from_slice(&cal);
        let found = verify(&img);
        assert_eq!(found.len(), 1, "padded partial: only the calibration block");
        assert_eq!(found[0].pos, 0x3FBFC);
        assert_eq!(found[0].name, "calibration");
        assert!(!found[0].ok);
        let fixed = correct(&img);
        assert!(verify(&fixed).iter().all(|c| c.ok));
    }

    #[test]
    fn all_same_byte_buffer_has_no_block() {
        let buf = vec![0xAAu8; FULL_SIZE];
        assert!(find_checksums(&buf).is_empty());
    }

    #[test]
    fn rol_ror_are_inverses() {
        let v: u16 = 0xABCD;
        for n in 0..16 {
            assert_eq!(ror(rol(v, n), n), v);
        }
    }
}

#[cfg(test)]
mod marker_path {
    use super::*;

    /// A full-size image in the real E46 layout: a tagged code block at
    /// 0x10000 (marker C3 3C 55 AA, id pair at 0x13FFA, code checksum at
    /// 0x13FFC) and the calibration block at 0x38000 with the same id at
    /// 0x38B7A and its pair at 0x3FBFA.  Both checksums must be discovered.
    #[test]
    fn full_read_finds_code_and_calibration() {
        let id = [0x23, 0xA6];
        let mut img = vec![0u8; FULL_SIZE];
        // tagged code block
        img[0x10000..0x10004].copy_from_slice(&MARKER);
        img[0x13FFA..0x13FFC].copy_from_slice(&id);
        img[0x13FFC..0x14000].copy_from_slice(&1u32.to_le_bytes());
        // calibration block
        img[0x38B7A..0x38B7C].copy_from_slice(&id);
        img[0x3FBFA..0x3FBFC].copy_from_slice(&id);
        img[0x3FBFC..0x3FC00].copy_from_slice(&2u32.to_le_bytes());

        let found = verify(&img);
        let pos: Vec<usize> = found.iter().map(|c| c.pos).collect();
        assert_eq!(
            pos,
            vec![0x3FBFC, 0x13FFC],
            "marker path must find both blocks"
        );
        assert_eq!(found[0].name, "calibration");
        assert_eq!(found[1].name, "code");
        assert!(!found[0].ok && !found[1].ok);

        let fixed = correct(&img);
        assert!(verify(&fixed).iter().all(|c| c.ok));
    }

    #[test]
    fn no_block_in_blank_image() {
        let img = vec![0u8; FULL_SIZE];
        assert!(verify(&img).is_empty());
    }
}
