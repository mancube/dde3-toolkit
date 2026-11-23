use super::*;

fn frame(payload: &[u8]) -> Vec<u8> {
    crate::protocol::build_frame(payload)
}

#[test]
fn names_known_services() {
    assert_eq!(name_service(&[0x12, 0x04, 0x00]), "IDENT");
    assert_eq!(name_service(&[0x12, 0x09, 0x06, 0x00, 0x80, 0x03, 0x80]), "FLASH_LESEN");
    assert_eq!(name_service(&[0x12, 0x09, 0x07, 0x06]), "FLASH_LOESCHEN (ERASE!)");
    assert_eq!(name_service(&[0x12, 0x09, 0x07, 0x02]), "FLASH_SCHREIBEN (WRITE!)");
    assert_eq!(name_service(&[0x12, 0x08, 0x90, 0x42]), "SEED_KEY");
    // AIF / programming-record read (sid 0x02) and the 0x90 key send.
    assert_eq!(name_service(&[0x12, 0x05, 0x02, 0x00, 0x15]), "AIF_LESEN");
    assert_eq!(name_service(&[0x12, 0x08, 0x90, 0x39, 0x3C, 0x3F, 0x42]), "KEY_SEND");
    assert_eq!(name_service(&[0x12, 0x0F, 0x0B, 0x10]), "MW_SELECT_LESEN (live data)");
    assert_eq!(name_service(&[0x12, 0x05, 0x14, 0x01]), "FS_QUICK_LESEN (DTC count)");
    assert_eq!(name_service(&[0x12, 0x04, 0x05]), "FS_LOESCHEN (DTC clear)");
    // ECU reply with OKAY status at [2]
    assert_eq!(name_service(&[0x12, 0x05, 0xA0, 0x03]), "REPLY");
    // the periodic 16-byte live block: 12 10 A0 10 00 ...
    assert_eq!(
        name_service(&[0x12, 0x10, 0xA0, 0x10]),
        "LIVE_BROADCAST (periodic/5-id)"
    );
    // unknown
    assert_eq!(name_service(&[0x12, 0x03, 0x77]), "service ??");
}

#[test]
fn real_car_wire_fixture_splits_frames() {
    // Merged capture from `dde3 toolkit log.txt` (M47): ECU live reply
    // immediately followed by the echo of our request.
    let wire: Vec<u8> = vec![
        0x12, 0x10, 0xA0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xDE, 0x0B, 0x26, 0x02,
        0x4A, 0x0A, 0x12, 0x0F, 0x0B, 0x10, 0x0F, 0x83, 0x0F, 0x10, 0x0F, 0x40, 0x0F, 0x00,
        0x0F, 0x65, 0xBF,
    ];
    let mut f = Framer::new();
    let events = f.feed(&wire, Instant::now());
    let frames: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Frame { payload } => Some(payload.clone()),
            Event::Raw(_) => None,
        })
        .collect();
    assert_eq!(frames.len(), 2, "expected 2 frames, got {events:?}");
    assert_eq!(frames[0][2], 0xA0, "live reply first");
    assert_eq!(frames[1][3], 0x10, "echo of MW_SELECT_LESEN second");
}

#[test]
fn leading_garbage_is_kept_as_raw_and_frame_still_found() {
    let mut wire = vec![0x00, 0xFF];
    wire.extend(frame(&[0x12, 0x04, 0x00]));
    let mut f = Framer::new();
    let events = f.feed(&wire, Instant::now());
    assert_eq!(events.len(), 2, "RAW run then frame, got: {events:?}");
    match &events[0] {
        Event::Raw(b) => assert_eq!(b, &vec![0x00, 0xFF]),
        _ => panic!("expected RAW first"),
    }
    match &events[1] {
        Event::Frame { payload } => assert_eq!(payload, &vec![0x12, 0x04, 0x00]),
        _ => panic!("expected FRAME second"),
    }
}

