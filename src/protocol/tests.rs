use super::*;

#[test]
fn frame_round_trip() {
    // Confirmed wire format: payload + trailing XOR, no B8/F1/LEN header.
    let bytes = build_frame(&[0x12, 0x04, 0x0D]);
    assert_eq!(bytes, vec![0x12, 0x04, 0x0D, 0x1B]);
    let mut parser = FrameParser::default();
    parser.feed(&bytes);
    let f = parser.take_frame().expect("frame");
    assert_eq!(f.payload, vec![0x12, 0x04, 0x0D]);
}

#[test]
fn mw_select_batched_length() {
    // Tested logger (MW_SELECT_LESEN_NORM) rule: LEN = 5 + 2n (total wire
    // bytes, prefix `12 LEN 0B 10` + 2 bytes/id + trailing XOR).
    let f = frame_mw_select(&[0x0F65]);
    assert_eq!(f[0], 0x12);
    assert_eq!(f[1], 7, "single id length must be 5 + 2*1");
    assert_eq!(f[2..4], [0x0B, 0x10]);
    assert_eq!(f.len(), 6);
    // A batched set of 4 ids (SET_1 in `testo customjobs.txt`).
    let f = frame_mw_select(&[0x0F62, 0x0F00, 0x0F30, 0x0E81]);
    assert_eq!(f[1], 13, "4 ids -> 5 + 8 = 13");
    assert_eq!(f.len(), 12);
    // id order preserved, big-endian.
    assert_eq!(&f[4..6], [0x0F, 0x62]);
    assert_eq!(&f[10..12], [0x0E, 0x81]);
}

#[test]
fn captured_winkfp_frames() {
    // Byte-exact frames from the 2026-11-09 passive capture of WinKFP.
    assert_eq!(build_frame(&[0x12, 0x04, 0x0D]), vec![0x12, 0x04, 0x0D, 0x1B]);
    assert_eq!(build_frame(&[0x12, 0x04, 0x00]), vec![0x12, 0x04, 0x00, 0x16]);
    assert_eq!(
        build_frame(&[0x12, 0x05, 0x90, 0x56]),
        vec![0x12, 0x05, 0x90, 0x56, 0xD1]
    );
}

#[test]
fn parser_keeps_partial_frame_across_reads() {
    // WinKFP sends the trailing XOR ~50 ms after the payload.
    let mut parser = FrameParser::default();
    parser.feed(&[0x12, 0x04, 0x00]);
    assert!(parser.take_frame().is_none());
    parser.feed(&[0x16]);
    let f = parser.take_frame().expect("frame");
    assert_eq!(f.payload, vec![0x12, 0x04, 0x00]);
}

#[test]
fn parser_skips_garbage_and_resyncs() {
    let mut parser = FrameParser::default();
    parser.feed(&[0x00, 0xFF]);
    assert!(parser.take_frame().is_none());
    parser.feed(&build_frame(&[0x12, 0x04, 0x00]));
    let f = parser.take_frame().expect("frame");
    assert_eq!(f.payload, vec![0x12, 0x04, 0x00]);
}

#[test]
fn parser_holds_full_mpps_write_echo() {
    // MPPS-style cal write: a 224-byte data block is one 233-byte wire
    // frame (`12 E9 07 02 <addr3> E0 <224 data> <XOR>`). The monitor tap
    // echoes our own TX back, so the parser must hold the whole 233-byte
    // echo WITHOUT the MAX_BUF resync draining it, then extract the ECU's
    // reply that follows back-to-back.
    //
    // The data tail deliberately contains a 0x12 (offset 200, inside the
    // last-96 bytes). With the old MAX_BUF=192 the 233-byte echo overflowed
    // the buffer, the resync drained the frame's `12 E9` header, and the
    // parser re-anchored on this in-band 0x12 — a phantom frame that never
    // completes, swallowing the real reply. That is the "no echo of service
    // 0x07" freeze the MPPS cal write hit on real hardware.
    let mut payload = vec![0x12, 0xE9, 0x07, 0x02, 0x03, 0x84, 0x60, 0xE0];
    for i in 0..224u16 {
        payload.push(if i == 200 { 0x12 } else { 0x55u8.wrapping_add(i as u8) });
    }
    let wire = build_frame(&payload);
    assert_eq!(wire.len(), 233, "MPPS cal-write frame is 233 bytes on the wire");

    let mut p = FrameParser::default();
    p.feed(&wire);
    let echo = p.take_frame().expect("echo of the 233-byte write must be extracted");
    assert_eq!(echo.payload, payload, "echo must round-trip intact");

    // The ECU's ack follows on the same line, back-to-back.
    let reply = build_frame(&[0x12, 0x04, 0xA0]);
    p.feed(&reply);
    let f = p.take_frame().expect("reply after the big echo");
    assert_eq!(f.payload, vec![0x12, 0x04, 0xA0]);
    assert!(p.take_frame().is_none(), "buffer drained");
}

