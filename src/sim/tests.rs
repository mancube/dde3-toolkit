use super::*;

/// UPROG_EIN + seed/key — the real ECU's flash-session bootstrap
/// (2026-11-19 capture). The flash services (0x06/0x07) NACK with the
/// 0xA2 stub until this is done.
fn unlock(s: &mut SimEcu) {
    s.handle(&[0x12, 0x04, 0x0D]);
    let z: u8 = 0x07;
    let r = s.handle(&[0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, z]);
    let key = crate::protocol::key_from_seed_reply(&r, z);
    s.handle(&[0x12, 0x08, 0x90, key[0], key[1], key[2], key[3]]);
}

/// Real-car parity (2026-11-19 capture, `captures/dde3-20261119-222226.log`):
/// UPROG_EIN replies `12 4C A0` + the 72-byte region table; the seed
/// telegram is the ident string; Z = 0x07 → key `9C A0 97 96`; the
/// key-check reply is `12 05 A0 00`.
#[test]
fn uprog_ein_and_seed_key_real_car_parity() {
    let mut s = SimEcu::new();
    let r = s.handle(&[0x12, 0x04, 0x0D]);
    assert_eq!(&r[..3], &[0x12, 0x4C, 0xA0], "UPROG_EIN reply shape");
    assert_eq!(r.len(), 3 + UPROG_EIN_DATA.len());
    assert_eq!(&r[3..], &UPROG_EIN_DATA, "region table byte-for-byte");
    let r = s.handle(&[0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, 0x07]);
    assert_eq!(&r[3..], &IDENT_DATA, "seed telegram == ident string");
    let key = crate::protocol::key_from_seed_reply(&r, 0x07);
    assert_eq!(key, [0x9C, 0xA0, 0x97, 0x96], "real-car key for Z=0x07");
    let rk = s.handle(&[0x12, 0x08, 0x90, key[0], key[1], key[2], key[3]]);
    assert_eq!(&rk[..4], &[0x12, 0x05, 0xA0, 0x00], "key-check reply shape");
}

#[test]
fn ident_and_status() {
    let mut s = SimEcu::new();
    // IDENT replies with the ident string in the normal envelope
    // (matches the live ECU: `12 2E A0 37 37 38 36 …`).
    // Wire = 3 header + 42 string + trailing XOR = 46 = LEN byte.
    let r = s.handle(&[0x12, 0x04, 0x00]);
    assert_eq!(&r[..3], &[0x12, 0x2E, 0xA0]);
    assert_eq!(r.len(), 45, "42-char string, LEN = payload + XOR");
    let info = crate::protocol::parse_ident(&r[3..]).expect("sim ident parses");
    assert_eq!(info.software, "7786887");
    assert_eq!(info.hw_number, "0281001445");
    let r = s.handle(&[0x12, 0x04, 0x9F]);
    assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);
    // UPROG_EIN: the real car replies `12 4C A0` + the 72-byte region
    // table (2026-11-19 capture) — not a bare status.
    let r = s.handle(&[0x12, 0x04, 0x0D]);
    assert_eq!(&r[..3], &[0x12, 0x4C, 0xA0]);
    assert_eq!(&r[3..], &UPROG_EIN_DATA);
    // unknown service → NACK
    let r = s.handle(&[0x12, 0x04, 0x55]);
    assert_eq!(&r[..3], &[0x12, 0x04, 0xFF]);
}

