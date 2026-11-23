use super::*;

fn synthetic_image(seed: u8) -> Vec<u8> {
    let mut img = vec![0u8; IMAGE_SIZE];
    for (i, b) in img.iter_mut().enumerate() {
        *b = seed.wrapping_add((i % 251) as u8);
    }
    img
}

#[test]
fn write_image_is_full_and_checked() {
    let img = synthetic_image(0x11);
    let fixed = prepare_image(&img, Mode::Write).expect("correction succeeds");
    assert_eq!(fixed.len(), IMAGE_SIZE);
    let bad: Vec<_> = checksum::verify(&fixed)
        .iter()
        .filter(|r| !r.ok)
        .map(|r| r.name)
        .collect();
    assert!(bad.is_empty(), "corrected image must verify: {bad:?}");
}

#[test]
fn write_image_rejects_wrong_size() {
    let err = prepare_image(&[0u8; 1024], Mode::Write).unwrap_err();
    assert!(err.to_string().contains("256 KB"));
}

#[test]
fn cal_image_from_full_is_32kb() {
    let img = synthetic_image(0x22);
    let cal = prepare_image(&img, Mode::WriteCal).expect("full image cal slice");
    assert_eq!(cal.len(), CAL_LEN);
}

#[test]
fn cal_image_rejects_wrong_size() {
    let err = prepare_image(&[0u8; 4096], Mode::WriteCal).unwrap_err();
    assert!(err.to_string().contains("256 KB or 32 KB"));
}