// ---- Real-car (2026-11-09 M47) fixtures from `dde3 toolkit log.txt` ----

#[test]
fn real_car_live_reply_splits_echo() {
    // Merged wire bytes as captured on the K-line: the ECU reply (16 B incl
    // trailing XOR) immediately followed by our TX echo (15 B incl XOR). The
    // parser must split them back-to-back, reply first, echo second.
    let wire: Vec<u8> = vec![
        // reply: 12 10 A0 10 00 00 00 00 00 03 DE 0B 26 02 4A 0A
        0x12, 0x10, 0xA0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xDE, 0x0B, 0x26,
        0x02, 0x4A, 0x0A,
        // echo of our request: 12 0F 0B 10 0F 83 0F 10 0F 40 0F 00 0F 65 BF
        0x12, 0x0F, 0x0B, 0x10, 0x0F, 0x83, 0x0F, 0x10, 0x0F, 0x40, 0x0F, 0x00, 0x0F,
        0x65, 0xBF,
    ];
    let mut p = FrameParser::default();
    p.feed(&wire);
    let reply = p.take_frame().expect("reply frame");
    assert_eq!(
        reply.payload,
        vec![0x12, 0x10, 0xA0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xDE, 0x0B, 0x26, 0x02, 0x4A]
    );
    let echo = p.take_frame().expect("echo frame");
    assert_eq!(
        echo.payload,
        vec![0x12, 0x0F, 0x0B, 0x10, 0x0F, 0x83, 0x0F, 0x10, 0x0F, 0x40, 0x0F, 0x00, 0x0F, 0x65]
    );
    assert!(p.take_frame().is_none(), "buffer drained");
}

#[test]
fn real_car_live_values_decode() {
    // The five u16 values in the live reply, in request order
    // [0F83 0F10 0F40 0F00 0F65], decode to the engine-off / ~12 deg C set:
    // PWG=0, N=0, boost=990 hPa, WTF=2854 (->12.3 deg C), batt=586 (->11.9 V).
    let payload = vec![
        0x12, 0x10, 0xA0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xDE, 0x0B, 0x26, 0x02, 0x4A,
    ];
    assert_eq!(payload[0], 0x12, "prefix must be the ECU address");
    assert_eq!(payload[2], 0xA0, "status at [2]");
    let vals: Vec<u16> = payload[5..].as_chunks::<2>().0.iter().map(|p| u16::from_be_bytes(*p)).collect();
    assert_eq!(vals, vec![0, 0, 990, 2854, 586]);
}

#[test]
fn real_car_ident_reply_is_envelope_string() {
    // Wire (2026-11-10 capture, dde3-toolkit/captures/dde3newlog.txt):
    // the ECU wraps the ident string in the normal envelope,
    // `12 2E A0 37 37 38 36 …` — status A0, ASCII data from [3].
    // LEN byte 0x2E = 46 = 3 header + 42 data + trailing XOR (the
    // RX log line shows the 45-byte payload, XOR stripped by the parser).
    let s = b"77868875201124048010281001445G521000000000"; // 42 chars
    assert_eq!(s.len(), 42, "len byte 0x2E = 3 header + 42 data + XOR");
    let mut wire: Vec<u8> = vec![0x12, 0x04, 0x00, 0x16]; // echo
    wire.extend_from_slice(&[0x12, 0x2E, 0xA0]);
    wire.extend_from_slice(s);
    wire.push(xor_cs(&wire));
    let mut p = FrameParser::default();
    p.feed(&wire);
    let _echo = p.take_frame().expect("echo");
    let ident = p.take_frame().expect("ident reply");
    assert_eq!(&ident.payload[..3], &[0x12, 0x2E, 0xA0], "envelope form");
    // identify() contract: envelope stripped, bare string remains.
    let stripped = strip_reply_envelope(&ident.payload);
    assert_eq!(stripped, s);
    assert!(ident_valid(stripped));
    let info = parse_ident(stripped).expect("parses");
    assert_eq!(info.software, "7786887");
    assert_eq!(info.ecu_code, "5201");
    assert_eq!(info.date, "4801");
    assert_eq!(info.hw_number, "0281001445");
    assert_eq!(info.g_code, "G521");
}

