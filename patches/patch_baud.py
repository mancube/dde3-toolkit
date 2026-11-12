#!/usr/bin/env python3
"""EDC15M / DDE3.0 fast-DS2 patch — add DS2 service 0x91 (K-line baud change).

Stock firmware has no 0x91 service: the DS2 dispatcher (0x2932C-0x293D0) falls
through unmatched services to the unsupported path 0x295F2 (reply 12 04 FF), and
the receive state machine (0x1E904, states 0-2) never re-rates the S0 UART.  So
stock DS2 is locked at 9600 8E1.

This patch hooks the dispatcher fall-through to a trampoline that, on service
0x91, ACKs and writes the S0 baud register S0BG to the requested divisor.  The
DS2 protocol itself is unchanged — only the wire speed rises, so the existing
dde3-toolkit DS2 code works verbatim once it reopens the port at the new rate.

Baud ladder (verified in firmware; baud = 9600 * 52 / (S0BG+1), anchored on the
stock init S0BG=0x33 -> 9600 at 0x1E8F4):

    rate      S0BG   actual    error    stock use
    9600      0x33    9600     0 %      default
    19200     0x19   19200     0 %      -
    38400     0x0C   38400     0 %      K2 fast state (0x1ED66)
    125000    0x03  124800    -0.16 %   (new; the N=3 divisor other C166
                                         BMW fast-loggers use)

The handler is hand-assembled with idioms copied verbatim from stock code
(CALLS 0x2692C header build, CBFA=A0 ACK, the 0x1E8F4 S0BG encoding, the
0x295F6 reply tail) and every instruction was verified with the project
disassembler.

Layout (32 bytes, all in a 65-byte erased block at 0x185C3; hook is 4 bytes):

    0x293D0  ea00f295 (JMPA 0x295F2)  ->  fa01c385 (JMPS 0x185C3)   [hook]
    0x185C3  46 f4 91 00   CMP   R4,#0091h
    0x185C7  ea 20 cf 85   JMPA  cc_Z,0x185CF
    0x185CB  fa 02 f2 95   JMPS  0x0295F2          ; not 0x91 -> unsupported
    0x185CF  da 02 2c 69   CALLS 0x02692C          ; build reply header
    0x185D3  e7 fa a0 00   MOVB  RL5,#00A0h
    0x185D7  f7 fa fa cb   MOVB  CBFAh,RL5         ; ACK status A0
    0x185DB  e6 5a XX 00   MOV   S0BG,#00XXh       ; XX = target divisor
    0x185DF  fa 02 f6 95   JMPS  0x0295F6          ; common reply tail

Wire use (tester = dde3-toolkit):
    tx @9600 : 12 08 91 00 <baud 24-bit BE> 03     (e.g. 125000 = 01 E8 48)
    the ACK is queued AFTER S0BG flips, so it leaves at the NEW rate — do not
    wait for a reply at 9600.  Reopen the port at the new rate, drain, then
    send IDENT (12 04 00) to confirm the link.  Revert = power cycle (boot
    re-inits 9600 at 0x1E8F4).

CORRECTION vs. the earlier baud91 note: that note said "no TDI checksum
impact" and used JMPA (ea..) for the cross-segment hook.  Both were wrong:
(1) the hook/trampoline lie in the cold-start PROGRAM-CHAIN checksum region
(0x10000-0x38B7F), so the balancing word at 0x38B7C must be recomputed, and
(2) a dispatcher-to-0x185C3 jump crosses segments (2 -> 1) and needs JMPS, not
JMPA (JMPA would land at 0x0218C3 and crash).  This builder fixes both.

usage: patch_baud.py [--rate 125000|38400|19200|9600] [IN] [OUT]
       defaults: rate=125000  IN=./e46_orig.bin  OUT=./e46_baud<rate>.bin
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import edc15m_patch as P  # noqa: E402

HOOK = 0x293D0
T = 0x185C3
H = 0x185CF
S0BG = {9600: 0x33, 19200: 0x19, 38400: 0x0C, 125000: 0x03}
ACTUAL = {9600: 9600, 19200: 19200, 38400: 38400, 125000: 124800}
BAUD_BE = {9600: "002580", 19200: "004B00", 38400: "009600", 125000: "01E848"}


def build_patch(rate):
    xx = S0BG[rate]
    hook_new = "fa01c385"
    t_code = "46f49100" + "ea20cf85" + "fa02f295"
    h_code = "da022c69" + "e7faa000" + "f7fafacb" + f"e65a{xx:02x}00" + "fa02f695"
    edits = [(HOOK, "ea00f295", hook_new)]
    free = [(T, t_code), (H, h_code)]
    return edits, free


def main(argv):
    rate = 125000
    args = []
    i = 1
    while i < len(argv):
        if argv[i] == "--rate":
            rate = int(argv[i + 1])
            i += 2
        else:
            args.append(argv[i])
            i += 1
    if rate not in S0BG:
        sys.exit(f"rate must be one of {sorted(S0BG)}")
    src = args[0] if args else os.path.join(HERE, "e46_orig.bin")
    dst = args[1] if len(args) > 1 else os.path.join(HERE, f"e46_baud{rate}.bin")
    print(f"[i] baud patch: DS2 service 0x91 -> S0BG 0x{S0BG[rate]:02X} "
          f"({ACTUAL[rate]} baud, requested {rate})")
    print(f"[i] {src} -> {dst}")
    orig = P.load_image(src)
    edits, free = build_patch(rate)
    buf, _ = P.build(src, dst, edits, extra_free_writes=free)
    print(f"[i] tester frame @9600: 12 08 91 00 {BAUD_BE[rate]} 03  (then reopen at {ACTUAL[rate]})")
    # Rate-specific name: the 125000 and 38400 images are different patches
    # and must not collide in the toolkit's manifest list.
    man = P.manifest(f"edc15m_baud_0x91_{rate}",
                     f"Add DS2 service 0x91; switches the K-line to {ACTUAL[rate]} baud "
                     f"(S0BG 0x{S0BG[rate]:02X}) on `12 08 91 00 {BAUD_BE[rate]} 03`. Revert = power cycle.",
                     src, dst, orig, buf)
    open(dst.rsplit(".", 1)[0] + ".json", "w").write(man)
    print(f"[i] manifest -> {dst.rsplit('.', 1)[0]}.json")


if __name__ == "__main__":
    main(sys.argv)
