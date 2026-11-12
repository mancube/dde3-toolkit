# EDC15M / DDE3.0 flash patches

Patch builders for the Bosch EDC15M (BMW E46 318d, M47, prog 7786887 /
cal 7789659). Each builder reads `./e46_orig.bin`, applies a small, documented
set of byte edits, **corrects all three checksums**, and writes a flashable
image plus a JSON manifest.

The same manifests are what `dde3-toolkit patches` (CLI) and the **Patches**
GUI tab apply — see `../docs/patches.md`. Nothing here is sent to the ECU:
patch the image, then write it from the Write flash tab.

> Off-road / bench use. Flashing a wrong image can brick the ECU. Keep
> `e46_orig.bin` as the pristine restore. Test on a bench ECU first.

## The three checksums (why a builder is needed)

An edit anywhere in the program area changes one or more of:

1. **TDI code block** — value at `0x13FFC` (covers `0x10000–0x13FFB`).
2. **TDI calibration** — value at `0x3FBFC` (covers `0x38B80–0x3FBFF`).
3. **Cold-start program chain** — balancing word at `0x38B7C`; the boot self-test
   hashes pages 4–0xD (`0x10000–0x37FFF`) + `0x38000–0x38B7F` and resets (code 9)
   unless the result matches the seed `h=0x8631 g=0xEFCD`.

Any edit in `0x10000–0x38B7F` changes the program chain **even when it is outside
the two TDI slots** — the common trap that makes a hand-patched image boot into a
reset loop. `edc15m_patch.fix_checksums()` corrects 1→2→3 in order (1 is inside
3's range, so order matters; the three fields do not overlap, so it is not
circular). It calls the verified reference tools vendored in `tools/`
(`edc15m_checksum.py`, `progsum.py`).

## Builders

| builder | output | what it does |
|---|---|---|
| `patch_baud.py --rate 125000` | `e46_baud125000.bin` | adds DS2 service 0x91; K-line → 124800 baud (S0BG 0x03) on command |
| `patch_baud.py --rate 38400` | `e46_baud38400.bin` | same, → 38400 baud (S0BG 0x0C) |
| `patch_immo_off.py` (`--mode gate`) | `e46_immo_off_gate.bin` | EWS delete: one 2-byte edit so fuel is never EWS-cut (recommended) |
| `patch_immo_off.py --mode full` | `e46_immo_off_full.bin` | EWS delete: forces the EWS result/release flags (mirrors the user's `e46_immo_off.bin`, but with a valid program checksum) |

Run from this folder, e.g.:

```sh
python3 patch_baud.py --rate 125000
python3 patch_immo_off.py --mode gate
```

Each run prints the exact byte edits, the three checksum values, and writes
`<image>.bin` + `<image>.json`. The manifest name is rate/mode specific
(`edc15m_baud_0x91_125000`, `edc15m_immo_off_gate`, …) so variants do not
collide in the toolkit's patch list.

## Apply / verify with the toolkit

```sh
dde3-toolkit patches list --image e46_orig.bin
dde3-toolkit patches apply edc15m_baud_0x91_125000 --image e46_orig.bin --out /tmp/out.bin
dde3-toolkit patches verify edc15m_baud_0x91_125000 --image /tmp/out.bin
dde3-toolkit checksum /tmp/out.bin          # code, calibration, program chain
```

The Rust applier reproduces these four shipped images byte-for-byte from
`e46_orig.bin` (test `patches::tests::builtin_patches_reproduce_the_shipped_images`).

Verify with the Python tools directly:

```sh
python3 tools/edc15m_checksum.py -c e46_baud125000.bin   # TDI code+cal
python3 tools/progsum.py e46_baud125000.bin              # program chain
```

Both must say OK. (All four shipped images pass.)

## Baud patch — wire use

The patch keeps 9600 for the initial handshake and switches on command:

```
tester @9600 : 12 08 91 00 <baud 24-bit BE> 03     (125000 = 01 E8 48; 38400 = 00 96 00)
               the ACK is queued AFTER S0BG flips, so it leaves at the NEW rate
reopen port  : 124800 (for the 125000 image) or 38400
               drain, then IDENT (12 04 00) to confirm the link
revert       : power cycle (boot re-inits 9600)
```

In `dde3-toolkit`: flash the image, then `dde3-toolkit baud --rate 125000`.
See `../docs/FINDINGS_flash_and_fast_ds2.md`.

## Immo-off (EWS) patch — what it changes

The EWS state publishes a release byte at RAM `C3A0` (0x00 ok / 0xFF locked) and
a flag `C3B7.0`; the fuel path zeroes injection at **0x2A652–0x2A670** when
locked.

- **gate** (default): one edit at `0x2A65E` (`3d03`→`0d06`) so that path always
  uses the real quantity — injection is never EWS-cut. EWS DTCs still report
  truthfully. Minimal and surgical.
- **full**: forces the EWS result/release flags (`C136`→0xFF at 0x300ED/0x30161/
  0x308BD, `C13C` read→#01 at 0x303BE/0x31164) so every consumer sees "matched".
  Equivalent to the supplied `e46_immo_off.bin` — which, note, has an **invalid
  program checksum** (its author bypassed the check instead of correcting it);
  this builder produces the same behaviour with all checksums valid.

Pick **gate** unless a downstream EWS consumer besides the fuel cut matters.

## Files

| file | |
|---|---|
| `edc15m_patch.py` | shared library: edit apply, 3-checksum fix, diff, manifest |
| `patch_baud.py` | baud (0x91) builder |
| `patch_immo_off.py` | EWS-delete builder |
| `tools/edc15m_checksum.py`, `tools/progsum.py` | vendored verified reference tools |
| `e46_orig.bin` | pristine 256 KB reference input |
| `e46_baud38400.bin` / `e46_baud125000.bin` (+ `.json`) | prebuilt baud images |
| `e46_immo_off_gate.bin` / `e46_immo_off_full.bin` (+ `.json`) | prebuilt immo images |
