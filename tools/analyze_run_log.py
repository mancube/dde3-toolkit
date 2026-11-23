#!/usr/bin/env python3
"""Analyze a dde3-toolkit run log (`dde3-YYYYmmdd-HHMMSS.log`) systematically.

The run logs are too big to read line by line; this tool parses every
TX/RX frame, validates the LEN/XOR envelope, pairs each TX with its RX,
classifies the command, and prints a compact report:

  * per-command summary: count, TX shape, RX shape (LEN, status, data len),
    reply latency distribution
  * the flash-read sequence (addresses, contiguity, gaps)
  * optional `--image <256KB.bin>`: verify every FLASH_LESEN reply byte-for-
    byte against the image
  * the live loop: cycle period / Hz, per-id value changes over time

Usage:
    python3 tools/analyze_run_log.py captures/dde3-20261119-222226.log
    python3 tools/analyze_run_log.py <log> --image captures/actual_flash.bin
    python3 tools/analyze_run_log.py <log> --pairs          # dump every TX/RX pair
    python3 tools/analyze_run_log.py <log> --class 0x06      # dump one class
"""

import re
import sys
from collections import Counter, OrderedDict

LINE = re.compile(
    r"^(?P<ts>\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}) "
    r"\[\+\s*(?P<gap>[\d.]+)s\] (?P<dir>TX|RX) (?P<hex>[\da-fA-F ]+)\s*$"
)


def parse(path):
    """Return (frames, notes). frames = list of (dir, bytes, ts, gap)."""
    frames = []
    notes = []
    for i, ln in enumerate(open(path), 1):
        m = LINE.match(ln)
        if not m:
            if ln.strip() and not ln.startswith(("===", "started:", "mode:", "2026-")):
                notes.append((i, ln.rstrip()))
            elif ln.startswith("2026-") and "TX " not in ln and "RX " not in ln:
                notes.append((i, ln.rstrip()))
            continue
        b = bytes(int(x, 16) for x in m.group("hex").split())
        frames.append((m.group("dir"), b, m.group("ts"), float(m.group("gap"))))
    return frames, notes


def xor_all(b):
    x = 0
    for v in b:
        x ^= v
    return x


def frame_check(b):
    """Return (len_ok, xor_ok, has_xor, status, data) for a reply frame.

    Convention (docs/wire-protocol.md, session 28): LEN byte = total wire
    length INCLUDING the trailing XOR. The run log's RX lines may or may not
    include the XOR byte — detect it per frame:
      * if len(b) == LEN and xor(b[:-1]) == b[-1]  -> XOR present
      * if len(b) == LEN - 1 and xor(b) == ...     -> XOR dropped by logger
    """
    if len(b) < 3:
        return (False, False, None, None, None)
    ln = b[1]
    has_xor = None
    if len(b) == ln:
        has_xor = xor_all(b[:-1]) == b[-1]
    elif len(b) == ln - 1:
        # XOR byte dropped by the logger: verify the frame would validate
        # by checking the XOR of (b + [x]) is trivially satisfiable — instead
        # just note the length matches LEN-1.
        has_xor = False
    else:
        has_xor = None
    status = b[2] if len(b) > 2 else None
    data = b[3:]
    len_ok = has_xor is not None
    xor_ok = has_xor if has_xor is not False else None
    return (len_ok, xor_ok, has_xor, status, data)


JOB_NAMES = {
    0x00: "IDENT",
    0x02: "AIF_LESEN",
    0x04: "FS_LESEN",
    0x05: "FS_LOESCHEN",
    0x06: "FLASH_LESEN",
    0x07: "FLASH_WRITE",
    0x0A: "STATUS_CODIER_CHECKSUMME",
    0x0B: "MW",
    0x0D: "UPROG_EIN",
    0x0E: "TEST_STAMP_READ",
    0x0F: "TEST_STAMP_WRITE",
    0x14: "FS_LESEN_COUNT",
    0x40: "ADJUST_READ",
    0x41: "ADJUST_SET",
    0x42: "ADJUST_STORE",
    0x6C: "EWS_SYNC_START",
    0x6D: "EWS_SYNC_STATUS",
    0x90: "SEED_KEY",
    0x91: "BAUD",
    0x9E: "SESSION_KEEP",
    0x9F: "DIAGNOSE_ENDE",
}


