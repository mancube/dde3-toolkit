#!/usr/bin/env python3
"""Analyze the 2026-11-23 real-car calibration-write run log + MPPS reference.

Answers two questions from the on-car test:
  1. Why did the flash-session bootstrap IDENT fail twice (attempts 1 and 2)
     but work at connect and on attempt 3?
  2. How does our flash-write pacing compare to MPPS's (captures/mpps
     capture/mppswrite.txt), frame class by frame class?

Run-log format (src/app.rs log writer + Kline::log):
  * frame lines:  `<flush-ts> [+<delta>s] TX|RX <hex>` — the absolute stamp is
    the FLUSH time of the whole batch; the true time of a frame is the
    previous frame's true time + delta (deltas are frame-to-frame).
  * note lines (BATTERY / ERROR / FAST9600 / ...): absolute stamp is real.

Usage:
    python3 tools/analyze_realcar_write.py captures/dde3-20261123-122003.log
    ... --mpps "captures/mpps capture/mppswrite.txt"
"""

import re
import sys
from collections import defaultdict

RUN = re.compile(
    r"^(?P<ts>\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}) "
    r"\[\+\s*(?P<gap>[\d.]+)s\] (?P<dir>TX|RX) (?P<hex>[0-9a-fA-F ]+)\s*$"
)
NOTES = re.compile(r"^(?P<ts>\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}) (?P<text>.*)$")
MPPS = re.compile(r"^\[\s*(?P<t>[\d.]+)\] (?P<dir>RX|TX|ECHO)\s+(?P<hex>[0-9a-fA-F ]+)(?:\s+(?P<lbl>\w+.*))?$")

def ts2s(ts):
    _date, hms = ts.split(" ")
    hh, mm, ss = hms.split(":")
    sec, ms = ss.split(".")
    return ((int(hh) * 60 + int(mm)) * 60 + int(sec)) + int(ms) / 1000.0


def classify(payload: bytes):
    """Classify a DS2 payload (without the 0x12 prefix)."""
    if len(payload) < 3:
        return "short"
    sid = payload[1]
    sub = payload[2] if len(payload) > 2 else None
    names = {
        0x00: "IDENT", 0x0D: "UPROG_EIN", 0x02: "AIF_READ",
        0x04: "FS_LESEN", 0x05: "FS_LOESCHEN", 0x90: "SEED_KEY",
        0x06: "FLASH_LESEN", 0x07: "FLASH_WRITE", 0x0F: "TEST_STAMP",
        0x9F: "DIAGNOSE_ENDE", 0x10: "LIVE",
    }
    base = names.get(sid, f"svc0x{sid:02X}")
    if sid == 0x07:
        subs = {0x02: "SCHREIBEN", 0x06: "LOESCHEN", 0x0F: "SCHREIBEN_ENDE"}
        base += "/" + subs.get(sub, f"sub{sub:02X}")
    elif sid == 0x0B:
        base = "LIVE_READ"
    return base


def parse_run_log(path, max_lines=None):
    """Return list of events: (t, kind, payload-or-text, rawline)."""
    events = []
    last_frame_t = None
    prev_ts = None
    for i, ln in enumerate(open(path), 1):
        if max_lines is not None and i > max_lines:
            break
        m = RUN.match(ln)
        if m:
            gap = float(m.group("gap"))
            abs_ts = m.group("ts")
            if last_frame_t is None:
                t = ts2s(abs_ts)  # first frame: flush time ~ its time
            elif gap == 0.0 and abs_ts != prev_ts:
                # fresh Kline instance (reconnect): delta chain resets to 0,
                # so anchor to this batch's flush time instead of chaining.
                t = ts2s(abs_ts)
            else:
                t = last_frame_t + gap
            last_frame_t = t
            prev_ts = abs_ts
            b = bytes(int(x, 16) for x in m.group("hex").split())
            events.append([t, m.group("dir"), b, ln.rstrip()])
            continue
        m = NOTES.match(ln)
        if m and not ln.startswith(("===", "started:", "mode:")):
            events.append([ts2s(m.group("ts")), "NOTE", m.group("text").strip(), ln.rstrip()])
    return events