#[test]
fn seed_key_round_trip() {
    let mut s = SimEcu::new();
    // Seed request: `12 08 90 42 4D 57 Z`. Reply is the 45-byte payload
    // `12 2E A0 <42>` (LEN 0x2E = 46 wire incl. XOR).
    let z: u8 = 0x10;
    let seed_req: Vec<u8> = vec![0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, z];
    let r = s.handle(&seed_req);
    assert_eq!(r[1], 0x2E, "seed reply LEN = 46 wire");
    assert_eq!(r.len(), 45, "payload without trailing XOR");
    assert_eq!(r[2], 0xA0);

    // Compute expected key on the full 46-byte wire frame (payload + XOR)
    // with modulo-46 wrapping: K[i] = FR[(0x12+i)%46] + FR[(Z+i)%46] + FR[(0x29+i)%46]
    let xor = crate::protocol::xor_cs(&r);
    let mut frame = r.clone();
    frame.push(xor);
    assert_eq!(frame.len(), 46, "46-byte wire frame");
    const FRAME_SIZE: usize = 46;

    let mut key = [0u8; 4];
    for i in 0..4usize {
        let a = frame[(0x12 + i) % FRAME_SIZE];
        let b = frame[(z as usize + i) % FRAME_SIZE];
        let c = frame[(0x29 + i) % FRAME_SIZE];
        key[i] = a.wrapping_add(b).wrapping_add(c);
    }

    // Wrong key first: must be NACKed (3-byte status, byte at index 2)
    // and leave the ECU locked.
    let bad: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
    let wrong_req: Vec<u8> = [0x12, 0x08, 0x90, bad[0], bad[1], bad[2], bad[3]]
        .to_vec();
    let rw = s.handle(&wrong_req);
    assert_eq!(rw[2], 0xB0, "wrong key -> 0xB0");

    // Correct key unlocks.
    let key_req: Vec<u8> = [0x12, 0x08, 0x90, key[0], key[1], key[2], key[3]].to_vec();
    let rk = s.handle(&key_req);
    assert_eq!(rk[2], 0xA0, "correct key unlocks");

    // A further seed request (the tool retries the whole exchange)
    // answers a FRESH 46-byte seed telegram again — NOT a short ack — with
    // a fresh expected key, so a retried key send still validates.
    let r2 = s.handle(&seed_req);
    assert_eq!(r2[1], 0x2E, "retry seed request -> fresh 46-byte telegram");
    assert_eq!(r2.len(), 45);
    assert_eq!(r2[2], 0xA0);
    let xor2 = crate::protocol::xor_cs(&r2);
    let mut frame2 = r2.clone();
    frame2.push(xor2);
    let mut key2 = [0u8; 4];
    for i in 0..4usize {
        key2[i] = frame2[(0x12 + i) % FRAME_SIZE]
            .wrapping_add(frame2[(z as usize + i) % FRAME_SIZE])
            .wrapping_add(frame2[(0x29 + i) % FRAME_SIZE])
    }
    let key2_req: Vec<u8> = [0x12, 0x08, 0x90, key2[0], key2[1], key2[2], key2[3]].to_vec();
    let rk2 = s.handle(&key2_req);
    assert_eq!(rk2[2], 0xA0, "retried key unlocks");
}