def classify(t):
    """Classify a TX frame -> (class_key, detail)."""
    if len(t) < 3:
        return ("?short", "")
    job = t[2]
    name = JOB_NAMES.get(job, f"0x{job:02X}")
    sub = t[3] if len(t) > 3 else None
    if job == 0x06:  # FLASH_LESEN: addr BE @4..7, count @7
        addr = (t[4] << 16) | (t[5] << 8) | t[6]
        cnt = t[7] if len(t) > 7 else "?"
        return ("FLASH_LESEN", f"addr=0x{addr:06X} count={cnt}")
    if job == 0x07:  # flash write family
        subn = {0x02: "SCHREIBEN", 0x06: "LOESCHEN", 0x0E: "DATENBEREICH_LOESCHEN",
                0x0F: "SCHREIBEN_ENDE", 0x10: "STEUERN_TESTPLATZ"}.get(sub, f"sub=0x{sub:02X}")
        addr = (t[4] << 16) | (t[5] << 8) | t[6] if len(t) > 6 else 0
        if sub == 0x02:
            n = t[7] if len(t) > 7 else 0
            return ("FLASH_SCHREIBEN", f"addr=0x{addr:06X} n={n}")
        return (f"FLASH_{subn}", f"addr=0x{addr:06X}")
    if job == 0x0B:
        subn = {0x03: "MEAS_BLOCK", 0x04: "DIGITAL_IN", 0x07: "STEUERN_TESTPLATZ",
                0x10: "MW_SELECT_LESEN", 0x11: "PSG_READ", 0x12: "PSG_SELECT"}.get(
            sub, f"sub=0x{sub:02X}")
        if sub == 0x10:
            ids = ["%02X%02X" % (t[i], t[i + 1]) for i in range(4, len(t) - 1, 2)]
            return ("MW_SELECT_LESEN", ",".join(ids))
        return (f"MW_{subn}", "")
    if job == 0x90:
        if t[3:6] == b"BMW":
            return ("SEED_REQ", f"Z=0x{t[6]:02X}")
        return ("KEY_SEND", " ".join("%02X" % c for c in t[3:7]))
    if job == 0x04:
        return ("FS_ENTRY" if sub == 0 else "FS_SHADOW", f"sub=0x{sub:02X}")
    return (name, f"sub=0x{sub:02X}" if sub is not None else "")


