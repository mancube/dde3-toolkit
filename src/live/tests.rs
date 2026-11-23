//! Tests for the live-data variable table and its raw-value decoding.

use super::LIVE_VARS;

fn var(id: u16) -> &'static super::LiveVar {
    LIVE_VARS
        .iter()
        .find(|v| v.id == id)
        .expect("live var by id")
}

#[test]
fn signed_var_sign_extends_negative_raw() {
    // SB injection start setpoint (0x0F52): the ECU reports it signed.
    // Raw 0xFFFE used to display as 655.34; it is -2 * 0.01 = -0.02.
    let v = var(0x0F52);
    assert!(v.signed);
    assert!((v.decode(0xFFFE) - (-0.02)).abs() < 1e-9);
    assert!((var(0x0F53).decode(0xFFFE) - (-0.02)).abs() < 1e-9); // actual value
}

#[test]
fn unsigned_var_keeps_full_u16_range() {
    // Water temperature (0x0F00): raw is u16, phys = raw * 0.1 - 273.14.
    let v = var(0x0F00);
    assert!(!v.signed);
    assert!((v.decode(3000) - 26.86).abs() < 1e-9); // 300 K
    // A high raw must NOT wrap negative on an unsigned variable.
    assert!(v.decode(0xFFFF) > 0.0);
}

#[test]
fn every_var_decodes_monotonically_in_its_domain() {
    for v in LIVE_VARS {
        if v.signed {
            // i16 domain: 0x8000 is the most negative, 0x7FFF the max.
            assert!(v.decode(0x8000) < 0.0, "{} should be negative", v.name);
            assert!(v.decode(0x7FFF) > 0.0, "{} should be positive", v.name);
            assert!(v.decode(0x8000) < v.decode(0x0001), "{}", v.name);
        } else {
            assert!(v.decode(0xFFFF) >= v.decode(0x0000), "{}", v.name);
        }
    }
}

#[test]
fn signed_set_matches_adf_data_type() {
    // The 22 DATA_TYPE-7 variables from the tool32 ADF export. If the CSV
    // is re-exported and regenerated, this list is the cross-check.
    let mut signed_ids: Vec<u16> = LIVE_VARS
        .iter()
        .filter(|v| v.signed)
        .map(|v| v.id)
        .collect();
    signed_ids.sort_unstable();
    let expected: Vec<u16> = vec![
        0x0ECB, 0x0F0A, 0x0F52, 0x0F53, 0x0F57, 0x0F80, 0x0F82, 0x0F84, 0x0F85,
        0x0F86, 0x0F89, 0x0F8A, 0x0F8B, 0x0F8C, 0x0F8D, 0x0F94, 0x0F97, 0x0F98,
        0x0F99, 0x0F9A, 0x0F9C, 0x1F8E,
    ];
    assert_eq!(signed_ids, expected);
}