/// Galletto's full write is **two sessions** (`testwrite.txt`): program
/// area 0x10000..0x38000, `DIAGNOSE_ENDE`, re-auth from scratch, cal
/// area 0x38000..0x40000. Verify the fake ECU enforces the re-auth:
/// after `DIAGNOSE_ENDE` a data write is NACKed until UPROG_EIN +
/// seed/key are repeated.
#[test]
fn two_phase_write_reauth_like_galletto() {
    let mut s = SimEcu::new();

    // Seed/key exchange; returns the key-check status byte.
    let seed_key = |s: &mut SimEcu| -> u8 {
        let z: u8 = 0x10;
        let r = s.handle(&[0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, z]);
        let mut frame = r.clone();
        frame.push(crate::protocol::xor_cs(&r));
        let mut key = [0u8; 4];
        for i in 0..4usize {
            key[i] = frame[(0x12 + i) % 46]
                .wrapping_add(frame[(z as usize + i) % 46])
                .wrapping_add(frame[(0x29 + i) % 46]);
        }
        s.handle(&[0x12, 0x08, 0x90, key[0], key[1], key[2], key[3]])[2]
    };

    // Phase 1: UPROG_EIN + seed/key, erase @0x10000, write 64 B.
    assert_eq!(s.handle(&[0x12, 0x04, 0x0D])[2], 0xA0, "phase-1 UPROG_EIN");
    assert_eq!(seed_key(&mut s), 0xA0, "phase-1 key");
    assert_eq!(
        s.handle(&[0x12, 0x09, 0x07, 0x06, 0x01, 0x00, 0x00, 0x00])[2],
        0xA0,
        "erase @0x10000"
    );
    let mut w1 = vec![0x12, 0x49, 0x07, 0x02, 0x01, 0x00, 0x00, 0x40];
    w1.extend(std::iter::repeat_n(0xAA, 64));
    assert_eq!(s.handle(&w1)[2], 0xA0, "phase-1 write");

    // DIAGNOSE_ENDE closes the session.
    assert_eq!(s.handle(&[0x12, 0x04, 0x9F])[2], 0xA0, "DIAGNOSE_ENDE");

    // Without re-auth the data write gets the flash NACK stub (ECU left
    // prog mode; the real car answers `12 04 A2` outside the unlocked
    // session, 2026-11-19 capture).
    assert_eq!(s.handle(&w1)[2], 0xA2, "write after DIAGNOSE_ENDE -> NACK");

    // Phase 2: full re-auth, erase @0x38000, write 64 B.
    assert_eq!(s.handle(&[0x12, 0x04, 0x0D])[2], 0xA0, "phase-2 UPROG_EIN");
    assert_eq!(seed_key(&mut s), 0xA0, "phase-2 key");
    assert_eq!(
        s.handle(&[0x12, 0x09, 0x07, 0x06, 0x03, 0x80, 0x00, 0x00])[2],
        0xA0,
        "erase @0x38000"
    );
    let mut w2 = vec![0x12, 0x49, 0x07, 0x02, 0x03, 0x80, 0x00, 0x40];
    w2.extend(std::iter::repeat_n(0xBB, 64));
    assert_eq!(s.handle(&w2)[2], 0xA0, "phase-2 write");

    // Both areas landed in the reconstructed image.
    let img = s.flash_image();
    assert!(img[0x10000..0x10040].iter().all(|&b| b == 0xAA));
    assert!(img[0x38000..0x38040].iter().all(|&b| b == 0xBB));
}

/// MPPS-style calibration-write preamble, mirroring
/// `Kline::write_calibration`: UPROG_EIN → DTC read/clear → unlock →
/// `07 0F` write-end @0x03FCE0 → erase → 224-byte block → 64-byte trailing
/// block. Verifies the fake ECU accepts every frame and the data lands.
#[test]
fn mpps_style_cal_write_preamble() {
    let mut s = SimEcu::new();
    // UPROG_EIN, then the DTC read/clear (before the seed/key, as MPPS
    // sends it), then unlock.
    s.handle(&[0x12, 0x04, 0x0D]);
    assert_eq!(s.handle(&[0x12, 0x05, 0x04, 0x00])[2], 0xA0, "DTC FS_LESEN entry");
    assert_eq!(s.handle(&[0x12, 0x04, 0x05])[2], 0xA0, "DTC FS_LOESCHEN");
    let z: u8 = 0x07;
    let r = s.handle(&[0x12, 0x08, 0x90, 0x42, 0x4D, 0x57, z]);
    let key = crate::protocol::key_from_seed_reply(&r, z);
    assert_eq!(
        s.handle(&[0x12, 0x08, 0x90, key[0], key[1], key[2], key[3]])[2],
        0xA0,
        "key"
    );
    // MPPS write-end (07 0F) @0x03FCE0, before the erase.
    assert_eq!(
        s.handle(&[0x12, 0x09, 0x07, 0x0F, 0x03, 0xFC, 0xE0, 0x00])[2],
        0xA0,
        "07 0F write-end"
    );
    // Erase the calibration area.
    assert_eq!(
        s.handle(&[0x12, 0x09, 0x07, 0x06, 0x03, 0x80, 0x00, 0x00])[2],
        0xA0,
        "erase @0x038000"
    );
    // First 224-byte block @0x038000 (LEN = 0x09 + 0xE0 = 0xE9).
    let mut w = vec![0x12, 0xE9, 0x07, 0x02, 0x03, 0x80, 0x00, 0xE0];
    w.extend(std::iter::repeat_n(0xAA, 224));
    assert_eq!(s.handle(&w)[2], 0xA0, "224-byte block");
    // Trailing 64-byte block @0x03FFC0 (the 147th block; 146×224 = 0x7FC0,
    // so 0x38000 + 0x7FC0 = 0x3FFC0, and 0x3FFC0 + 64 = 0x40000).
    let mut t = vec![0x12, 0x49, 0x07, 0x02, 0x03, 0xFF, 0xC0, 0x40];
    t.extend(std::iter::repeat_n(0xBB, 64));
    assert_eq!(s.handle(&t)[2], 0xA0, "64-byte trailing block");
    // Both blocks landed in the reconstructed image.
    let img = s.flash_image();
    assert!(img[0x38000..0x38000 + 224].iter().all(|&b| b == 0xAA));
    assert!(img[0x3FFC0..0x3FFC0 + 64].iter().all(|&b| b == 0xBB));
}

