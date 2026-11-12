#!/usr/bin/env python3
"""Shared helpers for EDC15M / DDE3.0 flash patches.

A patch is a list of byte edits on the 256 kB full read. After applying the
edits, three checksums must be corrected, in this exact order, or the ECU
resets at boot (reset code 9) or refuses to run:

  1. TDI V3.1 code block  -> value at file 0x13FFC   (covers 0x10000..0x13FFB)
  2. TDI V3.1 calibration -> value at file 0x3FBFC   (covers 0x38B80..0x3FBFF)
  3. Cold-start program chain -> balancing word at 0x38B7C
       covers flash pages 4..0xD (0x10000..0x37FFF) + 0x38000..0x38B7F,
       must end h=0x8631 g=0xEFCD.

Step 1 lies inside the step-3 range, so 1/2 run before 3.  None of the three
fields overlaps, so the sequence is not circular.

IMPORTANT correction to older notes: an edit anywhere in 0x10000..0x38B7F
changes the PROGRAM CHAIN even when it is outside the two TDI slots.  The
earlier baud-patch note that claimed "no TDI checksum impact" was checking
only the two TDI slots and missed the cold-start chain; such an image boots
into a reset loop.  Always call fix_checksums().

The two verified reference tools are vendored next to this file in
`tools/` (copied verbatim from `edc15m-checksum-tool/edc15m_checksum.py`
and `progsum.py`), so a fresh checkout of the toolkit runs the builders
with no external layout. The old external paths are still accepted as a
fallback for the analysis machine.
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
IMAGE_SIZE = 0x40000


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _resolve(*candidates):
    for path in candidates:
        if os.path.exists(path):
            return path
    raise SystemExit(
        "missing reference tool; looked for:\n  " + "\n  ".join(candidates)
    )


_cs = _load(_resolve(os.path.join(HERE, "tools", "edc15m_checksum.py"),
                     os.path.join(ROOT, "edc15m-checksum-tool", "edc15m_checksum.py")),
            "edc15m_checksum")
_prog = _load(_resolve(os.path.join(HERE, "tools", "progsum.py"),
                       os.path.join(ROOT, "progsum.py")),
              "progsum")


class PatchError(Exception):
    pass


def load_image(path):
    data = bytearray(open(path, "rb").read())
    if len(data) != IMAGE_SIZE:
        raise PatchError(f"{path}: expected a {IMAGE_SIZE}-byte full read, got {len(data)}")
    return data


def apply_edits(buf, edits):
    """edits: list of (offset, before_hex, after_hex). Every before must match."""
    report = []
    for off, before, after in edits:
        b, a = bytes.fromhex(before), bytes.fromhex(after)
        if len(a) != len(b):
            raise PatchError(f"0x{off:05X}: before/after length mismatch")
        cur = bytes(buf[off:off + len(b)])
        if cur != b:
            raise PatchError(f"0x{off:05X}: expected {before} but image has {cur.hex()}")
        buf[off:off + len(a)] = a
        report.append((off, before, after))
    return report


def fix_checksums(buf):
    """Correct the TDI code+cal checksums, then the program chain. Returns a dict."""
    out = {}
    ok, bad = _cs.process(buf, fix=True)          # 1 + 2: code 0x13FFC, cal 0x3FBFC
    out["tdi_corrected"] = bad
    out["tdi_code"] = int.from_bytes(buf[0x13FFC:0x14000], "little")
    out["tdi_cal"] = int.from_bytes(buf[0x3FBFC:0x3FC00], "little")
    old_chain = bytes(buf[_prog.FIX_AT:_prog.FIX_AT + 4])
    buf[_prog.FIX_AT:_prog.FIX_AT + 4] = _prog.solve(buf)   # 3: program chain
    out["prog_chain_word_old"] = old_chain.hex()
    out["prog_chain_word_new"] = bytes(buf[_prog.FIX_AT:_prog.FIX_AT + 4]).hex()
    if not _prog.verify(buf):
        raise PatchError("program-chain verify failed after fix")
    _ok, _bad = _cs.process(bytearray(buf), fix=False)
    if _bad:
        raise PatchError("TDI verify failed after fix")
    out["ok"] = True
    return out


def diff_report(orig, patched):
    """List contiguous changed ranges (offset, old_hex, new_hex)."""
    ranges = []
    i = 0
    n = len(orig)
    while i < n:
        if orig[i] != patched[i]:
            j = i
            while j < n and orig[j] != patched[j]:
                j += 1
            ranges.append((i, bytes(orig[i:j]).hex(), bytes(patched[i:j]).hex()))
            i = j
        else:
            i += 1
    return ranges


def build(src, dst, edits, extra_free_writes=None, verbose=True):
    """Full pipeline: load, apply edits (+ optional free-space code), checksum, write.

    extra_free_writes: list of (offset, after_hex) placed into erased space; the
    'before' is asserted to be 0xFF filler so we never clobber real code.
    """
    orig = load_image(src)
    buf = bytearray(orig)
    rep = apply_edits(buf, edits)
    if extra_free_writes:
        for off, after in extra_free_writes:
            a = bytes.fromhex(after)
            if bytes(buf[off:off + len(a)]) != b"\xff" * len(a):
                raise PatchError(f"0x{off:05X}: free-space target is not 0xFF filler")
            buf[off:off + len(a)] = a
            rep.append((off, "ff" * len(a), after))
    cks = fix_checksums(buf)
    open(dst, "wb").write(buf)
    if verbose:
        for off, before, after in sorted(rep):
            print(f"  [+] 0x{off:05X}  {before}  ->  {after}")
        print(f"  [i] TDI code  @0x13FFC = {cks['tdi_code']:08X}")
        print(f"  [i] TDI cal   @0x3FBFC = {cks['tdi_cal']:08X}")
        print(f"  [i] prog chain@0x38B7C = {cks['prog_chain_word_old']} -> {cks['prog_chain_word_new']}")
        total = sum(len(b) // 2 for _, b, _ in diff_report(orig, buf))
        print(f"  [i] wrote {dst} ({total} bytes changed, all checksums valid)")
    return buf, cks


def manifest(name, description, src, dst, orig, patched, hardware_tested=False):
    """A JSON patch manifest (dde-toolkit style) describing the byte edits."""
    import hashlib
    import json
    changes = [{"what": "edit", "file_offset": f"0x{off:05X}", "length": len(o) // 2,
                "before": o, "after": a} for off, o, a in diff_report(orig, patched)]
    return json.dumps({
        "patch": name,
        "version": 1,
        "description": description,
        "ecu": "Bosch EDC15M / BMW DDE3.0 (M47), prog 7786887 / cal 7789659",
        "address_mapping": "file offset in the 256 KB full read; CPU address = file offset",
        "input": {"path": os.path.basename(src),
                  "sha256": hashlib.sha256(orig).hexdigest(), "size": len(orig)},
        "output": {"path": os.path.basename(dst),
                   "sha256": hashlib.sha256(patched).hexdigest(), "size": len(patched)},
        "checksum": "already-corrected",
        "checksum_note": "code 0x13FFC, cal 0x3FBFC and program chain 0x38B7C are valid in the output image",
        "hardware_tested": hardware_tested,
        "changes": changes,
    }, indent=2)
