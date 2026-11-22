use super::dump_image;

#[test]
fn dump_image_pads_the_192kb_test_write_with_ff() {
    // Distinct content: boot area 0x11, written span 0xA5.
    let mut flash = vec![0x11u8; 0x40000];
    flash[0x10000..].fill(0xA5);
    let img = dump_image(&flash, 0x10000, 0x40000);
    assert_eq!(img.len(), 0x40000, "full 256 KB image");
    assert!(img[..0x10000].iter().all(|&b| b == 0xFF), "boot area padded FF");
    assert!(img[0x10000..].iter().all(|&b| b == 0xA5), "written span verbatim");
}

#[test]
fn dump_image_keeps_other_spans_compact() {
    let flash: Vec<u8> = (0..0x40000u32).map(|i| (i & 0xFF) as u8).collect();
    let img = dump_image(&flash, 0x38000, 0x40000);
    assert_eq!(img.len(), 0x8000, "cal span stays compact");
    assert_eq!(&img[..4], &flash[0x38000..0x38004]);
    let img = dump_image(&flash, 0x10000, 0x10080);
    assert_eq!(img.len(), 0x80, "short span stays compact");
    assert_eq!(&img[..4], &flash[0x10000..0x10004]);
}
