#!/usr/bin/env python3
"""EDC15M (BMW DDE3.0, SW 7786887) cold-start PROGRAM checksum: verify and correct.

What the ECU does (code 0x11708-0x11736, verified on e46_orig.bin):
  * walk DPP2 pages 4..0xD (file 0x10000-0x37FFF) through the 16-bit window
    0x8000-0xBFFF, then 0x0000-0x0B80 through DPP0 (file 0x38000-0x38B7F),
  * hash with the TDI V3.1 step (same step as edc15m-checksum-tool), state
    carried across pages, start state h=g=0,
  * require the final state h=0x8631, g=0xEFCD; otherwise log 0x1F and reset.
The balancing words are at file 0x38B7C-0x38B7F, directly after the block id
23A6 at 0x38B7A.  They are identical in all 31 BMW calibration releases.

This check is separate from:
  * the page-4 checksum at 0x13FFC (edc15m_checksum.py, CODE block) and
  * the calibration checksum at 0x3FBFC (edc15m_checksum.py, CAL block).
Correct 0x13FFC first (it lies inside this chain), then this one.

usage: progsum.py IMAGE...            verify
       progsum.py --fix IN OUT        write a corrected copy (never in place)
"""
import struct
import sys

M16 = 0xFFFF
TARGET = (0x8631, 0xEFCD)
FIX_AT = 0x38B7C


def rol(v, n):
    n &= 0xF
    return ((v << n) | (v >> (16 - n))) & M16 if n else v


def ror(v, n):
    n &= 0xF
    return ((v >> n) | (v << (16 - n))) & M16 if n else v


def run(buf, segments, h, g):
    """segments: list of (file_start, file_end, q_start). Returns (h, g, q)."""
    phase = 0
    q = 0
    for fs, fe, qs in segments:
        q = qs
        p = fs
        while p < fe:
            w = buf[p] | (buf[p + 1] << 8)
            p += 2
            q = (q + 2) & M16
            if phase == 0:
                h = (h ^ w) & M16
            elif phase == 1:
                flag = 0
                if g & 0xF:
                    h = rol(h, g & 0xF)
                    flag = h & 1
                g = (g - w - flag) & M16
                g = (g ^ h) & M16
            elif phase == 2:
                h = (h - w) & M16
                h = (q + h - 1) & M16
            else:
                g ^= w
                if h & 0xF:
                    g = ror(g, h & 0xF)
            phase = (phase + 1) & 3
    return h, g, q


def program_segments(end_cal=0x38B80):
    segs = [(page * 0x4000, page * 0x4000 + 0x4000, 0x8000) for page in range(4, 0xE)]
    segs.append((0x38000, end_cal, 0x0000))
    return segs


def verify(buf):
    h, g, _ = run(buf, program_segments(), 0, 0)
    return (h, g) == TARGET


def solve(buf):
    """Return the 4 bytes for FIX_AT that make the program chain end at TARGET."""
    segs = program_segments(end_cal=FIX_AT)
    h0, g0, q0 = run(buf, segs, 0, 0)
    words_before = sum(e - s for s, e, _ in segs) // 2
    assert words_before % 4 == 2, 'balancing words must fall on hash phases 2 and 3'
    th, tg = TARGET
    q1 = (q0 + 2) & M16
    w1 = (h0 + q1 - 1 - th) & M16            # phase 2: h = q + (h - w) - 1
    w2 = (g0 ^ rol(tg, th & 0xF)) & M16      # phase 3: g = ror(g ^ w, h & 0xF)
    return struct.pack('<HH', w1, w2)


def main(argv):
    if len(argv) == 4 and argv[1] == '--fix':
        src, dst = argv[2], argv[3]
        if src == dst:
            sys.exit('refusing to overwrite the input')
        b = bytearray(open(src, 'rb').read())
        if len(b) != 0x40000:
            sys.exit('need a 256 kB full read')
        old = bytes(b[FIX_AT:FIX_AT + 4])
        b[FIX_AT:FIX_AT + 4] = solve(b)
        assert verify(b)
        open(dst, 'wb').write(b)
        print(f'{dst}: 0x{FIX_AT:05X} {old.hex()} -> {b[FIX_AT:FIX_AT + 4].hex()}, program checksum OK')
        return 0
    if len(argv) < 2:
        sys.exit(__doc__)
    bad = 0
    for f in argv[1:]:
        b = open(f, 'rb').read()
        ok = len(b) == 0x40000 and verify(b)
        bad += not ok
        print(f'{f}: program checksum {"OK" if ok else "WRONG"}')
    return 1 if bad else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