#[test]
fn ident_valid_accepts_plain_string() {
    assert!(ident_valid(b"8875201"));
    assert!(ident_valid(b"DDE 3.0 fuer M47"));
    assert!(!ident_valid(&[]), "empty rejected");
    assert!(!ident_valid(&[0x12, 0x04, 0x00]), "envelope is not a string");
    // Envelope-prefixed reply must NOT validate — this is what used to
    // trigger the spurious "ident looks erased/corrupt" warning.
    let enveloped: Vec<u8> = vec![0x12, 0x2E, 0xA0, b'7', b'7', b'8', b'6'];
    assert!(!ident_valid(&enveloped));
    assert!(ident_valid(strip_reply_envelope(&enveloped)));
}

#[test]
fn parse_ident_fields() {
    let s = b"77868875201124048010281001445G521000000000";
    let i = parse_ident(s).expect("parses the captured string");
    assert_eq!(i.software, "7786887");
    assert_eq!(i.ecu_code, "5201");
    assert_eq!(i.date, "4801");
    assert_eq!(i.hw_number, "0281001445");
    assert_eq!(i.g_code, "G521");
    assert!(parse_ident(b"8875201").is_none(), "short string not parseable");
    assert!(parse_ident(&[0xFF; 40]).is_none(), "non-ASCII not parseable");
    assert_eq!(format_pair("5201"), "52/01");
    assert_eq!(format_pair("4801"), "48/01");
}

#[test]
fn is_not_live_broadcast_distinguishes() {
    // The 16-byte periodic block is skipped; small fault/battery replies kept.
    let broadcast = vec![
        0x12, 0x10, 0xA0, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xDE, 0x0B, 0x26, 0x02, 0x4A,
    ];
    let count_reply = vec![0x12, 0x05, 0xA0, 0x03]; // 3 faults
    let batt_reply = vec![0x12, 0x08, 0xA0, 0x10, 0x00, 0x02, 0x4A]; // 586 -> 11.9 V
    assert!(!is_not_live_broadcast(&broadcast));
    assert!(is_not_live_broadcast(&count_reply));
    assert!(is_not_live_broadcast(&batt_reply));
}

#[test]
fn fixed_frames_match_vm_extraction() {
    // UPROG_EIN: 12 04 0D (frames.md @0x308)
    assert_eq!(frame_uprog_ein(), vec![0x12, 0x04, 0x0D]);
    // IDENT: 12 04 00 (frames.md @0xf3d5)
    assert_eq!(frame_ident(), vec![0x12, 0x04, 0x00]);
    // DIAGNOSE_ENDE: 12 04 9F (frames.md @0x106dc)
    assert_eq!(frame_diagnose_ende(), vec![0x12, 0x04, 0x9F]);
    // SEED_KEY: 12 08 90 42 4D 57 05 <Z> (HANDOVER §15.3)
    assert_eq!(
        frame_seed_key(0x05),
        vec![0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, 0x05, 0x05]
    );
    // Fault clear: 12 09 07 0E 42 41 50 00 (frames.md @0xe55e)
    assert_eq!(
        frame_fault_clear(),
        vec![0x12, 0x09, 0x07, 0x0E, 0x42, 0x41, 0x50, 0x00]
    );
}

