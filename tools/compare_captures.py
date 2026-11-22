#!/usr/bin/env python3
"""Summarize the request (RX-in-fake-ECU-log) command sequence of a capture.

The captures are `respond` (fake ECU) logs: every RX line is a request the
external tool (MPPS / Galletto) sent to the fake ECU. This extracts the
request sequence, classifies each frame, and collapses the long
FLASH_LESEN / FLASH_SCHREIBEN runs into address ranges so the *procedure*
(bootstrap order, block sizes, session structure, DIAGNOSE_ENDE placement)
is readable at a glance.

Usage:
    python3 tools/compare_captures.py <capture.txt> [more.txt ...]
"""

import re
import sys

LINE = re.compile(r"^\[\s*([\d.]+)\]\s+(RX|TX|ECHO)\s+([0-9A-Fa-f ]+)\s*(\S.*)?$")


def parse(path):
    reqs = []  # (ts, bytes)
    for ln in open(path):
        m = LINE.match(ln)
        if not m:
            continue
        ts, direction, hexs, _label = m.groups()
        if direction != "RX":  # RX in the fake-ECU log = the tool's request
            continue
        b = bytes(int(x, 16) for x in hexs.split())
        reqs.append((float(ts), b))
    return reqs


def classify(b):
    """Classify a request frame -> (kind, detail)."""
    if len(b) < 3:
        return ("?short", "")
    sid = b[2]
    sub = b[3] if len(b) > 3 else None
    addr = (b[4] << 16) | (b[5] << 8) | b[6] if len(b) > 6 else None

    if sid == 0x00:
        return ("IDENT", "")
    if sid == 0x0D:
        return ("UPROG_EIN", "")
    if sid == 0x02:
        return ("AIF_LESEN (02)", f"param=0x{sub:02X}" if sub is not None else "")
    if sid == 0x04:
        return ("FS_LESEN (04)", f"sub=0x{sub:02X}")
    if sid == 0x05:
        return ("FS_LOESCHEN (05)", f"sub=0x{sub:02X}")
    if sid == 0x06:  # FLASH_LESEN: addr BE @4..6, count @7
        cnt = b[7] if len(b) > 7 else 0
        return ("FLASH_LESEN", f"addr=0x{addr:06X} n={cnt}")
    if sid == 0x07:
        subn = {0x02: "FLASH_SCHREIBEN", 0x06: "FLASH_LOESCHEN",
                0x0E: "DATENBEREICH_LOESCHEN", 0x0F: "FLASH_SCHREIBEN_ENDE",
                0x10: "STEUERN_TESTPLATZ"}.get(sub, f"07/0x{sub:02X}")
        if sub == 0x02:
            n = b[7] if len(b) > 7 else 0
            return (subn, f"addr=0x{addr:06X} n={n}")
        return (subn, f"addr=0x{addr:06X}" if addr is not None else "")
    if sid == 0x0B:
        subn = {0x03: "MW_MEAS_BLOCK", 0x04: "MW_DIGITAL_IN", 0x07: "MW_STEUERN",
                0x10: "MW_SELECT_LESEN", 0x11: "MW_PSG_READ", 0x12: "MW_PSG_SELECT"}.get(
            sub, f"MW/0x{sub:02X}")
        return (subn, "")
    if sid == 0x90:
        if b[3:6] == b"BMW":
            return ("SEED_KEY_REQ", f"Z=0x{b[6]:02X}")
        return ("KEY_SEND", " ".join("%02X" % c for c in b[3:7]))
    if sid == 0x9F:
        return ("DIAGNOSE_ENDE", "")
    if sid == 0x9E:
        return ("SESSION_KEEP", "")
    if sid == 0x0A:
        return ("STATUS_CODIER_CHECKSUMME", "")
    if sid == 0x91:
        return ("BAUD_UMSTELLUNG", "")
    return (f"sid=0x{sid:02X}", f"sub=0x{sub:02X}" if sub is not None else "")


def summarize(path):
    reqs = parse(path)
    print(f"\n{'=' * 78}\n{path}  —  {len(reqs)} requests\n{'=' * 78}")

    # Collapse consecutive same-kind flash runs into ranges.
    out = []
    i = 0
    while i < len(reqs):
        ts, b = reqs[i]
        kind, detail = classify(b)
        if kind in ("FLASH_LESEN", "FLASH_SCHREIBEN"):
            j = i
            addrs = []
            while j < len(reqs) and classify(reqs[j][1])[0] == kind:
                _, fb = reqs[j]
                a = (fb[4] << 16) | (fb[5] << 8) | fb[6]
                n = fb[7] if len(fb) > 7 else 0
                addrs.append((a, n))
                j += 1
            lo = min(a for a, _ in addrs)
            hi = max(a + n for a, n in addrs)
            sizes = sorted(set(n for _, n in addrs))
            out.append(f"  {kind:<22} x{len(addrs):<5} span 0x{lo:06X}..0x{hi:06X}  "
                       f"block={sizes}")
            i = j
        else:
            out.append(f"  {kind:<22} {detail}")
            i += 1
    for line in out:
        print(line)


if __name__ == "__main__":
    for p in sys.argv[1:]:
        summarize(p)
