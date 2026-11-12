# Firmware patches (manifest-driven)

The toolkit applies firmware patches the same way `dde4-toolkit` does: a patch
is a **JSON manifest** with explicit `before`/`after` byte ranges, and the
image is only modified when every `before` range matches. Applying or
reverting then corrects **all** flash checksums — both TDI V3.1 slots and the
cold-start program chain (`docs/checksum.md` §5b).

## What decides whether a patch fits

Only the **semantic edits**. The three checksum fields
(`0x13FFC`, `0x3FBFC`, `0x38B7C`) are *derived*: their value depends on the
image's whole content, so they can never be used to decide whether a patch
applies. Manifests tag them `"kind": "checksum"` (the loader also recognises
the three offsets for older manifests) and they are excluded from the
`available` / `applied` / `conflict` decision.

Consequences:

- **Any read of the same calibration works**, not just the exact file a
  manifest was built from. The manifest's `input.sha256` is shown as the
  *reference read* and is **informational only** — it never allows or refuses
  a patch.
- An image that is **already patched but has a stale/bypassed checksum** (a
  common shape for third-party EWS-off images) reads as `applied`, and
  `apply` **repairs** the checksums instead of refusing. `patches correct
  --image` does the same without needing a manifest.
- A genuine conflict — an edited range that matches neither the original nor
  the patched bytes — is still refused.

Nothing here is sent to the ECU. Patch in memory → save the image → write it
from the Write flash tab (or `dde3-toolkit write <image.bin>`).

> Off-road / bench use. A wrong image can brick the ECU. `patches/e46_orig.bin`
> is the pristine restore; test on a bench ECU first.

## Layout

| Path | |
|---|---|
| `patches/*.json` | patch manifests (compiled in via `include_str!`) |
| `patches/*.bin` | prebuilt, checksum-valid images |
| `patches/e46_orig.bin` | pristine 256 KB reference the builders read |
| `patches/patch_baud.py` | DS2 `0x91` baud builder (38400 / 125000 → 124800) |
| `patches/patch_immo_off.py` | EWS delete builder (`gate` / `full`) |
| `patches/edc15m_patch.py` | shared edit + 3-checksum pipeline + manifest writer |
| `patches/tools/` | vendored reference tools (`edc15m_checksum.py`, `progsum.py`) |
| `src/patches.rs` | Rust loader/applier used by the CLI and GUI |

The four shipped manifests are **built in**, so the CLI/GUI work from a fresh
clone. Extra manifests are read from `DDE3_PATCH_DIR`, `./patches`,
`../patches` and `<exe dir>/patches`; a file with the same name as a built-in
manifest overrides it.

## CLI

```sh
dde3-toolkit patches list                      # manifests (state with --image)
dde3-toolkit patches list --image e46_orig.bin
dde3-toolkit patches show edc15m_baud_0x91_125000
dde3-toolkit patches apply <name> --image <in.bin> [--out <out.bin>]
dde3-toolkit patches revert <name> --image <in.bin> [--out <out.bin>]
dde3-toolkit patches verify <name> --image <in.bin>
```

`<name>` may be the exact name, a unique substring, or the 1-based index from
`list`. `apply`/`revert` print the checksum report and refuse to write an image
with a bad checksum; the default output is `<in>.applied.bin` /
`<in>.reverted.bin`.

`patches correct --image <in.bin> [--out <out.bin>]` recomputes the checksums
without applying anything — the repair path for an already-patched read whose
program chain was bypassed or left stale.

Exit status is non-zero on a conflict (neither the original nor the patched
bytes at a range), a bad checksum, or `verify` finding the patch not applied.

## GUI

**Patches** tab: manifest list with live state (`available` / `applied` /
`conflict`), the changed ranges with before/after bytes, **Apply to image**,
**Revert in image**, **Open patch…**, **Reload**, **Save image…**, and a
"Go to Write" shortcut. Launch straight into it with `--tab patches`.

## Shipped patches

| manifest | output | what it does |
|---|---|---|
| `edc15m_baud_0x91_125000` | `e46_baud125000.bin` | adds DS2 service `0x91`; K-line → 124800 (S0BG `0x03`) |
| `edc15m_baud_0x91_38400` | `e46_baud38400.bin` | same, → 38400 (S0BG `0x0C`) |
| `edc15m_immo_off_gate` | `e46_immo_off_gate.bin` | EWS delete, surgical fuel gate at `0x2A65E` (recommended) |
| `edc15m_immo_off_full` | `e46_immo_off_full.bin` | EWS delete, forces result/release flags |

After flashing a baud image: `dde3-toolkit baud --rate 125000` (reopens at the
ECU's actual 124800) or `--rate 38400`. Revert by power cycle. See
`docs/FINDINGS_flash_and_fast_ds2.md` and `patches/README.md`.

## Why a builder is still needed

The manifests describe byte edits; the checksums are the hard part. An edit
anywhere in `0x10000–0x38B7F` changes the **cold-start program chain**, even
when it is outside both TDI slots — the trap that makes a hand-patched image
boot into a reset loop. The Python builders (`patches/*.py`) and the Rust
applier both correct code `0x13FFC`, calibration `0x3FBFC` and the chain
`0x38B7C`, in that order.

Both sides are validated against each other: the Rust test
`builtin_patches_reproduce_the_shipped_images` applies every built-in manifest
to `e46_orig.bin` and asserts the result is byte-for-byte the shipped `.bin`,
and the Python builders reproduce the same four images from the vendored
`patches/tools/` reference implementations.

Repairing a third-party EWS-off image (already patched, stale program chain):

```sh
dde3-toolkit patches list --image e46_immo_off.bin     # → applied (not "conflict")
dde3-toolkit patches correct --image e46_immo_off.bin --out fixed.bin
dde3-toolkit checksum fixed.bin                        # all three OK
```

Only the 4 chain bytes change; the image's own (possibly bypass-related) code
edits and their code checksum are left as they are.

## Regenerating an image

```sh
cd patches
python3 patch_baud.py --rate 125000        # reads ./e46_orig.bin, writes ./e46_baud125000.bin + .json
python3 patch_immo_off.py --mode gate
```

The builder prints every byte edit, the three checksum values, and rewrites
the manifest (`output.sha256` changes, so re-run the Rust tests afterwards).
