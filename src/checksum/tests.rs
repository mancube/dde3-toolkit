use super::*;

fn vector(len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; len];
    let mut x: u32 = VECTOR_SEED;
    for b in buf.iter_mut() {
        x = x.wrapping_mul(1103515245).wrapping_add(12345);
        *b = (x >> 16) as u8;
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
    assert!(verify(&fixed)[0].ok);
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

/// Known-answer cross-check against `progsum.py` (an independent Python
/// implementation) on the same LCG buffer: solving must produce `fc520cf8`
/// and the full walk — balancing words included — must end at the boot
/// self-test target.
#[test]
fn program_chain_vector_matches_progsum() {
    let buf = vector(FULL_SIZE);
    let words = prog_solve(&buf);
    assert_eq!(words, [0xfc, 0x52, 0x0c, 0xf8], "progsum.py solve() cross-check");

    let mut img = buf;
    img[PROG_CHAIN_POS..PROG_CHAIN_POS + 4].copy_from_slice(&words);
    let (h, g, _) = prog_run(&img, &prog_segments(PROG_CHAIN_POS + 4));
    assert_eq!((h, g), PROG_TARGET, "the walk must end at the boot target");
}

/// A full image with the real EDC15M layout (id 0x23A6, stale TDI code
/// slot) must expose the program chain, and `correct` must fix it *after*
/// the code slot — solving over the raw image would leave the chain stale.
#[test]
fn program_chain_is_corrected_after_the_code_slot() {
    let mut img = vec![0u8; FULL_SIZE];
    img[0x10000..0x10004].copy_from_slice(&MARKER);
    img[0x13FFA..0x13FFC].copy_from_slice(&PROG_CHAIN_ID);
    img[0x13FFC..0x14000].copy_from_slice(&1u32.to_le_bytes());
    img[PROG_CHAIN_ID_POS..PROG_CHAIN_ID_POS + 2].copy_from_slice(&PROG_CHAIN_ID);
    img[0x3FBFA..0x3FBFC].copy_from_slice(&PROG_CHAIN_ID);
    img[0x3FBFC..0x3FC00].copy_from_slice(&2u32.to_le_bytes());

    let checks = verify(&img);
    let chain = checks
        .iter()
        .find(|c| c.name == "program chain")
        .expect("a full image with a code slot carries the chain");
    assert_eq!(chain.pos, PROG_CHAIN_POS);
    assert!(!chain.ok, "a blank balancing word must fail");
    assert!(
        !prog_solve(&img).eq(&prog_solve(&{
            let mut tdi = img.clone();
            for c in checks.iter().filter(|c| c.name != "program chain") {
                tdi[c.pos..c.pos + 4].copy_from_slice(&c.computed.to_le_bytes());
            }
            tdi
        })),
        "a stale code slot must change the solved chain (order matters)"
    );

    let fixed = correct(&img);
    assert!(
        verify(&fixed).iter().all(|c| c.ok),
        "every checksum, chain included, must verify after correction"
    );
    assert_eq!(fixed, correct(&fixed), "correction must be idempotent");
}

/// A 32 kB calibration read padded with 0xFF carries the real 0x23A6 id
/// at 0x38B7A but has no program area: no chain row, no chain write.
#[test]
fn padded_partial_with_real_cal_id_has_no_program_chain() {
    let mut img = vec![0xFFu8; FULL_SIZE];
    img[PROG_CHAIN_ID_POS..PROG_CHAIN_ID_POS + 2].copy_from_slice(&PROG_CHAIN_ID);
    img[0x3FBFA..0x3FBFC].copy_from_slice(&PROG_CHAIN_ID);
    img[0x3FBFC..0x3FC00].copy_from_slice(&7u32.to_le_bytes());

    let found = verify(&img);
    assert_eq!(found.len(), 1, "only the calibration block");
    assert_eq!(found[0].name, "calibration");
    let fixed = correct(&img);
    assert_eq!(
        &fixed[PROG_CHAIN_POS..PROG_CHAIN_POS + 4],
        &[0xFF; 4],
        "the padded program area must never be written"
    );
}

#[test]
fn rol_ror_are_inverses() {
    let v: u16 = 0xABCD;
    for n in 0..16 {
        assert_eq!(ror(rol(v, n), n), v);
    }
}