#[test]
fn unanchored_ascii_lands_in_raw() {
    // Unanchored ASCII bytes (no 0x12 start) are kept as RAW. The real
    // IDENT reply does not do this (it is enveloped, see protocol.rs) —
    // this just pins the framer's verbatim RAW behaviour for such bytes.
    let ident = b"8875201";
    let mut wire = ident.to_vec();
    wire.push(crate::protocol::xor_cs(&ident[..]));
    let t = Instant::now();
    let mut f = Framer::new();
    let events = f.feed(&wire, t);
    // Nothing was framed (no anchor); the bytes surface verbatim as RAW
    // (unanchored runs flush immediately, preserving order and content).
    let raw: Vec<u8> = events
        .iter()
        .filter_map(|e| match e {
            Event::Raw(b) => Some(b.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect();
    assert_eq!(&raw[..ident.len()], ident, "ident string preserved: {events:?}");
    assert!(f.stale(t + STALE).is_none(), "buffer already drained");
}

#[test]
fn partial_frame_reassembles_within_stale_window() {
    // WinKFP's ~50 ms XOR gap is below STALE, so a split frame reassembles.
    let mut f = Framer::new();
    let t = Instant::now();
    let full = frame(&[0x12, 0x04, 0x00]);
    let _ = f.feed(&full[..3], t); // payload without trailing XOR
    assert!(
        f.stale(t + Duration::from_millis(50)).is_none(),
        "50 ms gap is normal"
    );
    let t2 = t + Duration::from_millis(51);
    let events = f.feed(&full[3..], t2);
    match &events[0] {
        Event::Frame { payload } => assert_eq!(payload, &vec![0x12, 0x04, 0x00]),
        _ => panic!("expected reassembled frame, got {events:?}"),
    }
}

#[test]
fn partial_frame_flushes_as_raw_when_truly_stale() {
    // If the trailing XOR never arrives, the stale flush keeps the bytes.
    let full = frame(&[0x12, 0x04, 0x00]);
    let t = Instant::now();
    let mut f = Framer::new();
    let _ = f.feed(&full[..3], t);
    let ev = f.stale(t + STALE + Duration::from_millis(1));
    assert!(
        matches!(ev, Some(Event::Raw(ref b)) if b == &full[..3]),
        "got {ev:?}"
    );
}

/// Regression: a 73-byte `FLASH_SCHREIBEN` whose payload contains an
/// in-band XOR coincidence (running XOR of the prefix equals the next
/// byte at index 21 — exactly the real-car write to 0x10100 from the
/// 2026-11-11 session) must NOT be truncated at 22 bytes. The LEN byte
/// (0x49 = 73 wire bytes) is authoritative.
#[test]
fn long_write_with_in_band_xor_collision_stays_intact() {
    // Frame: 12 49 07 02 01 01 00 40 <64 data bytes> <xor> = 73 wire bytes.
    // Header (8) + first 13 data bytes = 21 bytes (indices 0..20).
    let mut f: Vec<u8> = vec![
        0x12, 0x49, 0x07, 0x02, 0x01, 0x01, 0x00, 0x40,
        0xAA, 0x55, 0xFB, 0x88, 0xE6, 0xF2, 0x00, 0x40, 0x46, 0xF6, 0xEE,
        0x11, 0x22,
    ];
    assert_eq!(f.len(), 21, "header (8) + 13 data bytes = indices 0..20");
    // Force the collision: byte 21 == XOR of bytes 0..21 (the in-band
    // coincidence that truncated write #5 on the real car).
    f.push(crate::protocol::xor_cs(&f[..21]));
    // Remaining data bytes to reach 64 total: 64 - 13 - 1 = 50.
    f.extend(std::iter::repeat_n(0x5A, 50));
    f.push(crate::protocol::xor_cs(&f[..])); // true trailing XOR, LEN 73
    assert_eq!(f.len(), 73, "LEN byte 0x49 must equal the wire length");

    // Fed all at once: one complete frame, zero RAW.
    let mut fr = Framer::new();
    let events = fr.feed(&f, Instant::now());
    let frames: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Frame { payload } => Some(payload.clone()),
            Event::Raw(_) => None,
        })
        .collect();
    let raws: Vec<_> = events.iter().filter(|e| matches!(e, Event::Raw(_))).collect();
    assert_eq!(frames.len(), 1, "one frame, got {events:?}");
    assert!(raws.is_empty(), "no RAW runs, got {events:?}");
    assert_eq!(frames[0].len(), 72, "payload = 73 wire bytes minus XOR");
    assert_eq!(&frames[0][..4], &[0x12, 0x49, 0x07, 0x02]);

    // Fed in small chunks (as the UART delivers them): still one frame,
    // nothing truncated at the byte-21 collision while incomplete.
    let mut fr = Framer::new();
    let t = Instant::now();
    let mut events = Vec::new();
    for chunk in f.chunks(13) {
        events.extend(fr.feed(chunk, t));
    }
    let frames: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::Frame { payload } => Some(payload.clone()),
            Event::Raw(_) => None,
        })
        .collect();
    let raws: Vec<_> = events.iter().filter(|e| matches!(e, Event::Raw(_))).collect();
    assert_eq!(frames.len(), 1, "reassembled across chunks, got {events:?}");
    assert!(raws.is_empty(), "no RAW runs, got {events:?}");
    assert_eq!(frames[0].len(), 72);
}

#[test]
fn drain_returns_pending_bytes_at_end() {
    let mut f = Framer::new();
    let full = frame(&[0x12, 0x04, 0x0D]);
    let t = Instant::now();
    let _ = f.feed(&full[..2], t);
    let events = f.drain();
    assert!(
        matches!(events.as_slice(), [Event::Raw(ref b)] if b == &full[..2]),
        "got {events:?}"
    );
}