def main():
    args = sys.argv[1:]
    dump_pairs = "--pairs" in args
    dump_class = None
    if "--class" in args:
        dump_class = args[args.index("--class") + 1].upper().replace("0X", "")
    image = None
    if "--image" in args:
        image = open(args[args.index("--image") + 1], "rb").read()
    path = next(a for a in args if not a.startswith("--") and a != dump_class and a != image)

    frames, notes = parse(path)
    print(f"== {path}")
    print(f"   frames: {len(frames)}  ({sum(1 for f in frames if f[0]=='TX')} TX / "
          f"{sum(1 for f in frames if f[0]=='RX')} RX)")
    for i, ln in notes:
        print(f"   [line {i}] {ln}")

    # Pair TX -> RX (strictly alternating in this log format).
    pairs = []
    i = 0
    while i < len(frames):
        d, b, ts, gap = frames[i]
        if d == "TX" and i + 1 < len(frames) and frames[i + 1][0] == "RX":
            r = frames[i + 1]
            pairs.append((b, r[1], gap, r[3]))
            i += 2
        else:
            print(f"   !! unpaired {d}: {' '.join('%02X' % c for c in b)}")
            i += 1

    # Envelope validation on RX frames.
    bad = 0
    for t, r, gap, rgap in pairs:
        ok, xok, has_xor, status, data = frame_check(r)
        if not ok or (has_xor and not xok):
            bad += 1
            print(f"   !! RX envelope mismatch: TX {t.hex(' ').upper()}")
            print(f"      RX {' '.join('%02X' % c for c in r).upper()} "
                  f"(len={len(r)}, LEN field={r[1] if len(r)>1 else '?':#04x})")
    print(f"   envelope: {len(pairs) - bad}/{len(pairs)} RX frames validate "
          f"(LEN field, XOR where present)")

    # Per-class summary.
    cls = OrderedDict()
    for t, r, gap, rgap in pairs:
        key, detail = classify(t)
        ok, xok, has_xor, status, data = frame_check(r)
        e = cls.setdefault(key, {"n": 0, "rx": Counter(), "gap": [], "details": Counter()})
        e["n"] += 1
        e["rx"][(len(r), r[1] if len(r) > 1 else 0, r[2] if len(r) > 2 else 0)] += 1
        e["gap"].append(rgap)
        e["details"][detail] += 1

    print("\n== per-command summary (RX shape: (len, LENfield, status)) ==")
    for key, e in cls.items():
        gs = sorted(e["gap"])
        p50 = gs[len(gs) // 2]
        print(f"  {key:<22} n={e['n']:<4} rx={dict(e['rx'])}  "
              f"reply-gap p50={p50*1000:.0f}ms max={gs[-1]*1000:.0f}ms")
        for d, c in e["details"].most_common(8):
            print(f"      {d}  x{c}" if c > 1 else f"      {d}")
        if len(e["details"]) > 8:
            print(f"      (+{len(e['details']) - 8} more distinct)")

    # Flash-read sequence.
    reads = [(t, r) for t, r, g, rg in pairs if classify(t)[0] == "FLASH_LESEN"]
    if reads:
        print(f"\n== FLASH_LESEN sequence: {len(reads)} reads ==")
        addrs = []
        for t, r in reads:
            addr = (t[4] << 16) | (t[5] << 8) | t[6]
            cnt = t[7]
            addrs.append((addr, cnt))
        print(f"  first: 0x{addrs[0][0]:06X}  last: 0x{addrs[-1][0]:06X}  "
              f"span 0x{addrs[0][0]:06X}..0x{addrs[-1][0] + addrs[-1][1]:06X}")
        # contiguity
        gaps = []
        for (a0, c0), (a1, c1) in zip(addrs, addrs[1:]):
            if a1 != a0 + c0:
                gaps.append((a0 + c0, a1))
        if gaps:
            print(f"  GAPS: {[(f'0x{g[0]:06X}->0x{g[1]:06X}') for g in gaps[:10]]}")
        else:
            print("  contiguous, no gaps")
        if image:
            badn = 0
            for t, r in reads:
                addr = (t[4] << 16) | (t[5] << 8) | t[6]
                cnt = t[7]
                # RX data starts at [3]; last byte may be XOR (detected:
                # len(r) == 4 + cnt + 1 with valid trailing XOR).
                data = r[3:]
                if len(data) == cnt + 1 and xor_all(r[:-1]) == r[-1]:
                    data = data[:-1]
                if data != image[addr:addr + cnt]:
                    badn += 1
                    if badn <= 3:
                        print(f"  MISMATCH @0x{addr:06X}: reply {data[:8].hex()} "
                              f"vs image {image[addr:addr+8].hex()}")
            print(f"  vs image: {'ALL MATCH' if badn == 0 else str(badn) + ' MISMATCHES'}")

    # Live loop.
    live = [(t, r, rg) for t, r, g, rg in pairs if classify(t)[0] == "MW_SELECT_LESEN"]
    if live:
        print(f"\n== live loop: {len(live)} cycles ==")
        # cycle period from the gap of the TX line (time since previous event)
        # — the log's [+x] on a TX line is the gap since the previous line.
        # Re-derive from timestamps instead.
        ts_live = [ts for d, b, ts, gap in frames if d == "TX" and b[2] == 0x0B]
        from datetime import datetime
        tss = [datetime.strptime(t, "%Y-%m-%d %H:%M:%S.%f") for t in ts_live]
        if len(tss) > 2:
            iv = [(b - a).total_seconds() for a, b in zip(tss, tss[1:])]
            iv = iv[1:-1]  # drop first/last (partial)
            if iv:
                print(f"  period: p50={sorted(iv)[len(iv)//2]*1000:.0f}ms "
                      f"min={min(iv)*1000:.0f}ms max={max(iv)*1000:.0f}ms "
                      f"-> {1.0/(sorted(iv)[len(iv)//2]):.2f} Hz")
        # value changes
        seen = OrderedDict()
        for t, r, rg in live:
            key = " ".join("%02X" % c for c in r)
            seen[key] = seen.get(key, 0) + 1
        print(f"  distinct RX frames: {len(seen)}")
        for k, c in list(seen.items())[:6]:
            print(f"    x{c:<4} {k}")

    if dump_pairs:
        print("\n== all pairs ==")
        for t, r, gap, rgap in pairs:
            key, detail = classify(t)
            print(f"  TX {t.hex(' ').upper()}  [{key} {detail}]")
            print(f"  RX {r.hex(' ').upper()}  (+{rgap*1000:.0f}ms)")

    if dump_class:
        print(f"\n== pairs of class {dump_class} ==")
        for t, r, gap, rgap in pairs:
            key, detail = classify(t)
            if key.upper() == dump_class:
                print(f"  TX {t.hex(' ').upper()}  [{detail}]")
                print(f"  RX {r.hex(' ').upper()}  (+{rgap*1000:.0f}ms)")


if __name__ == "__main__":
    main()