#[test]
fn frame_aif_read_matches_galletto() {
    // Galletto's AIF read from the full-flash-read session: `12 05 02 00`
    // + trailing XOR (xor(12 05 02 00) = 0x15 — the `… 15` of `12 05 02 00
    // 15` in the capture).
    assert_eq!(frame_aif_read(0x00), vec![0x12, 0x05, 0x02, 0x00]);
    assert_eq!(build_frame(&frame_aif_read(0x00)), vec![0x12, 0x05, 0x02, 0x00, 0x15]);
}

#[test]
fn key_from_seed_reply_matches_galletto_vectors() {
    // Seed replies are the 45-byte payloads `12 2E A0 <42 data bytes>` (+ the
    // trailing XOR the key formula runs on); the accepted key is the
    // unbiased job sum `K[i] = FR[(0x12+i)%46] + FR[(Z+i)%46] + FR[(0x29+i)%46]`.
    // Unbiased vectors only: Z=0x07 -> `39 3C 3F 42`, Z=0x10 -> `87 8A 8D 90`,
    // Z=0x16 -> `BB BE C1 C4` (the captured Galletto values for Z>=0x10 sat at
    // unbiased-5, i.e. behind the old bias; the job sum is unbiased).
    let mut reply = vec![0x12, 0x2E, 0xA0];
    for i in 0u8..42 {
        reply.push(((0x07u16 * 31 + i as u16 + 0x27) & 0xFF) as u8); // seed data for Z=0x07
    }
    assert_eq!(key_from_seed_reply(&reply, 0x07), [0x39, 0x3C, 0x3F, 0x42]);

    let mut reply10 = vec![0x12, 0x2E, 0xA0];
    for i in 0u8..42 {
        reply10.push(((0x10u16 * 31 + i as u16 + 0x27) & 0xFF) as u8); // seed data for Z=0x10
    }
    assert_eq!(key_from_seed_reply(&reply10, 0x10), [0x87, 0x8A, 0x8D, 0x90]);

    let mut reply16 = vec![0x12, 0x2E, 0xA0];
    for i in 0u8..42 {
        reply16.push(((0x16u16 * 31 + i as u16 + 0x27) & 0xFF) as u8); // seed data for Z=0x16
    }
    assert_eq!(key_from_seed_reply(&reply16, 0x16), [0xBB, 0xBE, 0xC1, 0xC4]);
}

#[test]
fn parameterized_frames_match_vm_extraction() { 
    // Verified against the WinKFP .prg VM (docs/FINDINGS_flash_and_fast_ds2.md):
    // FLASH_LESEN addr=0x38000 count=0x80 -> 12 09 06 00 03 80 00 80 (addr BE @4..6)
    assert_eq!(
        frame_flash_read(0x38000, 0x80),
        vec![0x12, 0x09, 0x06, 0x00, 0x03, 0x80, 0x00, 0x80]
    );
    // width probe: count is a single wire byte (high byte dropped)
    assert_eq!(
        frame_flash_read(0x12345, 0x34),
        vec![0x12, 0x09, 0x06, 0x00, 0x01, 0x23, 0x45, 0x34]
    );
    // FLASH_LOESCHEN addr=0x38000 -> 12 09 07 06 03 80 00 00 (no count)
    assert_eq!(
        frame_flash_erase(0x38000, 0x00),
        vec![0x12, 0x09, 0x07, 0x06, 0x03, 0x80, 0x00, 0x00]
    );
    // FLASH_SCHREIBEN 4 data bytes @0x38000 -> 12 0D 07 02 03 80 00 04 11 22 33 44
    assert_eq!(
        frame_flash_write(0x38000, &[0x11, 0x22, 0x33, 0x44]),
        vec![0x12, 0x0D, 0x07, 0x02, 0x03, 0x80, 0x00, 0x04, 0x11, 0x22, 0x33, 0x44]
    );
}

#[test]
fn flash_write_end_matches_mpps() {
    // MPPS calibration-write preamble: 07 0F @0x03FCE0 ->
    // 12 09 07 0F 03 FC E0 00 (XOR added on the wire).
    assert_eq!(
        frame_flash_write_end(0x03FCE0),
        vec![0x12, 0x09, 0x07, 0x0F, 0x03, 0xFC, 0xE0, 0x00]
    );
}

