# DDE3 Toolkit — Handover

> For the agent taking over. Read this file, then `docs/wire-protocol.md`
> (on-the-wire facts) before touching the protocol code.

## Mission

Build a **DDE3 (BMW EDC15 DDE3.0, "M47") K-line diagnostic tool in Rust**
mirroring the `dde4-toolkit` feature set: full/partial flash read & write,
live data, protocol logging, fault read/clear. The protocol was
reverse-engineered from `10DDE30.prg` (WinkFP bytecode) with
`tools/prg_vm.py`. Note: **EDC15M and DDE3 are the same ECU** (Bosch EDC15M
chip, DDE 3.0 hardware name).

## Documentation index

| File | Contents |
|---|---|
| `docs/wire-protocol.md` | **Authoritative** on-the-wire facts: framing, reply convention (`0x12` prefix, not `0x92`), XOR checksum, timing, ECU identity |
| `docs/dtc-fault-protocol.md` | 4-step fault-read protocol, 50-byte fault record layout, DTC text tables in `DDE30DS0.prg` |
| `docs/checksum.md` | TDI V3.1 checksum algorithm, block discovery, the false-BAD fix, the cold-start program chain |
| `docs/break-mode.md` | Break-entered service protocol (10.4 k / 38.4 k) + decoded command set |
| `docs/baud-0x91-patch.md` | DS2 0x91 baud-change ECU patch + `baud` command |
| `docs/patches.md` | Manifest-driven firmware patches (CLI `patches` + GUI tab), builders, prebuilt images |
| `docs/reverse-engineering.md` | .prg format, the 33 jobs, WinkFP VM status, DDE4 parallels, job re-scope |
| `docs/session-log.md` | Historical status snapshots (superseded content) |
| `docs/FINDINGS_*.md`, `docs/SESSION_2026-11-09.md`, `docs/handoff-fast-baud.md` | Earlier finding write-ups |

## Current status (2026-11-11)

- ✅ Wire framing confirmed by hardware capture (no B8 envelope:
  `<DS2 payload> <XOR>`, 9600 8E1).
- ✅ Real-car reply convention fixed: ECU replies with prefix **`0x12`**,
  not `0x92` (5 new real-car fixture tests, `cargo test` → 48 passing).
- ✅ Fault read/clear, DTC tables (embedded in `DDE30DS0.prg`), checksum
  (TDI V3.1, byte-for-byte parity with the Python reference), 8-tab GUI,
  headless bench, in-process ECU sim.
- ⬜ Open: real-car verification of the items below.

## ← CURRENT TASK (take over from here)

**Real-car verification of the rewritten `read_dtcs()` (session 12).**
`read_dtcs()` was rewritten for the 4-step chunked fault protocol
(`docs/dtc-fault-protocol.md`); the old version mis-parsed the ORT field.
One real-car confirmation is still pending. Steps:

1. `dde3-toolkit dtc --port /dev/ttyUSB1` → expect **"Gluehanlage"** (ORT
   0x08) + ART texts "Gluehkerzen bzw. Gluehrelais defekt" /
   "Fehler momentan vorhanden" / "Fehler sporadisch" — **not**
   "Mengensteller".
2. Check the wire log (`captures/dde3newlog.txt`) for the **step-4 tail-count reply
   shape** (`12 05 14 01` again → `<anz'> <rec 37..50> [<next header>]`).
   If it differs from the table in `docs/dtc-fault-protocol.md`, adjust only
   the tail slice in `read_dtcs()` — the rest of the pipeline is
   data-driven.

### Fast-logger status (2026-11-11)