#[test]
fn aif_read_46_bytes_from_cal_7cb2() {
    let mut s = SimEcu::new();
    // The exact request the flashing tool sent: `12 05 02 00` (+XOR on
    // the wire). The param byte is unused by the ECU.
    let r = s.handle(&[0x12, 0x05, 0x02, 0x00]);
    // `12 32 A0 <46 B>`: LEN 0x32 = 50 = 3 header + 46 record + XOR.
    assert_eq!(&r[..3], &[0x12, 0x32, 0xA0]);
    assert_eq!(r.len(), 3 + AIF_LEN);
    // The in-car record, byte-for-byte (tunerpro 16-11-2026.bin @ 0x7CB2).
    assert_eq!(&r[3..], &AIF_RECORD);
    // The job's "AIF_NICHT_PROGRAMMIERT" check is resp[3] == 0xFF.
    assert_eq!(r[3], 0x20);
    // The DDE3 job's param-0xFF variant gives the same record.
    let r2 = s.handle(&[0x12, 0x05, 0x02, 0xFF]);
    assert_eq!(&r[1..], &r2[1..]);
    // Blank the slot in the unlocked programming session → empty-slot
    // short reply. Write to flash address 0x3FCB2 (= cal 0x7CB2, the
    // AIF slot).
    unlock(&mut s);
    let mut w = vec![0x12, 0x0B, 0x07, 0x02, 0x03, 0xFC, 0xB2, AIF_LEN as u8];
    w.extend(std::iter::repeat_n(0xFF, AIF_LEN));
    assert_eq!(s.handle(&w)[2], 0xA0);
    let r5 = s.handle(&[0x12, 0x05, 0x02, 0x00]);
    assert_eq!(r5.len(), 4, "empty slot → 1 data byte");
    assert_eq!(r5[3], 0xFF, "the job's NICHT_PROGRAMMIERT byte");
}

#[test]
fn flash_read_round_trip() {
    let mut s = SimEcu::new();
    // Outside the unlocked session both read and write get the 0xA2 NACK
    // stub (real car, 2026-11-19 capture).
    let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 0x80]);
    assert_eq!(&r[..3], &[0x12, 0x04, 0xA2], "read pre-unlock -> NACK stub");
    let r = s.handle(&[0x12, 0x0B, 0x07, 0x02, 0x00, 0x00, 0x00, 0x02, 0xAA, 0xBB]);
    assert_eq!(r[2], 0xA2, "write pre-unlock -> NACK stub");
    // Unlock, then read 0x80 bytes @ addr 0: 12 09 06 00 <A_hi A_mid
    // A_lo> <count@7>; LEN = payload + trailing XOR = 3 + 0x80 + 1 = 0x84.
    unlock(&mut s);
    let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 0x80]);
    assert_eq!(&r[..3], &[0x12, 0x84, 0xA0]);
    assert_eq!(r.len(), 3 + 0x80);
    // write, read back
    let r = s.handle(&[0x12, 0x0B, 0x07, 0x02, 0x00, 0x00, 0x00, 0x02, 0xAA, 0xBB]);
    assert_eq!(r[2], 0xA0);
    let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 0x02]);
    assert_eq!(&r[3..5], &[0xAA, 0xBB]);
}