#[test]
fn cal_write_block_covers_32kb() {
    // MPPS block math: the 32 KB calibration is 146×224 + 1×64.
    let n_full = CAL_LEN / CAL_WRITE_BLOCK;
    let rem = CAL_LEN % CAL_WRITE_BLOCK;
    assert_eq!(n_full, 146, "146 full 224-byte blocks");
    assert_eq!(rem, 64, "64-byte trailing block");
    assert_eq!(n_full * CAL_WRITE_BLOCK + rem, CAL_LEN);
    // The frame builder accepts the 224-byte block (LEN = 0x09 + 0xE0 = 0xE9).
    let blk = vec![0xABu8; CAL_WRITE_BLOCK];
    let f = frame_flash_write(0x38000, &blk);
    assert_eq!(f[1], 0x09 + CAL_WRITE_BLOCK as u8, "LEN = 9 + 0xE0");
    assert_eq!(f[7], CAL_WRITE_BLOCK as u8, "data-length byte");
}

#[test]
fn reopen_divisor_maps_125000_to_124800() {
    assert_eq!(reopen_divisor(125000), 124_800);
    assert_eq!(reopen_divisor(38400), 38_400);
    assert_eq!(reopen_divisor(9600), 9_600);
}

#[test]
fn dtc_frames_match_vm_extraction() {
    // FS_QUICK_LESEN start: 12 05 14 01 (ds_frames.md @0x88b8)
    assert_eq!(frame_dtc_count(), vec![0x12, 0x05, 0x14, 0x01]);
    // FS_LESEN entry: 12 05 04 00 (ds_frames.md @0x9935)
    assert_eq!(frame_dtc_entry(), vec![0x12, 0x05, 0x04, 0x00]);
    // FS_SHADOW_LESEN: 12 05 04 01 (ds_frames.md @0x136b0)
    assert_eq!(frame_dtc_shadow(), vec![0x12, 0x05, 0x04, 0x01]);
    // FS_LOESCHEN: 12 04 05 (ds_frames.md @0x1354e)
    assert_eq!(frame_dtc_clear(), vec![0x12, 0x04, 0x05]);
}

#[test]
fn dtc_describe_uses_text_tables() {
    let d = Dtc {
        ort: 0x05,
        art: 0x01,
        uw: vec![100, 200],
        ..Default::default()
    };
    let s = d.describe();
    assert!(s.contains("Nadelbewegungsfuehler"), "ORT text: {s}");
    assert!(s.contains("Signal Kurzschluss nach B+"), "ART text: {s}");
}

#[test]
fn dtc_from_record_reference_m47() {
    // The M47 reference fault as printed in `actual error.txt` (the
    // reference tool's 50-byte "Errorcode" record).
    let rec: [u8; 50] = [
        0x08, 0xC8, 0x3C, 0x00, 0x38, 0x9C, 0xE3, 0x00, 0x37, 0x9D, 0xDE, 0xC4, 0xC1, 0x04,
        0x00, 0x48, 0x00, 0x99, 0x03, 0x00, 0x76, 0xDC, 0x5B, 0x03, 0x4A, 0x00, 0x76, 0xD1,
        0x7D, 0x00, 0x76, 0xDC, 0x5A, 0x00, 0x34, 0x32, 0x33, 0x32, 0x34, 0x00, 0x30, 0x21,
        0x00, 0x00, 0x00, 0x31, 0xC5, 0x00, 0x00, 0x22,
    ];
    let d = Dtc::from_record(&rec);
    assert_eq!(d.ort, 0x08, "ORT must be Gluehanlage, not Mengensteller");
    assert_eq!(crate::dtc::ort_text(d.ort), "Gluehanlage");
    assert_eq!(d.freq, 60);
    assert_eq!(d.art_mask, 0xC8);
    assert_eq!(d.arts, vec![0x32, 0x05, 0x07]);
    assert_eq!(d.record, rec.to_vec());
    let s = d.describe();
    assert!(s.contains("Gluehkerzen bzw. Gluehrelais defekt"), "{s}");
    assert!(s.contains("Fehler momentan vorhanden"), "{s}");
    assert!(s.contains("Fehler sporadisch"), "{s}");
    assert!(!s.contains("Mengensteller"), "{s}");
}

