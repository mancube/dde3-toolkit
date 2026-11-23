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
        vec![0x3FBFC, 0x13FFC, PROG_CHAIN_POS],
        "marker path must find both TDI blocks and the program chain"
    );
    assert_eq!(found[0].name, "calibration");
    assert_eq!(found[1].name, "code");
    assert_eq!(found[2].name, "program chain");
    assert!(found.iter().all(|c| !c.ok));

    let fixed = correct(&img);
    assert!(verify(&fixed).iter().all(|c| c.ok));
}

#[test]
fn no_block_in_blank_image() {
    let img = vec![0u8; FULL_SIZE];
    assert!(verify(&img).is_empty());
}
