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

## Latest (2026-11-17): Galletto-compatible fake ECU + flash-dump serving

- **Seed/key (`0x90`) fixed and wire-verified.** The `SEED_KEY_BIAS = 5`
  assumption is gone: the unbiased job formula
  `K[i] = resp[0x12+i] + resp[Z+i] + resp[0x29+i]` (byte adds, i = 0..3, on
  the final 45-byte seed payload) matches all three captured Galletto
  vectors — `Z=0x07 → 39 3C 3F 42`, `Z=0x10 → 82 85 88 8B`,
  `Z=0x16 → B6 B9 BC BF`. The "bias" was a capture misread (the key frame's
  last byte is the frame XOR). Galletto now unlocks on the first key send
  for every Z, and the full **flash READ** flow runs (1665 FLASH_LESEN
  frames, zero NACKs; see `captures/`).
- **`respond --flash <image.bin>`**: the fake ECU serves a real 256 KB dump
  verbatim — `FLASH_LESEN` (`12 09 06 …`) and the AIF read (`0x02`,
  slot 0x7CB2) return the file's bytes, so an external tool reads exactly
  the image you gave it (use `captures/actual_flash.bin`).
  `SimEcu::with_flash_image()` / `from_flash_file()` in `src/sim.rs`;
  tests `flash_read_serves_file_image`, `flash_image_wrong_size_rejected`.
- Docs: `docs/simulator.md` (seed/key section), `docs/respond-mode.md`
  (`--flash`), README `respond` section. Goal doc:
  `~/Desktop/edc15m/research/exe_seedkey_goal.md` (closed).

### Session 17 (2026-11-17) — full write now programs only `[0x10000, 0x40000)`

Confirmed against the Galletto logs (`captures/galletto capture with respond
tool/fakewritegalleto.txt` + `testread.txt`) and applied:

- Full write (`Kline::write_full`) now erases + writes blocks + write-ends at
  `FLASH_WRITE_START` = 0x10000, so it programs only `[0x10000, 0x40000)`
  (program area + calibration) — **not** below 0x10000 (boot block / gap).
  Previously it erased/wrote from 0x00000.
- Galletto fakewrite: erase @0x10000, 3072 × 64-B writes → reconstructed dump
  = 196608 bytes @ `0x10000..0x40000` (write.bin); toolkit now mirrors that.
- Full read verified **matching** Galletto (`testread.txt`: 2048 × FLASH_LESEN
  @128 B, contiguous from 0x0, no gaps, 256 KB) — unchanged, no edit needed.
- Cal write/read region (0x38000..0x40000) already matched.
- CLI `write` sim read-back compare now scoped to `[FLASH_WRITE_START, IMAGE_SIZE)`.

### Session 18 (2026-11-17) — missing DS2 responses implemented (MPPS follow-up)

MPPS against the fake ECU (`respond --dump writempps.bin …`) stalled right
after the DTC read/clear round-trip: services the tool uses still fell through
to the `12 04 FF` NACK (decomp handler map `0x26900`). Implemented in
`SimEcu::handle`:

- session keep `9E`, end `9F` — status only;
- test stamp read `0E` / write `0F` — status (+ 3 stamp bytes / store);
- actuator override `0C` — status only;
- EWS sync start `6C` / status `6D` — status only;
- adjustment values read `40` (status + u16 BE) / set `41` / store `42` — status;
- MW sub-services `03`/`04`/`11`/`12`/`07` (measurement block, digital inputs,
  PSG read/select, STEUERN_TESTPLATZ) — proper per-sub replies;
- flash-write-type `07 10` (STEUERN_TESTPLATZ) — status, accepted if unlocked.

Tests: `sim::tests::missing_ds2_responses_are_answered`,
`sim::tests::mw_subservices_answered`. Docs: `docs/simulator.md` (coverage
status), `docs/respond-mode.md` §8.

### Session 19 (2026-11-18) — AIF/hardware-ID read serves ASCII ident string

User finding (MPPS): the content returned by the FLASH_LESEN-based hardware-ID
read was "wrong / not enough" — surely missing the `0281001445` hardware-number
portion, and something over that for the ECU Type field. Adjusted the served
content in `SimEcu::handle` → `flash_read`:

- AIF read (`FLASH_LESEN addr=0x000000, count=46`) now serves the ASCII
  identification string (`12 2E A0 <42 char> <XOR>` → LEN = 0x2E = 46) via the
  new `IDENT_STRING` const (`77868875201124048010281001445G521000000000`,
  42 chars, carries hw `0281001445` + ECU type fields) instead of the raw AIF
  record bytes (`12 32 A0 <46 B> <XOR>`).
- Test hook `SimEcu::set_aif([u8; 46])` preserved (override wins) so content-
  matching tests remain possible; new tests `aif_override_serves_test_content`,
  `flash_read_aif_serves_ident_string`.
- Docs: `docs/respond-mode.md` §3 (flash read) + §7 files-touched updated.

Tests: 79 passing. Released **v0.3.5** (commit `a5d7280`, tag, Gitea) — the
Cargo.lock chore follow-up (`282ed31`) synced the version. Next: re-run MPPS
against the v0.3.5 fake ECU and confirm the stall/hang is resolved.

### Session 20 (2026-11-18) — Windows release startup fix (Win7 host)

User report: the Windows release (`dist/dde3-toolkit-windows-x86_64.exe`)
fails at startup with
`the procedure entry point GetSystemTimePreciseAsFileTime could not be located
in the dynamic link library KERNEL32.dll` — the host is **Windows 7** and the
release must work there too.

Diagnosis (PE import table of the shipped exe confirmed):
- The eframe 0.31 / winit 0.30 / windows-sys GUI stack imports
  `GetSystemTimePreciseAsFileTime` (Win8+) from `KERNEL32.dll`.
- Windows 7's `KERNEL32.dll` does not export that entry point → loader fails at
  startup.

Fix (v0.3.6):
- `scripts/release.sh` now cross-builds the Windows binary with
  `RUSTFLAGS="-C target-feature=+crt-static -C link-arg=/DELAYLOAD:KERNEL32.dll
  -C link-arg=/DEFAULTLIB:delayimp"` → the kernel32 import is delay-loaded
  (via `delayimp.lib`) so resolution is deferred and the binary starts on
  Win7. Verified locally: `cargo xwin build --release --target
  x86_64-pc-windows-msvc` now links cleanly with the delay-load flags.
- `README.md` "Windows build" section updated to the same RUSTFLAGS recipe.

Tests: 79 passing (unchanged code). Released **v0.3.6** (commit `5d84fcc`, tag,
Gitea). Next: run the v0.3.6 Windows release on the Win7 host and confirm it
starts (GUI should come up; MPPS/fake-ECU testing continues thereafter).

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