def fmt(t):
    return f"{t:9.3f}"


def main():
    log = sys.argv[1] if len(sys.argv) > 1 else "captures/dde3-20261123-122003.log"
    mpps = None
    if "--mpps" in sys.argv:
        mpps = sys.argv[sys.argv.index("--mpps") + 1]
    max_lines = None
    if "--lines" in sys.argv:
        max_lines = int(sys.argv[sys.argv.index("--lines") + 1])

    ev = parse_run_log(log, max_lines)
    t0 = ev[0][0]
    for e in ev:
        e[0] -= t0  # relative to first event

    frames = [e for e in ev if e[1] in ("TX", "RX")]
    notes = [e for e in ev if e[1] == "NOTE"]

    print(f"=== {log} ===")
    print(f"events: {len(ev)}  frames: {len(frames)}  notes: {len(notes)}")
    print(f"span: {ev[-1][0]:.1f} s\n")

    # ---- 1. timeline of notes + context around each ERROR -----------------
    print("--- NOTE lines (absolute, rel to start) ---")
    for t, _, text, _ in notes:
        print(f"  [{fmt(t)}] {text}")
    print()

    # error contexts: 6 frames before / 2 after each ERROR note
    print("--- frames in [err-30s, err+3s] around each ERROR ---")
    for (t, kind, text, _) in notes:
        if "ERROR" not in text:
            continue
        print(f"  !! [{fmt(t)}] {text}")
        for (ft, d, b, _) in frames:
            if t - 30 <= ft <= t + 3:
                print(f"     [{fmt(ft)}] {d} {b[:10].hex(' ')}{'...' if len(b) > 10 else ''}")
    print()

    # ---- 2. per-class TX pacing in our run ---------------------------------
    print("--- our run: per-class TX stats (gap to next TX of any class) ---")
    tx = [e for e in frames if e[1] == "TX"]
    cls = defaultdict(list)
    for i, (t, _, b, _) in enumerate(tx):
        name = classify(b[1:])
        gap_next = tx[i + 1][0] - t if i + 1 < len(tx) else None
        cls[name].append((t, gap_next, b))
    for name in sorted(cls):
        rows = cls[name]
        gaps = [g for _, g, _ in rows if g is not None]
        if gaps:
            import statistics
            print(f"  {name:22s} n={len(rows):4d}  gap min={min(gaps)*1000:7.0f}ms "
                  f"med={statistics.median(gaps)*1000:7.0f}ms max={max(gaps)*1000:7.0f}ms")
        else:
            print(f"  {name:22s} n={len(rows):4d}")
    print()

    # ---- 3. TX->RX reply latency per class ----------------------------------
    print("--- our run: TX->RX reply latency (first RX before next TX) ---")
    rx = [e for e in frames if e[1] == "RX"]
    lat = defaultdict(list)
    unpaired = 0
    for i, (t, _, b, _) in enumerate(tx):
        nxt = tx[i + 1][0] if i + 1 < len(tx) else None
        found = False
        for (tr, _, br, _) in rx:
            # strictly after: a +0.000s-delta line shares its time with the
            # previous line, which may be the PREVIOUS frame's reply
            if tr <= t:
                continue
            if nxt is not None and tr > nxt:
                break
            lat[classify(b[1:])].append(tr - t)
            found = True
            break
        if not found:
            unpaired += 1
    if unpaired:
        print(f"  ({unpaired} TX frames had no reply before the next TX)")
    for name in sorted(lat):
        v = sorted(lat[name])
        import statistics
        print(f"  {name:22s} n={len(v):4d}  med={statistics.median(v)*1000:7.0f}ms "
              f"p90={v[int(len(v)*0.9)]*1000:7.0f}ms max={v[-1]*1000:7.0f}ms")
    print()

    # ---- 4. MPPS reference pacing ------------------------------------------
    if mpps:
        print(f"=== MPPS reference: {mpps} ===")
        mev = []
        for ln in open(mpps):
            m = MPPS.match(ln)
            if m:
                b = bytes(int(x, 16) for x in m.group("hex").split())
                mev.append((float(m.group("t")), m.group("dir"), b))
        mpps_tx = [e for e in mev if e[1] == "RX"]  # RX in respond log = MPPS->ECU
        mpps_rx = [e for e in mev if e[1] == "TX"]  # TX in respond log = ECU->MPPS
        mt0 = mpps_tx[0][0]
        mpps_tx = [(t - mt0, d, b) for t, d, b in mpps_tx]
        mpps_rx = [(t - mt0, d, b) for t, d, b in mpps_rx]
        print(f"  MPPS frames: {len(mpps_tx)}  span: {mpps_tx[-1][0]:.1f}s")
        print("  --- MPPS per-class TX stats ---")
        mcls = defaultdict(list)
        for i, (t, _, b) in enumerate(mpps_tx):
            name = classify(b[1:])
            gap = mpps_tx[i + 1][0] - t if i + 1 < len(mpps_tx) else None
            mcls[name].append(gap)
        for name in sorted(mcls):
            gaps = [g for g in mcls[name] if g is not None]
            if gaps:
                import statistics
                print(f"  {name:22s} n={len(gaps):4d}  gap min={min(gaps)*1000:7.0f}ms "
                      f"med={statistics.median(gaps)*1000:7.0f}ms max={max(gaps)*1000:7.0f}ms")
        print("  --- MPPS TX->reply latency ---")
        mlat = defaultdict(list)
        j = 0
        for i, (t, _, b) in enumerate(mpps_tx):
            nxt = mpps_tx[i + 1][0] if i + 1 < len(mpps_tx) else None
            for k in range(j, len(mpps_rx)):
                tr, _, _ = mpps_rx[k]
                if tr < t:
                    continue
                if nxt is not None and tr > nxt:
                    break
                j = k
                mlat[classify(b[1:])].append(tr - t)
                break
        for name in sorted(mlat):
            v = sorted(mlat[name])
            import statistics
            print(f"  {name:22s} n={len(v):4d}  med={statistics.median(v)*1000:7.0f}ms max={v[-1]*1000:7.0f}ms")
        print()

    # ---- 5. write phase breakdown (our run) --------------------------------
    print("--- our run: FLASH write phase timeline (first/last, total) ---")
    w = [(t, b) for t, d, b, _ in frames if d == "TX" and len(b) > 4 and b[2] == 0x07]
    if w:
        kinds = defaultdict(list)
        for t, b in w:
            kinds[("data" if b[3] == 0x02 else f"sub{b[3]:02X}")].append(t)
        for k, ts_ in sorted(kinds.items()):
            print(f"  {k:8s} n={len(ts_):4d}  first={ts_[0]:.1f}s last={ts_[-1]:.1f}s "
                  f"span={ts_[-1]-ts_[0]:.1f}s")
        data = kinds.get("data", [])
        if len(data) > 2:
            gaps = [data[i + 1] - data[i] for i in range(len(data) - 1)]
            import statistics
            print(f"  data-frame cadence: med={statistics.median(gaps)*1000:.0f}ms "
                  f"min={min(gaps)*1000:.0f}ms max={max(gaps)*1000:.0f}ms")
            print(f"  => {len(data)} frames in {data[-1]-data[0]:.1f}s = "
                  f"{(data[-1]-data[0])/len(data)*1000:.0f} ms/frame")
    print()

    # ---- 6. silence gaps > 5s between frames -------------------------------
    print("--- gaps > 5s between consecutive frames ---")
    for i in range(1, len(frames)):
        g = frames[i][0] - frames[i - 1][0]
        if g > 5:
            print(f"  {frames[i-1][0]:9.3f} -> {frames[i][0]:9.3f}  ({g:7.1f}s)  "
                  f"next: {frames[i][1]} {frames[i][2][:8].hex(' ')}")


if __name__ == "__main__":
    main()
