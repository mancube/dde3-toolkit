use super::*;

fn manifest() -> String {
    r#"{
        "patch": "test_patch",
        "version": 1,
        "description": "unit test",
        "input": {"sha256": "00"},
        "changes": [
            {"what": "one", "file_offset": "0x100", "length": 2,
             "before": "0000", "after": "aabb"}
        ],
        "checksum": "auto"
    }"#
    .into()
}

fn patches_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("patches")
}

#[test]
fn apply_and_revert_round_trip() {
    let p = Patch::parse(&manifest(), PatchSource::Builtin).unwrap();
    let mut img = vec![0u8; IMAGE_SIZE];
    assert_eq!(p.state(&img), PatchState::Available);
    p.apply(&mut img).unwrap();
    assert_eq!(p.state(&img), PatchState::Applied);
    assert_eq!(&img[0x100..0x102], &[0xAA, 0xBB]);
    p.revert(&mut img).unwrap();
    assert_eq!(p.state(&img), PatchState::Available);
    assert_eq!(&img[0x100..0x102], &[0x00, 0x00]);
}

#[test]
fn conflict_is_refused() {
    let p = Patch::parse(&manifest(), PatchSource::Builtin).unwrap();
    let mut img = vec![0u8; IMAGE_SIZE];
    img[0x100] = 0x11;
    assert!(matches!(p.state(&img), PatchState::Conflict(_)));
    assert!(p.apply(&mut img).is_err());
}

#[test]
fn partial_calibration_image_is_invalid() {
    let p = Patch::parse(&manifest(), PatchSource::Builtin).unwrap();
    let img = vec![0u8; 0xC000];
    assert!(matches!(p.state(&img), PatchState::Invalid(_)));
}

#[test]
fn builtin_manifests_parse() {
    let (patches, warnings) = builtins();
    assert!(
        warnings.is_empty(),
        "built-in manifests must parse: {warnings:?}"
    );
    let mut names: Vec<&str> = patches.iter().map(|p| p.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "edc15m_baud_0x91_125000",
            "edc15m_baud_0x91_38400",
            "edc15m_immo_off_full",
            "edc15m_immo_off_gate",
        ]
    );
    let outputs: Vec<&str> = patches
        .iter()
        .filter_map(|p| p.output.as_deref())
        .collect();
    assert_eq!(outputs.len(), 4, "every manifest names its output image");
}

/// The manifest generator tags derived checksum fields, and only those:
/// the fit check must see the real edits.
#[test]
fn checksum_fields_are_tagged_and_excluded_from_the_fit() {
    let (patches, _) = load_all();
    for p in &patches {
        for c in &p.changes {
            let derived = matches!(
                c.offset,
                crate::checksum::CODE_CHECKSUM_POS
                    | crate::checksum::CAL_CHECKSUM_POS
                    | crate::checksum::PROG_CHAIN_POS
            );
            assert_eq!(
                c.is_checksum(),
                derived,
                "{}: 0x{:05X} classified wrongly",
                p.name,
                c.offset
            );
        }
        assert!(
            p.semantic_changes().len() < p.changes.len(),
            "{}: checksum fields must be excluded",
            p.name
        );
    }
}

/// A third-party "already patched" read: every semantic edit applied, but
/// the program-chain word still holds the ORIGINAL value (a bypassed
/// checksum — the shape of the user's `e46_immo_off.bin`). It must read as
/// `applied`, and applying must repair the chain instead of refusing.
#[test]
fn already_patched_with_stale_chain_is_repaired_not_refused() {
    let dir = patches_dir();
    let orig = std::fs::read(dir.join("e46_orig.bin")).expect("reference image");
    let p = load_all()
        .0
        .into_iter()
        .find(|p| p.name == "edc15m_immo_off_full")
        .expect("immo full manifest");
    let mut img = orig.clone();
    p.apply(&mut img).unwrap();
    assert_eq!(p.state(&img), PatchState::Applied);

    let chain = p
        .changes
        .iter()
        .find(|c| c.is_checksum())
        .expect("manifest carries the chain field");
    img[chain.offset..chain.offset + 4].copy_from_slice(&chain.before);
    assert_eq!(
        p.state(&img),
        PatchState::Applied,
        "a stale checksum must not change whether the patch fits"
    );
    assert!(
        !crate::checksum::verify(&img).iter().all(|c| c.ok),
        "the stale chain really is invalid"
    );

    p.apply(&mut img).unwrap();
    assert_eq!(
        &img[chain.offset..chain.offset + 4],
        &chain.after,
        "apply must repair the program chain"
    );
    assert!(crate::checksum::verify(&img).iter().all(|c| c.ok));
}

/// The reference sha256 is informational only: a read that differs outside
/// the edited ranges still takes the patch.
#[test]
fn reference_hash_never_gates_apply() {
    let dir = patches_dir();
    let mut img = std::fs::read(dir.join("e46_orig.bin")).expect("reference image");
    let p = load_all()
        .0
        .into_iter()
        .find(|p| p.name == "edc15m_baud_0x91_125000")
        .expect("baud manifest");
    assert!(p.reference_matches(&img));
    img[0x20000] ^= 0xFF; // a byte no patch range touches
    assert!(
        !p.reference_matches(&img),
        "the hash differs — and that must not matter"
    );
    assert_eq!(p.state(&img), PatchState::Available);
    assert_eq!(
        p.edit_match(&img),
        (2, 2),
        "both semantic edits are present"
    );
    p.apply(&mut img).unwrap();
    assert_eq!(p.state(&img), PatchState::Applied);
    assert!(crate::checksum::verify(&img).iter().all(|c| c.ok));
}

/// Every shipped manifest, applied to the shipped reference image, must
/// reproduce the shipped output image byte for byte — checksums included.
#[test]
fn builtin_patches_reproduce_the_shipped_images() {
    let dir = patches_dir();
    let orig = std::fs::read(dir.join("e46_orig.bin")).expect("reference image");
    let (patches, warnings) = load_all();
    assert!(warnings.is_empty(), "no manifest warnings: {warnings:?}");
    let mut checked = 0;
    for p in &patches {
        let Some(out_name) = p.output.as_deref() else {
            continue;
        };
        let expected = std::fs::read(dir.join(out_name)).expect("shipped output image");
        assert!(
            p.reference_matches(&orig),
            "{}: reference image hash mismatch",
            p.name
        );
        let mut image = orig.clone();
        p.apply(&mut image)
            .unwrap_or_else(|e| panic!("{}: {e}", p.name));
        assert_eq!(image, expected, "{} must reproduce {out_name}", p.name);
        p.revert(&mut image)
            .unwrap_or_else(|e| panic!("{}: revert {e}", p.name));
        assert_eq!(image, orig, "{} revert must restore the reference", p.name);
        checked += 1;
    }
    assert_eq!(checked, 4, "all four shipped images must be checked");
}