#[test]
fn flash_read_serves_file_image() {
    // A real dump (e.g. captures/actual_flash.bin) is served verbatim:
    // FLASH_LESEN returns those bytes and the AIF slot comes from the
    // file, not from the built-in AIF_RECORD.
    let path =
        std::env::temp_dir().join(format!("dde3_sim_file_img_{}.bin", std::process::id()));
    let mut img = vec![0u8; IMAGE_SIZE];
    for (i, b) in img.iter_mut().enumerate() {
        *b = (i as u8) ^ 0xA5;
    }
    img[AIF_SLOT..AIF_SLOT + AIF_LEN].fill(0xC3);
    std::fs::write(&path, &img).unwrap();
    let mut s = SimEcu::from_flash_file(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    unlock(&mut s);

    // The tool's actual FLASH_LESEN frame: 0x80 bytes @ 0.
    let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 0x80]);
    assert_eq!(&r[..3], &[0x12, 0x84, 0xA0]);
    assert_eq!(&r[3..3 + 0x80], &img[0x0000..0x0080]);
    // A mid-flash offset (the tool steps 0x80 per request).
    let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x03, 0x40, 0x00, 0x80]);
    assert_eq!(&r[3..3 + 0x80], &img[0x34000..0x34080]);
    // The AIF record now comes from the file — via the programming-record
    // service 0x02 …
    let r = s.handle(&[0x12, 0x05, 0x02, 0x00]);
    assert_eq!(&r[3..], &img[AIF_SLOT..AIF_SLOT + AIF_LEN]);
    assert_eq!(r[3], 0xC3);
    // … and via the FLASH_LESEN-based hardware-ID read (`addr=0, count=46`)
    // that foreign flashing tools issue — the same slot content.
    let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 46]);
    assert_eq!(&r[..3], &[0x12, 0x32, 0xA0]);
    assert_eq!(r.len(), 3 + AIF_LEN);
    assert_eq!(&r[3..], &img[AIF_SLOT..AIF_SLOT + AIF_LEN]);
    assert_eq!(r[3], 0xC3);
}

#[test]
fn flash_image_wrong_size_rejected() {
    let path = std::env::temp_dir().join(format!("dde3_sim_short_{}.bin", std::process::id()));
    std::fs::write(&path, vec![0u8; 4096]).unwrap();
    assert!(SimEcu::from_flash_file(&path).is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn erase_and_faults() {
    let mut s = SimEcu::new();
    unlock(&mut s);
    let r = s.handle(&[0x12, 0x09, 0x07, 0x06, 0x00, 0x00, 0x00, 0x00]);
    assert_eq!(r[2], 0xA0);
    let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 0x01]);
    assert_eq!(r[3], 0xFF);
    // fault clear is a plain OK
    let r = s.handle(&[0x12, 0x09, 0x07, 0x0E, 0x42, 0x41, 0x50, 0x00]);
    assert_eq!(r[2], 0xA0);
}

#[test]
fn mw_live() {
    let mut s = SimEcu::new();
    let r = s.handle(&[0x12, 0x05, 0x0B, 0x10, 0x00, 0x01]);
    assert_eq!(r[0], 0x12);
    assert_eq!(r[2], 0xA0);
    let v = u16::from_be_bytes([r[5], r[6]]);
    assert_eq!(v, SimEcu::mw_value(1));
    // battery voltage: plausible ~12.6 V at the DDE3 scale
    let r = s.handle(&[0x12, 0x05, 0x0B, 0x10, 0x0F, 0x65]);
    let v = u16::from_be_bytes([r[5], r[6]]);
    let volt = v as f64 * 0.02037243;
    assert!((11.0..14.0).contains(&volt), "battery {volt} V");
}

#[test]
fn mw_live_batch() {
    // Batched request: several ids in one telegram (the tested logger form).
    let mut s = SimEcu::new();
    let r = s.handle(&[0x12, 0x0D, 0x0B, 0x10, 0x0F, 0x63, 0x0F, 0x65, 0x0F, 0x10]);
    assert_eq!(r[0], 0x12);
    assert_eq!(r[2], 0xA0);
    assert_eq!(&r[3..5], &[0x10, 0x00]);
    // Three values, in request order, from offset 5.
    assert_eq!(r.len(), 5 + 3 * 2);
    let v0 = u16::from_be_bytes([r[5], r[6]]);
    let v1 = u16::from_be_bytes([r[7], r[8]]);
    let v2 = u16::from_be_bytes([r[9], r[10]]);
    assert_eq!(v0, SimEcu::mw_value(0x0F63));
    assert_eq!(v1, SimEcu::mw_value(0x0F65));
    assert_eq!(v2, SimEcu::mw_value(0x0F10));
}