#[test]
fn jobstatus_lookup() {
    assert!(JobStatus::from_sb(0xA0, 0x0D).is_okay());
    assert_eq!(JobStatus::from_sb(0xA0, 0x0D).as_str(), "OKAY");
    assert!(!JobStatus::from_sb(0xA1, 0x0D).is_okay());
    assert_eq!(JobStatus::from_sb(0xFF, 0x0D).as_str(), "ERROR_SG_NACK");
    assert!(matches!(
        JobStatus::from_sb(0xAA, 0x0D),
        JobStatus::ErrorSgUnknown(_)
    ));
}

#[test]
fn seed_reply_survives_chunked_delivery() {
    // Regression: the 46-byte SEED reply `12 2E A0 00 01 02 … 29 9D`
    // arrived in chunks and the old shortest-XOR-prefix parser emitted
    // the 3-byte frame `12 13 14` from inside the still-arriving reply
    // (the linear seed table contains the in-band coincidence
    // 0x12^0x13^0x14 == 0x15), so the toolkit computed a key from
    // `12 13 14` and collided with the real reply on the wire.
    let req = build_frame(&[0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, 0x07]);
    let mut reply_payload = vec![0x12, 0x2E, 0xA0];
    for i in 0u8..=0x29 {
        reply_payload.push(i);
    }
    let mut reply_wire = reply_payload.clone();
    reply_wire.push(xor_cs(&reply_payload));
    assert_eq!(reply_wire.len(), 46, "LEN 0x2E = total wire length");

    // Byte-by-byte delivery (worst-case chunking): nothing may be
    // emitted before the full 46-byte frame has arrived.
    let mut p = FrameParser::default();
    p.feed(&req);
    let echo = p.take_frame().expect("echo");
    assert_eq!(echo.payload, vec![0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, 0x07]);
    for (i, b) in reply_wire.iter().enumerate() {
        p.feed(&[*b]);
        if i + 1 < reply_wire.len() {
            assert!(
                p.take_frame().is_none(),
                "no frame before all {} bytes (got one after {})",
                reply_wire.len(),
                i + 1
            );
        }
    }
    let f = p.take_frame().expect("full seed reply");
    assert_eq!(f.payload, reply_payload);
    assert!(p.take_frame().is_none());
}

#[test]
fn parser_drops_leading_garbage_before_frame() {
    let mut p = FrameParser::default();
    p.feed(&[0x00, 0xFF, 0x12, 0x04, 0x00, 0x16]);
    let f = p.take_frame().expect("frame after garbage");
    assert_eq!(f.payload, vec![0x12, 0x04, 0x00]);
}

#[test]
fn parser_resyncs_on_bad_xor() {
    // A frame whose declared LEN is present but whose trailing XOR does
    // not validate must never be emitted; the buffer resyncs byte by
    // byte and a valid frame behind the debris is still recovered.
    let mut p = FrameParser::default();
    p.feed(&[0x12, 0x04, 0x00, 0x99]); // bad XOR (would be 0x16)
    for _ in 0..8 {
        assert!(p.take_frame().is_none(), "corrupt frame must not be emitted");
    }
    p.feed(&build_frame(&[0x12, 0x04, 0x0D]));
    let mut got = None;
    for _ in 0..16 {
        if let Some(f) = p.take_frame() {
            got = Some(f);
            break;
        }
    }
    assert_eq!(got.unwrap().payload, vec![0x12, 0x04, 0x0D]);
}

#[test]
fn full_read_skips_unexposed_irom_prefix() {
    // The ECU does not expose its IROM: [0, FLASH_READ_START) always reads
    // back 0xFF, so read_full starts the wire read at FLASH_READ_START and
    // FF-pads the prefix — the returned image is still the full 256 KB in
    // the same byte layout.
    assert_eq!(FLASH_READ_START, 0x8000);
    assert!(FLASH_READ_START < FLASH_WRITE_START, "read start must precede the write span");
    assert_eq!(IMAGE_SIZE - FLASH_READ_START, 0x38000);
    // 1792 wire chunks instead of the old 2048 (256 chunks / 12.5 % saved).
    assert_eq!(
        (IMAGE_SIZE - FLASH_READ_START) / READ_CHUNK as usize,
        1792
    );
}
