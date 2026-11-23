use super::*;
#[test]
fn tables_are_sorted_sparse() {
    assert!(ORT.windows(2).all(|w| w[0].0 < w[1].0));
    assert!(FART.windows(2).all(|w| w[0].0 < w[1].0));
    assert!(UMWELT.windows(2).all(|w| w[0].0 < w[1].0));
}
#[test]
fn known_entries() {
    assert_eq!(ort_text(0x05), "Nadelbewegungsfuehler");
    assert_eq!(fart_text(0x01), "Signal Kurzschluss nach B+");
    let (u1, u2) = ort_uw_ids(0x05);
    assert_eq!((u1, u2), (0x02, 0x07));
    let uw = uw_info(0x08).expect("battery voltage entry");
    assert_eq!(uw.name, "Batteriespannung");
    assert!((uw_value(0x08, 148.0 as u16) - 14.78).abs() < 0.05);
    assert!(matrix_row(0x00).is_some());
    // M47 reference report (actual error.txt): ORT 0x08, mask 0xC8
    assert_eq!(active_arts(0x08, 0xC8), vec![0x32, 0x05, 0x07]);
    assert_eq!(fart_text(0x32), "Gluehkerzen bzw. Gluehrelais defekt");
    assert_eq!(fart_text(0x05), "Fehler momentan vorhanden");
    assert_eq!(fart_text(0x07), "Fehler sporadisch");
    assert_eq!(ort_text(0x02), "unbekannter Fehlerort");
    assert_eq!(fart_text(0x91), "unbekannte Fehlerart");
}