#[test]
fn fault_memory_sequence() {
    let mut s = SimEcu::new();
    // count: 1 seeded fault (M47 reference: Gluehanlage) + header
    // (rec 0..22). LEN = payload + trailing XOR: 26 + 1 = 0x1B (real-car DTC reply shape).
    let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
    assert_eq!(&r[..4], &[0x12, 0x1B, 0xA0, 1]);
    assert_eq!(r.len(), 3 + 1 + 22);
    assert_eq!(&r[4..6], &[0x08, 0xC8]); // ORT + ART mask of fault 1 (Gluehanlage)
    // entry 1: `A0 <pos> <rec 22..26>` — LEN 8 + 1 = 9
    let r = s.handle(&[0x12, 0x05, 0x04, 0x00]);
    assert_eq!(&r[..4], &[0x12, 0x09, 0xA0, 1]);
    assert_eq!(r.len(), 3 + 1 + 4);
    // shadow 1: `A0 <pos> <rec 26..37>` — LEN 15 + 1 = 0x10
    let r = s.handle(&[0x12, 0x05, 0x04, 0x01]);
    assert_eq!(&r[..4], &[0x12, 0x10, 0xA0, 1]);
    assert_eq!(r.len(), 3 + 1 + 11);
    // tail 1 (last of a single-fault memory): `A0 <anz=0> <rec 37..50>` — LEN 17 + 1 = 0x12
    let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
    assert_eq!(&r[..4], &[0x12, 0x12, 0xA0, 0]);
    assert_eq!(r.len(), 3 + 1 + 13);
    // DS2 clear — LEN 3 + 1 = 4
    let r = s.handle(&[0x12, 0x04, 0x05]);
    assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);
    // empty count: `A0 <anz=0>` — LEN 4 + 1 = 5
    let r = s.handle(&[0x12, 0x05, 0x14, 0x01]);
    assert_eq!(&r[..4], &[0x12, 0x05, 0xA0, 0]);
}

#[test]
fn fault_replay_assembles_full_records() {
    // Walk the FS_LESEN sequence exactly like `Kline::read_dtcs` does
    // (same chunk slices) and verify each 50-byte record round-trips.
    let mut s = SimEcu::new();
    let c = s.handle(&[0x12, 0x05, 0x14, 0x01]);
    let data = &c[3..];
    let count = data[0] as usize;
    let mut header = data[1..23].to_vec();
    let mut left = count;
    let mut assembled = Vec::new();
    while left > 0 {
        let mut rec = header.clone();
        let e = s.handle(&[0x12, 0x05, 0x04, 0x00]);
        rec.extend_from_slice(e.get(4..8).unwrap_or(&[]));
        let sh = s.handle(&[0x12, 0x05, 0x04, 0x01]);
        rec.extend_from_slice(sh.get(4..15).unwrap_or(&[]));
        let t = s.handle(&[0x12, 0x05, 0x14, 0x01]);
        let d2 = &t[3..];
        rec.extend_from_slice(d2.get(1..14).unwrap_or(&[]));
        let next = d2.first().copied().unwrap_or(0) as usize;
        assembled.push(rec);
        left -= 1;
        if next == 0 || left == 0 {
            break;
        }
        if d2.len() >= 14 + 22 {
            header = d2[14..14 + 22].to_vec();
        } else {
            break;
        }
    }
    let exp = [
        (0x08u8, 0xC8u8, 0x0038u16, 0x9CE3u16),
    ];
    assert_eq!(assembled.len(), 1);
    for (i, rec) in assembled.iter().enumerate() {
        assert_eq!(rec.len(), 50);
        assert_eq!(rec[0], exp[i].0, "ORT {i}");
        assert_eq!(rec[1], exp[i].1, "ART mask {i}");
        assert_eq!(&rec[3..5], &exp[i].2.to_be_bytes(), "uw1 {i}");
        assert_eq!(&rec[5..7], &exp[i].3.to_be_bytes(), "uw2 {i}");
        let d = crate::protocol::Dtc::from_record(rec);
        assert_eq!(d.ort, exp[i].0);
        assert_eq!(d.uw[0], exp[i].2);
        assert_eq!(d.uw[1], exp[i].3);
    }
}

