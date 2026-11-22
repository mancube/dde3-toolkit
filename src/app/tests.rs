use super::*;

#[test]
fn live_rate_window_is_the_true_mean() {
    let mut stamps = VecDeque::new();
    let t0 = Instant::now();
    assert_eq!(live_rate_update(&mut stamps, t0), 0.0, "first sample: no rate yet");
    // ~50 ms per cycle → the 1 s window average must settle near 20 Hz
    // and never overshoot it (it is the plain mean of what happened).
    let mut hz = 0.0;
    for _ in 0..8 {
        thread::sleep(Duration::from_millis(50));
        hz = live_rate_update(&mut stamps, Instant::now());
    }
    assert!((14.0..28.0).contains(&hz), "expected ~20 Hz, got {hz}");
}

#[test]
fn live_rate_window_ages_out_fast_start() {
    // Overshoot scenario: a few fast cycles at the start must age out of
    // the 1 s window instead of pinning the counter high (the old EMA
    // decayed a spike over ~15 samples).
    let mut stamps = VecDeque::new();
    let t0 = Instant::now();
    let mut hz = live_rate_update(&mut stamps, t0);
    for _ in 0..3 {
        thread::sleep(Duration::from_millis(10));
        hz = live_rate_update(&mut stamps, Instant::now());
    }
    assert!(hz > 50.0, "fast start should read fast: {hz} Hz");
    // Then slow to ~100 ms cycles; within the next second the window
    // must reflect the slow reality, not the fast start.
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(100));
        hz = live_rate_update(&mut stamps, Instant::now());
    }
    assert!((5.0..20.0).contains(&hz), "expected ~10 Hz after aging, got {hz}");
}
