#!/usr/bin/env python3
"""EDC15M / DDE3.0 immobiliser (EWS) delete — for off-road / bench use.

The EWS state time-shares the K-line and publishes a release byte at RAM
`C3A0` (0x00 = released/matched, 0xFF = locked) plus a flag `C3B7.0`.  The
fuel-quantity path gates injection on them at 0x2A652-0x2A670:

    02A652  MOVB RL5, C3A0h           ; RL5 = EWS release byte
    02A65C  AND  R4,#1                ; R4 = C3B7 & 1
    02A65E  JMPR cc_NZ, 02A666        ; flag set            -> cut
    02A660  CMPB RL5,#FFh
    02A664  JMPR cc_NZ, 02A66C        ; C3A0 != 0xFF (ok)   -> use real quantity
    02A666  MOV  C178h, ZEROS         ; otherwise C178 = 0  = FUEL CUT
    02A66C  MOV  R4, C176h ; MOV C178h,R4                   = real quantity

Two modes:

  gate  (default, recommended) — one 2-byte edit at 0x2A65E:
          3d 03  (JMPR cc_NZ,0x2A666)  ->  0d 06  (JMPR cc_UC,0x2A66C)
        Injection quantity is never zeroed by EWS; nothing else changes.
        EWS diagnostics/DTC still report truthfully.

  full  — mirrors the user's e46_immo_off.bin: force the EWS result/release
        flags themselves so every consumer of C136/C13C/C3A0 sees "matched":
          0x300ED/0x30161/0x308BD  MOVB C136h,ZEROS -> MOVB C136h,ONES  (8e->8f)
          0x303BE/0x31164          MOVB RLn,C13Ch    -> MOVB RLn,#01
        Use this only if a downstream EWS consumer besides the fuel gate
        matters on your car; otherwise 'gate' is cleaner.

Both modes land in the cold-start PROGRAM-CHAIN checksum region, so the
balancing word at 0x38B7C is recomputed automatically.  (The supplied
e46_immo_off.bin does NOT have a valid program chain — it bypassed the
check instead of correcting it; this builder corrects it.)

usage: patch_immo_off.py [--mode gate|full] [IN] [OUT]
       defaults: IN=./e46_orig.bin  OUT=./e46_immo_off_<mode>.bin
"""
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import edc15m_patch as P  # noqa: E402

EDITS = {
    "gate": [(0x2A65E, "3d03", "0d06")],
    "full": [
        (0x300ED, "8e36", "8f36"),
        (0x30161, "8e36", "8f36"),
        (0x308BD, "8e36", "8f36"),
        (0x303BE, "f3f43cc1", "e7f40100"),
        (0x31164, "f3f83cc1", "e7f80100"),
    ],
}


def main(argv):
    mode = "gate"
    args = []
    i = 1
    while i < len(argv):
        if argv[i] == "--mode":
            mode = argv[i + 1]
            i += 2
        else:
            args.append(argv[i])
            i += 1
    if mode not in EDITS:
        sys.exit(f"mode must be gate or full, not {mode}")
    src = args[0] if args else os.path.join(HERE, "e46_orig.bin")
    dst = args[1] if len(args) > 1 else os.path.join(HERE, f"e46_immo_off_{mode}.bin")
    print(f"[i] immo-off ({mode}) {src} -> {dst}")
    orig = P.load_image(src)
    buf, _ = P.build(src, dst, EDITS[mode])
    man = P.manifest(f"edc15m_immo_off_{mode}",
                     f"EWS immobiliser delete ({mode}); off-road use. "
                     + ("Fuel gate 0x2A65E forced to use real quantity." if mode == "gate"
                        else "EWS result/release flags forced to matched."),
                     src, dst, orig, buf)
    open(dst.rsplit(".", 1)[0] + ".json", "w").write(man)
    print(f"[i] manifest -> {dst.rsplit('.', 1)[0]}.json")


if __name__ == "__main__":
    main(sys.argv)