#[test]
fn missing_ds2_responses_are_answered() {
    // Every DS2 service the foreign tools use must get a well-formed
    // reply (JOBRESULT status + LEN = payload + trailing XOR) instead of
    // falling through to the `12 04 FF` NACK — a NACK mid-session made
    // the flashing tool stall waiting for a response it will never get.
    let mut s = SimEcu::new();

    // DS2 session keep (9E) / end (9F): status only.
    assert_eq!(s.handle(&[0x12, 0x04, 0x9E])[2], 0xA0);
    assert_eq!(s.handle(&[0x12, 0x04, 0x9F])[2], 0xA0);

    // DS2 test stamp read (0E): status + 3 bytes, LEN = payload + XOR = 7.
    let r = s.handle(&[0x12, 0x04, 0x0E]);
    assert_eq!(&r[..3], &[0x12, 0x07, 0xA0]);
    assert_eq!(r.len(), 6);
    assert_eq!(&r[3..6], &[0, 0, 0]);

    // DS2 test stamp write (0F): status only.
    let r = s.handle(&[0x12, 0x04, 0x0F]);
    assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);

    // DS2 actuator override (0C): status only.
    let r = s.handle(&[0x12, 0x08, 0x0C, 0x0F, 0x65, 0x05, 0x00]);
    assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);

    // DS2 EWS sync start (6C) / status (6D): status only.
    assert_eq!(s.handle(&[0x12, 0x05, 0x6C, 0x00])[2], 0xA0);
    assert_eq!(s.handle(&[0x12, 0x04, 0x6D])[2], 0xA0);

    // DS2 adjustment value read (40): status + u16 BE value, LEN = 6.
    let r = s.handle(&[0x12, 0x05, 0x40, 0x07]);
    assert_eq!(&r[..3], &[0x12, 0x06, 0xA0]);
    assert_eq!(r.len(), 5);
    assert_eq!(&r[3..5], &[0x00, 0x00]);

    // DS2 adjustment value set (41): status only; the read returns the
    // stored value.
    assert_eq!(s.handle(&[0x12, 0x07, 0x41, 0x07, 0x00, 0x2A])[2], 0xA0);
    let r = s.handle(&[0x12, 0x05, 0x40, 0x07]);
    assert_eq!(&r[3..5], &[0x00, 0x2A]);
    // DS2 adjustment value store → EEPROM (42): status only.
    assert_eq!(s.handle(&[0x12, 0x05, 0x42, 0x07])[2], 0xA0);
}

#[test]
fn mw_subservices_answered() {
    let mut s = SimEcu::new();

    // MW measurement block (0x03): LEN = payload(54) + XOR = 55 = 0x37,
    // header byte at [3] + 25 u16 BE from [4..54].
    let r = s.handle(&[0x12, 0x05, 0x0B, 0x03]);
    assert_eq!(&r[..3], &[0x12, 0x37, 0xA0]);
    assert_eq!(r.len(), 54);
    assert_eq!(r[3], 0x00);
    assert_eq!(r.len(), 3 + 1 + 25 * 2);

    // Digital input block (0x04): LEN = payload(8) + XOR = 9 = 0x09,
    // header byte at [3] + 4 data bytes at [4..8].
    let r = s.handle(&[0x12, 0x05, 0x0B, 0x04]);
    assert_eq!(&r[..3], &[0x12, 0x09, 0xA0]);
    assert_eq!(r.len(), 8);
    assert_eq!(r.len(), 3 + 1 + 4);

    // PSG value read (0x11): LEN = payload(7) + XOR = 8 = 0x08, 2-byte
    // header `11 00` at [3..5] + u16 BE value.
    let r = s.handle(&[0x12, 0x05, 0x0B, 0x11]);
    assert_eq!(&r[..3], &[0x12, 0x08, 0xA0]);
    assert_eq!(&r[3..5], &[0x11, 0x00]);

    // PSG value select (0x12): status only.
    let r = s.handle(&[0x12, 0x05, 0x0B, 0x12, 0x00]);
    assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);

    // STEUERN_TESTPLATZ: status only when unlocked (fresh sim has no
    // pending seed → unlocked). Frame: `12 0B 07 10 …` (addr 12, len 0B,
    // sid 07 write-type, sub 10).
    let r = s.handle(&[0x12, 0x0B, 0x07, 0x10, 0x00, 0x00, 0x00, 0x02, 0x4B, 0x9A]);
    assert_eq!(&r[..3], &[0x12, 0x04, 0xA0]);
}