Tier 0 of `docs/FINDINGS_faster_logger.md` (back-to-back polling, 1 ms/byte
TX pacing) was **flaky in the car** — fault reads errored and live/logger
returned nothing while it was the only timing. The link now defaults to the
**original WinKFP timing (55 ms poll gap, 3 ms/byte)**; the Tier-0 timing is
opt-in and can be A/B tested in-car (note: the fault-read path is paced the
same in both modes — only the live loop's poll gap/pacing change):
- GUI: **"faster 9600"** switch on the Live tab — flips live, applies from the
  next poll. Off again on disconnect.
- CLI: `bench --live --fast-9600` (default = the 55 ms / 3 ms timing).

Live TX pacing is 1 ms only in fast mode (0 ms at fast baud); the fast-baud
(0x91) button is separate and still requires the baud patch (untested).

Protocol log is timestamped (`[+Δs]` gap per line). Real-car baseline was
2.50 Hz (steady 399 ms/cycle, 5 ids) with ~250 ms unexplained — **next live
capture must read the TX→RX gap** to see whether the ECU itself is the
bottleneck, and to see what the ECU does when polled back-to-back.

### Session 13 (2026-11-12) — flash framing resolved, fast-DS2 patch corrected

New: `docs/FINDINGS_flash_and_fast_ds2.md` (authoritative). Key results:
- **DS2 dispatcher fully mapped** (disasm 0x2932C–0x293D0). Services 0x06/0x07
  are **NACK stubs (status 0xA2) in the normal session** — real flash read/write
  only works in programming mode (after `UPROG_EIN` `12 04 0D`), handled by the
  boot block (not in any dump).
- **Flash framing RESOLVED and FIXED** (was the "T3 contradiction"). Running the
  WinKFP job VM with an over-range count probe + the job bytecode prove: read =
  `12 09 06 00 <addr BE> <count:1>`, write = `12 <9+N> 07 02 <addr BE> <N> <data>`,
  erase/end `07 06`/`07 0F <addr BE> 00`. The old `frame_flash_read` was
  little-endian, dropped the `[3]=00` byte and kept LEN=0x09 for a 7-byte payload,
  so the real car never replied (`captures/dde3newlog.txt`). `frame_flash_read/
  _write/_erase/_write_end` + the `sim.rs` decoders + tests are now corrected.
  Adversarially reviewed (4 dimensions) — compiles clean, no blockers.
- **Checksum-status reply captured**: `12 04 0A` → `12 05 A0 00`
  (`captures/dde3newlog.txt`). Open item #1 below is now closed.
- **Fast DS2 is real and patched.** `../../patches/patch_baud.py` adds DS2
  service 0x91 and switches S0BG to 38400 (0x0C) or **125000 (0x03 → 124800)**.
  The old `baud91_patch/` draft was wrong twice (missed the cold-start
  program-chain checksum → reset loop; used JMPA for a cross-segment jump →
  crash); the new builder fixes both and ships checksum-valid images
  `e46_baud38400.bin` / `e46_baud125000.bin`.
- **Toolkit:** `baud` takes `--rate 38400|125000` (reopens at the ECU's actual
  divisor, 125000→124800); `bench --fast-baud <rate> --live` is a headless fast
  logger; the GUI Live tab has a "Fast baud (125k)" toggle (link label + write
  pacing follow the active rate, state resets on disconnect/link-loss). All
  adversarially reviewed across compile/protocol/test/integration/egui
  dimensions — no blockers. Build on the host (`cargo build --release &&
  cargo test`; no Rust on the analysis machine).

NEXT (hardware): flash `e46_baud125000.bin` on a bench ECU, `dde3-toolkit baud
--rate 125000`, confirm IDENT at 124800; then the batched live loop logs at
~30–50 Hz (vs 2.5 Hz at 9600). Also capture one `UPROG_EIN`+FLASH_LESEN to pin
the flash read count-width before touching `frame_flash_read`.

### Session 14 (2026-11-12) — checksum module now covers the code-area program chain

`src/checksum.rs` previously corrected only the two TDI V3.1 slots. A third
code-area check — the **cold-start program chain** at `0x38B7C` — was Python
only (`progsum.py`), so a Rust-patched image (baud hook, EWS gate, any edit in
`0x10000-0x38B7F`) still reset-looped even with both TDI slots valid.

- Ported `progsum.py` (`prog_run`/`prog_solve`) into `checksum.rs`;
  `verify()` now returns a `program chain` row and `correct()` writes it
  **after** the TDI slots (the code slot `0x13FFC` is inside the chain range).
- Gated to full images that also have a real TDI code block, so a 32 KB
  calibration read padded with `0xFF` is never treated as a full image and the
  padded program area is never written.
- CLI `checksum` and the GUI Checksum tab pick the row up unchanged (it is a
  normal 4-byte slot for the Stored-edit/Apply/Fix path).
- Tests (62 passing): `progsum.py` cross-check vector (`fc520cf8`), ordering
  test with a stale code slot, padded-partial no-chain test, idempotency.
  Real images `e46_orig.bin` + all four patch images verify all-OK.

### Session 15 (2026-11-12) — firmware patches are part of the toolkit

Ported dde4-toolkit's manifest-driven patching to DDE3 and vendored the patch
bundle from `~/Desktop/edc15m/patches/` into `patches/` (self-contained):

- `src/patches.rs` — JSON manifests (`before`/`after` byte ranges, refused
  unless every range matches), built-in manifests via `include_str!`, extra
  manifests from `DDE3_PATCH_DIR` / `./patches` / `<exe>/patches`. Apply and
  revert call `checksum::correct`, so TDI code + calibration **and** the
  cold-start program chain are corrected.
- CLI: `patches list|show|apply|revert|verify` with `--image`/`--out`; prints
  the checksum report and refuses to write a bad image.
- GUI: new **Patches** tab (manifest list + state, changed ranges, apply /
  revert / open / reload / save, "Go to Write"); `--tab patches` opens it.
- `patches/`: 4 prebuilt images + manifests, `e46_orig.bin` reference, the
  three builders, and the verified reference tools vendored in `patches/tools/`
  so `python3 patches/patch_baud.py` works from a fresh clone.
- The two baud manifests were renamed by rate (`edc15m_baud_0x91_125000` /
  `_38400`; they shared one name before) and `patch_baud.py` now generates the
  rate-specific name.
- Tests (67 passing): every built-in manifest applied to `e46_orig.bin`
  reproduces its shipped `.bin` byte-for-byte, revert restores the reference,
  conflict/partial-image paths refused. Python builders reproduce the same
  four images from the vendored tools.
- Docs: `docs/patches.md`, rewritten `patches/README.md`, README feature/safety
  corrections (checksums *are* corrected on write; status reply *was* captured).

Session 15b — patch fit is content-based, not file-locked (user report: a
loaded image showed "target image differs" and a third-party EWS-off read was
refused as "partially applied"):
- The three checksum fields are tagged `"kind": "checksum"` and treated as
  **derived**: `state()` only looks at the semantic edits, so `available` /
  `applied` / `conflict` reflect the actual bytes. `input.sha256` is now the
  *reference read*, informational only, and never gates a patch — any read of
  the same calibration patches fine.
- `apply()` on an already-applied image repairs the checksums instead of
  returning early; new `patches correct --image` (and the GUI "Correct
  checksums" button) recomputes TDI slots + program chain with no manifest.
- Verified on the user's `e46_immo_off.bin` (7/7 edits applied, chain stale):
  it now reads `applied`, and repair changes exactly the 4 chain bytes at
  `0x38B7C` → all three checksums OK. Tests: 70 passing (+3).

### Session 16 (2026-11-12) — GUI image lifecycle + control polish

User report: in the Patches tab a loaded image could not be unloaded; general
UI/UX polish requested, no behaviour changes.

- New **Close image** control on the Write, Checksums and Patches tabs (and a
  loaded-image status strip: name, size/kind, checksum state). `unload_image()`
  clears image/name/partial/checks/comp-edit and is refused while `busy`
  (a worker may still hold the image). Nothing is sent to the ECU.
- Patches tab: **Open image…** added (it had no way to load an image), "Open
  patch…" renamed **Load manifest…**, and a "No image loaded" empty-state card
  with a CTA; the repeated per-manifest "load a 256 KB image first" detail is
  suppressed when no image is loaded.
- Write tab: the image name is now a read-only label (it was an editable text
  field that did nothing) plus Open/Close.
- Side nav shows the loaded image (short name, size, full/cal) while loaded.
- Tooltips and **disabled reasons** (`on_disabled_hover_text`) on Connect/
  Disconnect, Read, Erase+write, Write-cal, Apply/Revert/Correct, Copy/Clear,
  Open/Save — e.g. "Connect to the ECU first", "Confirm the backup and type
  WRITE first", "Already applied — use Revert".
- Shortcuts: **Ctrl+O** open image, **Ctrl+S** save image (ignored while a text
  field has focus). One `open_image_dialog()` entry point for all three call
  sites so they cannot drift.

### Remaining open items (after the above)

1. ~~Checksum hardware confirmation~~ — **done**: `12 04 0A` → `12 05 A0 00`
   captured 2026-11-10 (`captures/dde3newlog.txt`).
2. **MW live-data ADF ids** — the ADF table comes from the EDC15M export;
   ids to be confirmed on the real ECU (battery 0x0F65 already confirmed).
3. **Break-mode T2.1 capture** — `break-probe` + `tools/monitor.py` on
   `/dev/ttyUSB0` to confirm the on-wire 4-byte layout, then wire the
   byte-addressed reads into CLI/GUI (`docs/break-mode.md`).
4. **0x91 patch bench test** — flash `e46_baud91.bin`, `baud` subcommand,
   power-cycle revert check (`docs/baud-0x91-patch.md`).