#[test]
fn aif_override_serves_test_content() {
    // The AIF (hardware-ID) content served on `FLASH_LESEN addr=0 count=46`
    // is overridable (test hook) so a foreign tool's expectations can be
    // exercised without touching the real-car AIF bytes.
    let mut s = SimEcu::new();
    let aif = {
        let mut a = [0u8; 46];
        for (i, b) in a.iter_mut().enumerate() {
            *b = (i as u8).wrapping_add(0x61);
        }
        a
    };
    s.set_aif(aif);
    unlock(&mut s);
    let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 46]);
    assert_eq!(&r[..3], &[0x12, 0x32, 0xA0]);
    assert_eq!(r.len(), 3 + AIF_LEN);
    assert_eq!(&r[3..], &aif[..]);
}

#[test]
fn flash_read_aif_serves_slot_content() {
    // The AIF (hardware-ID) content served on `FLASH_LESEN addr=0
    // count=46` comes from the flash image's AIF slot — the same slot the
    // service-0x02 handler serves — so a `respond --flash <file>` run
    // serves the real car's AIF record (hardware number / ECU type / VIN
    // data) again, instead of a hard-coded string. The `set_aif` override
    // still wins when a foreign tool needs content-matched replies.
    let mut s = SimEcu::new();
    unlock(&mut s);
    let r = s.handle(&[0x12, 0x09, 0x06, 0x00, 0x00, 0x00, 0x00, 46]);
    // `12 32 A0 <46 B> <XOR>`: LEN = 3 + 46 + XOR = 50 = 0x32.
    assert_eq!(&r[..3], &[0x12, 0x32, 0xA0]);
    assert_eq!(r.len(), 3 + AIF_LEN);
    assert_eq!(&r[3..], &AIF_RECORD[..]);
    assert_eq!(r[3], AIF_RECORD[0]);
}

#[test]
fn baud_patch_91_switches_link_and_acks() {
    // respond --patched-baud: the fake ECU behaves like the patched real
    // ECU — 0x91 is accepted, the link switches to the patch's fixed
    // rate, and the ACK goes out at the NEW rate (the transport applies
    // the switch before transmitting the reply — take_baud_switch).
    let mut s = SimEcu::new();
    s.set_baud_patch(124_800); // the 125000 patch's actual S0BG divisor
    let r = s.handle(&[0x12, 0x08, 0x91, 0x00, 0x01, 0xE2, 0x40, 0x03]);
    assert_eq!(&r[..3], &[0x12, 0x04, 0xA0], "0x91 ACKed");
    assert_eq!(s.take_baud_switch(), Some(124_800), "switch pending");
    assert_eq!(s.take_baud_switch(), None, "switch consumed once");
    // The link is now at the new rate: normal services keep working.
    let r = s.handle(&[0x12, 0x04, 0x00]);
    assert_eq!(r[2], 0xA0, "IDENT at the new rate");
    // A second 0x91 re-switches (idempotent on the real ECU too).
    let r = s.handle(&[0x12, 0x08, 0x91, 0x00, 0x01, 0xE2, 0x40, 0x03]);
    assert_eq!(r[2], 0xA0, "second 0x91 ACKed");
    assert_eq!(s.take_baud_switch(), Some(124_800), "second switch");
}

#[test]
fn baud_91_without_patch_keeps_stub_ack() {
    // No --patched-baud: the legacy stub ACK stays and no link switch is
    // reported.
    let mut s = SimEcu::new();
    let r = s.handle(&[0x12, 0x08, 0x91, 0x00, 0x00, 0x09, 0x60, 0x03]);
    assert_eq!(&r[..3], &[0x12, 0x04, 0xA0], "stub ACK");
    assert_eq!(s.take_baud_switch(), None, "no switch without the patch");
}
