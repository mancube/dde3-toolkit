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
| `docs/ds2-rx-buffer.md` | Firmware-confirmed K-line RX buffer & frame limits: 128-byte app receiver (`0x1EB34`), the two-receiver split (why 233-byte MPPS frames work), the unconfirmable "256" (boot block all `0xFF`) |
| `docs/dtc-fault-protocol.md` | 4-step fault-read protocol, 50-byte fault record layout, DTC text tables in `DDE30DS0.prg` |
| `docs/checksum.md` | TDI V3.1 checksum algorithm, block discovery, the false-BAD fix, the cold-start program chain |
| `docs/baud-0x91-patch.md` | DS2 0x91 baud-change ECU patch + `baud` command |
| `docs/patches.md` | Manifest-driven firmware patches (CLI `patches` + GUI tab), builders, prebuilt images |
| `docs/reverse-engineering.md` | .prg format, the 33 jobs, WinkFP VM status, DDE4 parallels, job re-scope |
| `docs/uprog-ein.md` | UPROG_EIN (`0x0D`): the handler decompilation, the 72-byte region/segment table, what it's used for (PRUEFCODE, programming-mode gate, tester region map) |
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

### Session 21 (2026-11-18) — Windows 7: missing COMBASE.DLL after delay-load fix

After v0.3.6 (delay-load of `KERNEL32.dll` only), the rebuilt Windows release
now progressed past the kernel32 error and failed at startup with a missing
`combase.dll` error; user asked to make the exe fully static.

Fix: extended the release's delay-load set to the full native GUI layer
(kernel32, user32, gdi32, ole32, combase, comdlg32, imm32, shell32, shlwapi,
advapi32, setupapi, cfgmgr32, uxtheme, dwmapi, opengl32, bcryptprimitives) via
`/DELAYLOAD:*` flags + `/DEFAULTLIB:delayimp`, verified with
`cargo xwin build --release --target x86_64-pc-windows-msvc`.

Released **v0.3.7** (pending: bump + run). Next: run the v0.3.7 Windows release
on the Win7 host and confirm startup (then continue MPPS fake-ECU testing).

### Session 22 (2026-11-18) — Windows release under Wine (prefix /home/ilija/Bottles/Programs/)

User asked to test the Windows release under Wine using prefix
`/home/ilija/Bottles/Programs/` (after noting it may not run under wine at all).

Run: `WINEPREFIX=/home/ilija/Bottles/Programs/ wine dist/dde3-toolkit-windows-x86_64.exe`
Result: reproducible, exit 1. With `WINEDEBUG=+loaddll`, kernel32/user32/gdi32/
combase/ole32 all load as wine builtins with **no missing-DLL errors** — the
v0.3.7 delay-load set is loading cleanly under wine. Process then died with
`virtual:virtual_setup_exception stack overflow` (wine-internal crash; likely
GUI/GL-related under wine, not a missing DLL).

Conclusion: the missing-import startup blockers appear resolved at the loader
level; full GUI startup validation still pending (bare-metal Win7 host test
remains the real acceptance check).

### Session 23 (2026-11-19) — AIF/hardware-ID read serves flash content again

User finding (MPPS via `respond --flash ../../captures/actual_flash.bin`):
the `--flash` override of the FLASH_LESEN reply did **not** work — the VIN
was no longer reported, the reply was missing the ECU type (`Bosch DDE3`),
the hardware ID and the VIN the older version produced, so the tool did not
pass the read/write buttons gate.

Root cause: session 19 (commit `95066b5`, v0.3.5) replaced the AIF/hardware-ID
read content (`FLASH_LESEN addr=0x000000, count=46`) with a hard-coded ASCII
identification string (`IDENT_STRING`, `12 2E A0 <42> <XOR>`) — so the
`--flash <file>` serve never reached the AIF read again and MPPS saw the
built-in string instead of the real car's AIF record.

Fix (`src/sim.rs` `flash_read`):
- the AIF read again serves the flash image's AIF slot
  (`self.flash[AIF_SLOT..AIF_SLOT + AIF_LEN]`, `AIF_SLOT = 0x3FCB2` = cal
  `0x7CB2`), i.e. the real car's AIF record when running with
  `respond --flash captures/actual_flash.bin` — restoring the pre-v0.3.5
  behaviour where the actual flash read netted a properly-read VIN;
- `SimEcu::set_aif()` override still wins (content-matched tool tests keep
  working).
- Removed the now-unused `IDENT_STRING` const; renamed the regression test
  `flash_read_aif_serves_ident_string` → `flash_read_aif_serves_slot_content`
  and extended `flash_read_serves_file_image` to also assert the
  FLASH_LESEN-based AIF read serves the file's slot content.

Tests: 79 passing, clippy clean (2 pre-existing warnings). Next: re-run MPPS
against the fake ECU (`respond --flash captures/actual_flash.bin`) and
confirm the VIN read + read/write buttons gate pass.

### Session 24 (2026-11-19) — fault memory now single-error, clear → 0 errors

User finding (fresh `respond` run, MPPS via `respond --flash
../../captures/actual_flash.bin`):

- after a restart the **read-faults job works** again (count/entry/shadow/
  tail replies all byte-correct, LEN = payload + XOR, XOR ok), but the
  error it reports is **broken / malformed** (decodes to code `0x95` —
  FArt “Redundante Schubueberwachung”). and the **clear** does not take
  correctly.
- “it is not an MPPS fault — the original job tool reports as such: 1 error
  and it is malformed”. So the defect is in the **ECU-side (fake) reply
  content**, not in MPPS’s parsing.

Root cause / redo (using the ECU decompilation + job decompilation as the
source, `edc15m/decompiled diagnostic command jobs via serial.txt`:
`FS_QUICK_LESEN @ 0x88B8`, `FS_LESEN @ 0x9935`, `FS_LOESCHEN @ 0x1354E`,
`FS_SHADOW_LESEN @ 0x136B0`):

- the FS reply *shapes* were already right (verified frame-by-frame on the
  fresh `respond` log: `12 1B A0 <anz> <rec 0..22>`, `12 09 A0 <pos> <rec
  22..26>`, `12 10 A0 <pos> <rec 26..37>`, tail `12 12/28 A0 …`);
- the *content* was the problem: `SimEcu::default_faults()` seeded **three**
  placeholder fault records, so the read came back with 3 errors instead of
  the single one the tools / the real car expect.

Fix (`src/sim.rs`):

- `SimEcu::default_faults()` now returns **exactly one** well-formed 50-byte
  fault record (ORT `0x26` Luftmassemesser, mask `0x02`, freq `1`, freeze
  u16 BE `0x012C` / `0x0096`) — the FS_LESEN sequence (count/entry/shadow/tail)
  walks it cleanly and terminates (`A0 <anz=0>`) on its own.
- The DS2 clear (`12 04 05` → `12 04 A0`) already resets the fault state;
  with the single-error memory the post-clear read now reports `A0 <anz=0>`
  (0 errors).
- Tests updated: `sim::tests::fault_memory_sequence`,
  `sim::tests::fault_replay_assembles_full_records` (single-fault
  expectations; all other sim/protocol tests untouched).

Tests: 79 passing, clippy clean (2 pre-existing warnings).

### Session 25 (2026-11-19) — fault memory serves the REAL M47 error record

User finding (fresh `respond` run after session 24): the read-faults job
now reports **1 error** (`12 1B A0 01 26 02 …` → anzahl `0x01`) but the
error is still broken — ORT `0x5F` → “unbekannter Fehlerort”, error
frequency `101`, Errorcode `5F 62 65 68 …` (the old placeholder record’s
bytes).

User correction: “it is not an MPPS fault — the original job tool reports
as such: 1 error and it is malformed”. Source of truth: `analysis/actual
error.txt` (INPA on the real car) + `protocol.rs::dtc_from_record_reference_m47`.

Fix (`src/sim.rs` `default_faults()`): serve the **real M47 reference
fault record** verbatim instead of the synthesized placeholder:

```text
08 C8 3C 00 38 9C E3 00 37 9D DE C4 C1 04 00 48 … 31 C5 00 00 22
```

(ORT `0x08` Gluehanlage, ART mask `0xC8`, freq `60`, Errorcode `08 C8 3C …`).
The synthesized `SimEcu::record()` helper was removed as it is no longer
used.

### Session 26 (2026-11-20) — full flash read now runs the Galletto session bootstrap

User finding: the **actual read-flash procedure of the toolkit did not match
the way Galletto read the full flash** — see the `galletto capture with
respond tool` (`testread.txt`: IDENT → UPROG_EIN → AIF read → SEED_KEY →
2048 × `FLASH_LESEN` @128 B, contiguous). The toolkit's read instead sent the
`FLASH_LESEN` chunk loop directly.

Fix (`src/protocol.rs`):

- new `Kline::begin_flash_session()` — full flasher session before the
  first `FLASH_LESEN` chunk:
  `IDENT` (`12 04 00`) → `UPROG_EIN` (`12 04 0D`) → AIF read (`12 05 02 00`)
  → SEED_KEY (seed `12 08 90 42 4D 57 Z` + key via new
  `protocol::key_from_seed_reply`, unbiased job sum) →
  `FLASH_LESEN` chunks;
- `Kline::read_flash` (shared by `read_full` / `read_range`) now runs the
  bootstrap first — GUI, CLI `read`/`read-cal` and the `--write` read-back
  compare all go through it.
- new pure helpers + tests: `frame_aif_read`, `key_from_seed_reply`
  (unbiased vectors: Z=0x07 → `39 3C 3F 42`, Z=0x10 → `87 8A 8D 90`,
  Z=0x16 → `BB BE C1 C4`).

Tests: 81 passing, clippy clean (2 pre-existing `app.rs` doc-list warnings
only). Next: bench-verified the `--sim` full read end-to-end (bootstrap +
chunk loop) and re-run the MPPS/Galletto fake-ECU read session.

### Session 27 (2026-11-20) — full flash write now runs the same flasher session bootstrap

Transfer of the session-26 fix to the **write** path (“mimic the Galletto
logs”): `Kline::write_full` / `Kline::write_calibration` previously sent a bare
`UPROG_EIN` (`12 04 0D`) before erase → write → end. Galletto’s write session
(`fakewritegalleto.txt`) runs the full session first (IDENT → UPROG_EIN → AIF →
SEED_KEY) and only then the flash-write family (FLASH_LOESCHEN → FLASH_SCHREIBEN →
FLASH_SCHREIBEN_ENDE).

- Renamed `Kline::begin_flash_read_session` → `Kline::begin_flash_session`
  (the flasher session bootstrap; now shared by the read *and* write paths);
- `write_full` / `write_calibration` now run `begin_flash_session()` before
  erase instead of the bare `UPROG_EIN` (docs/comments updated accordingly).
- Write-side docs: `docs/FINDINGS_flash_and_fast_ds2.md` §2 note extended to
  the write path.

Tests: 81 passing, clippy clean (2 pre-existing warnings). Next: bench-verify
  the `--sim` full write end-to-end (bootstrap → erase → 3072 × 64-B writes →
  end) and re-run the Galletto fakewrite fake-ECU session.

### Session 28 (2026-11-20) — SEED_KEY bootstrap fixed: LEN-validated FrameParser + echo-wait retransmit

The session-26/27 bootstrap (`begin_flash_session`) failed on the wire:
the toolkit logged `RX 12 13 14` (3 bytes) as the SEED reply, computed a
key from the garbage (`3C 3B 3A 39` instead of `39 3C 3F 42`), and the key
TX collided with the real 46-byte reply still in flight (the fake ECU's
log shows the merged RAW run `… 27 08 CA E7 F8 90 3C 3B 3A 39 8E`).

**Root cause — proven by running the real parser on the real byte stream**
(a temporary test fed the exact wire frames byte-by-byte): the old
`FrameParser::take_frame` completed frames by *shortest XOR-terminated
prefix* plus a 0x12 **anchor scan inside the buffer**. The SEED reply
`12 2E A0 00 01 02 … 29 9D` is a linear ramp, and the in-band run
`12 13 14 15` satisfies `0x12^0x13^0x14 == 0x15` — a self-terminating
"frame". At 9600 baud the 46-byte reply takes ~49 ms to arrive, so
`take_frame` always saw it mid-stream; the anchor scan locked onto the
**data byte** 0x12 (the ramp's own 0x12) and emitted `12 13 14` as a
complete 3-byte frame. `do_request` accepted it (payload[2] = 0x14 is not
a known status → "data reply"), the key was computed from 3 bytes, and the
early key TX hit the still-arriving real reply on the half-duplex line.
Longer frames (IDENT 46 B, AIF 50 B) only survived because their data
never happens to self-terminate. The same bug class had already been fixed
in `capture.rs`'s `Framer` (73-byte `FLASH_SCHREIBEN` truncated at byte 21)
— the active-path `FrameParser` had never been updated.

Fixes (`src/protocol.rs`):

- **`FrameParser::take_frame` rewritten — no 0x12 anchor scan.** A frame is
  `12 <LEN> <job> <data…> <XOR>` where **LEN is the total wire length
  INCLUDING the trailing XOR** (verified on every frame in
  `captures/dde3newlog.txt`: IDENT `12 2E …` = 46 wire, AIF `12 32 …` = 50,
  flash write `12 49 …` = 73; the AIF/seed/flash-read replies all check
  out). A frame is emitted **only when all LEN bytes are present AND the
  trailing XOR validates** — a partial frame is never emitted, no matter
  what its bytes look like. Leading non-0x12 bytes are dropped one at a
  time (collision debris); an implausible LEN or a failed trailing XOR
  drops one byte and resyncs. The legacy "raw reply = whole buffer" case
  was removed (both the real ECU and the fake ECU envelope every reply;
  the in-process sim bypasses the parser).
- **`do_request`: transmit → wait for echo → retransmit.** The echo of our
  own TX (reflected by the monitor tap) must be on the line by
  `echo_timeout` (5 ms/byte of frame, min 250 ms — the per-byte pacing
  dominates at 9600). Missing echo ⇒ the request never reached the bus ⇒
  resend (up to `MAX_TX_RETRIES = 3`), instead of waiting out the full
  reply timeout and bailing. No-echo resends are safe even for
  `request_once` frames (no echo ⇒ the ECU never received the frame; a
  reply would have arrived as a frame). BUSY resends keep their
  `retry_on_busy` gate and `MAX_BUSY_RETRIES` cap. TX log lines now show
  the retry reason: `(BUSY retry n)` / `(no-echo retry n)`.
- **Progress bar (GUI):** new `Msg::BatterySnapshot` — the pre-job battery
  snapshot inside a read/write job now only refreshes the voltage display;
  it no longer clears `busy` / `active_job` (the bar vanished the moment
  the snapshot landed). The battery *button* job still uses `Msg::Battery`.
- **Live-broadcast collision:** the flash service frames
  (`read_flash` chunk loop, `erase_flash`, `write_block`, `write_end`)
  now go through `request_filtered(…, is_not_live_broadcast)` so the
  ECU's periodic 16-byte live block can no longer be mistaken for the
  service reply (same pattern the DTC/battery reads already used).

Tests: 84 passing (3 new parser regression tests: the 46-byte SEED reply
byte-by-byte — nothing emitted before all 46 bytes; leading-garbage drop;
bad-XOR resync). Clippy: only the 4 pre-existing warnings.

**End-to-end verified:** full 256 KB `read` against the fake ECU
(`respond --flash captures/actual_flash.bin --reply-delay 0`) — bootstrap
(IDENT → UPROG_EIN → AIF → SEED_KEY → key) passes clean, the 2048-chunk
`FLASH_LESEN` loop runs without truncation or collision.

### Session 29 (2026-11-21) — respond `--dump`: 192 KB test write exported as full 256 KB image

User request: the test-write dumps from the `respond` tool — when the peer
wrote the 192 KB span 0x10000..0x40000 — must be padded with 0xFF from
0x0 to 0x10000 on export (full 256 KB image, boot area = erased flash), so
the dump lines up with a full flash read.

- new pure `respond::dump_image(flash, lo, hi)`: span `(0x10000, 0x40000)`
  → full 256 KB image with `[0..0x10000]` filled 0xFF, written span
  verbatim; any other span stays compact (first-to-last written byte),
  unchanged.
- the run-end dump in `respond::run` uses it; the log line says
  `(0x0000..0x40000, 0x0000..0x10000 padded FF)` when padding applied.
- tests: `dump_image_pads_the_192kb_test_write_with_ff`,
  `dump_image_keeps_other_spans_compact` (86 passing).

### Session 30 (2026-11-21) — write path now mimics Galletto's two-session flow

User pointed out the inconsistency: Galletto's full write is **two
re-authenticated sessions** (see `captures/galletto capture with respond
tool/testwrite.txt` @ ~line 7725): program area 0x10000..0x38000, then
`DIAGNOSE_ENDE` (`12 04 9F`), then IDENT/UPROG_EIN/AIF/SEED_KEY again, erase
@0x38000, calibration area 0x38000..0x40000, `DIAGNOSE_ENDE`. The toolkit's
`write_full` had done everything in one session with a single erase and a
`07 0F` write-end (which Galletto never sends).

- `Kline::write_full` restructured: two phases
  `(0x10000,0x38000)` then `(0x38000,0x40000)`, each = `begin_flash_session`
  → per-area erase → **64-byte** blocks (was 128; 64 is the only
  real-ECU-proven size) → `DIAGNOSE_ENDE` via `request_filtered(
  is_not_live_broadcast)`. `write_end`/`07 0F` removed from the write flow.
  Progress stays continuous over the full 192 KB.
- `Kline::write_calibration` = Galletto's phase 2 on its own (64-byte blocks,
  ends with `DIAGNOSE_ENDE`).
- New sim test `two_phase_write_reauth_like_galletto`: proves the fake ECU
  NACKs a data write after `DIAGNOSE_ENDE` until UPROG_EIN + seed/key are
  repeated (87 passing).
- E2E verified on a pty loopback (socat pair): `bench --write
  captures/actual_flash.bin` vs `respond --flash` — respond log shows 2×
  UPROG_EIN/SEED_KEY/LOESCHEN/DIAGNOSE_ENDE, exactly 3072 writes
  (2560+512), no `07 0F`; reconstructed dump byte-identical to the source
  image over 0x10000..0x40000 (boot area FF-padded per session 29's rule).
- Docs: FINDINGS §2.1 rewritten (two-session flow, per-phase frame counts),
  README feature line updated.

### Session 31 (2026-11-18) — per-zone `07 0F` write-end rejected (decompilation gap)

User request: end each zone write (program area, calibration area) with
`FLASH_SCHREIBEN_ENDE` (`07 0F`) before `DIAGNOSE_ENDE` — "an addition like
a failsafe" — under the rule **"don't implement a command if it's not fully
understood from the job decompilation"**.

Checked `before/10DDE30.prg`, job `FLASH_SCHREIBEN_ENDE` @0xD782
(`analysis/winkfp_proto.txt`, run with `tools/prg_vm.py`):
- understood: frame skeleton `12 09 07 0F <A:3 BE> 00` + XOR, reply =
  JOBRESULT status at [2]; zero-param VM run → `12 09 07 0F 00 00 00 00`
  (matches `analysis/frames.md`);
- **address value NOT determined**: the job copies [4..6] from the
  caller-supplied BIN-buffer parameter; the .prg never computes an address;
  parameterized VM runs don't show the clean BE-address layout the read job
  proves; no real PC-software capture shows the value (Galletto never sends
  `07 0F` at all);
- **timing NOT in the .prg**: 33 leaf jobs, no orchestrator, no job jumps
  to 0xD782 — the call order is in the PC software's C code.

Decision: keep Galletto's flow (zones end with `DIAGNOSE_ENDE` only). The
uncommitted `write_end()`-per-zone attempt (working tree on top of
`d584a92`) was reverted — kept in `git stash` for reference. FINDINGS §2.1
updated with the full analysis. Re-evaluate only when a real
MPPS/PC-software capture shows the address + timing.

### Session 32 (2026-11-18) — experimental 192-byte flash blocks + global LINK side-nav

User request: an experimental 192-byte block mode for flash read **and**
write (full + calibration) — "closer to the theoretical block size limit the
ECU's flash write code will accept" (246 B/frame) — behind a stricter gate,
plus the link/timing toggles moved out of individual tabs into one
program-wide place. Default OFF everywhere.

- `src/protocol.rs`: `WRITE_BLOCK_EXP` / `READ_CHUNK_EXP` (=192) next to the
  known-safe `WRITE_BLOCK` (64) / `READ_CHUNK` (128); `Kline` carries
  `write_block`/`read_chunk` with `set_experimental_blocks(on)`;
  `read_full`/`read_range` and `write_full`/`write_calibration` use the
  configured sizes. The frame builder already allowed ≤246 B.
- Gating (two independent gates on writes): CLI `--experimental` + typing
  **WRITE192** (not WRITE); GUI "192 B blocks (EXP)" toggle + WRITE192 in the
  write dialog.
- GUI side nav now has a program-wide **LINK · GLOBAL** section: 192 B blocks
  (EXP), faster 9600, fast write, fast baud 125k — the faster-9600/fast-baud
  toggles moved off the Live tab, fast write off the Write tab. The battery
  voltage pill moved from the Read/Write tabs to the top bar beside the
  CONNECTED status.
- Side-nav layout: the tab list scrolls on its own (30 px buttons, own
  ScrollArea); LINK/IMAGE/ECU+version are pinned at the bottom, always
  visible — the pinned height is measured each frame with an invisible
  `UiBuilder::invisible()` sizing pass (`pinned_sections`), so nothing clips
  at any window size.
- CLI dispatch fix: `--experimental` was in the GUI-flag list, so
  `dde3-toolkit write x --experimental` launched the GUI. GUI detection is
  now "no args, or first arg starts with `-`" (CLI commands are first).
- Tests: `protocol::tests::experimental_block_sizes`,
  `sim::tests::experimental_192_byte_blocks` (89 passing).
- Docs: `docs/wire-protocol.md` §32, README (`--experimental`, GUI section).
- **On-car verdict (same day): rejected** — see session 33.

### Session 33 (2026-11-18) — 192-byte block mode removed after on-car failure

On-car `write --experimental`: the first two 192-byte `FLASH_SCHREIBEN`
frames (`12 C9 07 02 01 00 00 C0 …` @0x10000, `… 01 00 C0 C0` @0x100C0)
each drew a 4-byte write-OKAY (`12 04 A0 …`, status 0xA0 — not the 16-byte
`12 10 A0 10 …` live broadcast); the third (`… 01 01 80 C0` @0x10180) got
no reply and no echo — after 3 resends the line was dead. The frame format
was wire-valid (LEN 0xC9 = 9+192 fits one byte); the ECU's flash-write path
cannot sustain 192-byte blocks (two "OKAY"s, then a hung ECU).

Removed entirely (user call):
- `protocol.rs`: `WRITE_BLOCK_EXP`/`READ_CHUNK_EXP`, the
  `write_block`/`read_chunk` fields, `set_experimental_blocks`,
  `experimental_block_sizes` test — gone; read/write use the fixed
  64 B / 128 B constants again.
- CLI: `--experimental` flag, the `WRITE192` word (back to `WRITE`), bench
  field — gone.
- GUI: `experimental_blocks` toggle + banner + `WRITE192` gate — gone.
  The LINK · GLOBAL side-nav section stays (faster 9600 / fast write /
  fast baud), as do the battery pill in the top bar, the scrolling tab
  list with pinned bottom sections, and the first-arg CLI/GUI dispatch rule.
- Sim: `experimental_192_byte_blocks` test — gone (87 passing).
- `docs/wire-protocol.md` §32 now records the on-car failure; 64 B / 128 B
  are the only verified block sizes. Don't re-attempt larger write blocks
  without a capture proving the ECU's flasher accepts them.

### Session 34 (2026-11-18) — full per-run log file (GUI)

The in-app Protocol log panel keeps only the most recent 2000 lines
(`push_log` drains 500 past 2000), so a long session's early frames were
lost from the on-screen record. Now **every GUI launch also writes a full,
untruncated log to a temp file**, named after the startup time:

- Path: `<log dir>/dde3-YYYYmmdd-HHMMSS.log` (one file per run;
  `open_run_log` in `app.rs` creates the dir + file in `App::new`).
  The log dir is configurable, highest priority first: `--log-dir <path>`
  (GUI flag) > `$DDE3_LOG_DIR` (env, same convention as `DDE3_PATCH_DIR`)
  > the persistent default `default_log_dir()` =
  `$XDG_DATA_HOME/dde3-toolkit/logs` or `~/.local/share/dde3-toolkit/logs`
  (replaces the original `$TMPDIR` location — /tmp is wiped on reboot).
- Every `push_log` line (all wire TX/RX + operation messages) is appended
  with a wall-clock `YYYY-MM-DD HH:MM:SS.mmm` prefix, **flushed per line**
  (a crash mid-write still leaves a complete log). Original `[+x.xxxs]`
  relative timing is kept after the prefix.
- Header block: version, start time, mode (demo/K-line). `on_exit` appends
  a `run ended` line on a clean window close (not on SIGTERM).
- The path is printed to stderr at startup and shown in the Log tab header
  (muted, monospace, next to the `N lines` label).
- `now_local()` (app.rs) formats the clock via `libc::localtime_r` — no new
  dependency. If the temp file can't be opened, the run continues without it
  (a stderr note is printed); logging never blocks or aborts an operation.

Verified end-to-end against the fake ECU over a socat pty pair
(`respond --flash` on one end, GUI `--autoconnect --auto-live` on the other):
199 lines captured, 194 of them TX/RX frames, byte-for-byte the full session.
87 tests passing; build warning-free.

### Session 35 (2026-11-18) — `respond --patched-baud`: fake ECU mirrors the 0x91 baud patch

The fake ECU can now behave like the **patched real ECU**, so the toolkit's
own fast-baud path is testable end-to-end without a car (branch
`feat/respond-baud-91`):

- `respond --patched-baud <38400|125000>` (only the two shipped patch rates
  are accepted): the `0x91` is accepted, the port reopens at the patch's
  fixed rate **before** the ACK is transmitted (125000 → 124800, the S0BG
  divisor — the same `reopen_divisor` mapping the tester side uses), so the
  ACK rides the line at the **new** rate, exactly like the real patch
  (S0BG write, then ACK). The frame's baud field is ignored (v1 patches are
  single-rate). The link stays at the new rate until `respond` is restarted
  (= power cycle). Without the flag the legacy stub ACK stays, so `--sim`
  fast-baud runs are unchanged.
- Sim hooks: `SimEcu::set_baud_patch(rate)` + `take_baud_switch()` (pending
  switch, consumed once); `respond` applies the port reopen +
  `clear(ClearBuffer::All)`, logs `BAUD old -> new` and prints "link left at
  …" at exit.
- E2E over a socat pty pair: `baud --rate 125000` / `38400` (switch + IDENT
  verify), `bench --fast-baud 125000 --live` (~89 Hz vs ~2.5 Hz at 9600),
  full 256 KB `--read` byte-exact vs the served image (24 s with
  `--data-delay 0`), full `--write` → `--dump` reconstructs the exact source
  span (boot area 0x0..0x10000 FF-padded, by design).
- Tests: 89 passing (+2: `sim::tests::baud_patch_91_switches_link_and_acks`,
  `baud_91_without_patch_keeps_stub_ack`). Docs: `docs/respond-mode.md` §9,
  `docs/baud-0x91-patch.md` (fake-ECU test rig).

### Session 36 (2026-11-18) — fast-baud write/read pacing fixed (bench)

User report: the full write with the 125k patch active was **slower than at
9600**. Root cause: the bench Write/Read arms passed the default
`--data-delay 3` (ms/byte — a 9600-baud WinKFP habit) straight to
`write_full` / the read loop even at 124800. A 73-byte `FLASH_SCHREIBEN`
frame took 219 ms of pacing vs 5.9 ms of wire → 3072 frames ≈ 11.5 min,
the 13× wire speedup fully masked. The Live loop and the GUI write job
already zeroed pacing at fast baud (`if fast { 0 }`); bench was the outlier.

Fix (`src/bench.rs`): when the 0x91 switch succeeds, `set_write_delay(0)` is
applied for the whole run (covers the read path + bootstrap frames) and the
Write/WriteCal arms pass the effective delay (0 at fast baud) to
`write_full` / `write_calibration`, which re-set it from the argument.
`WRITE_START` now logs the effective delay.

Verified over the pty loopback with **default** flags (no `--data-delay`):
full write 36.1 s (was ~11.5 min), dump byte-exact over the written span;
full read 24.3 s, byte-exact. 89 tests passing, clippy clean.
Docs: `docs/FINDINGS_flash_and_fast_ds2.md` §5 (pacing rule), bench usage
line.

### Session 37 (2026-11-18) — GUI full write at 125k still paced (fixed)

User report: the GUI full write was still slow with the link at 125000.
Root cause: the GUI write jobs keyed pacing off the `fast_write` checkbox
only (`let fast = self.fast_write;`) — the "fast write" toggle is a 9600-baud
inter-byte-pause switch, unrelated to the 0x91 link state. With the link at
124800 but the checkbox unticked, the write ran 3 ms/byte → ~11.5 min.
The GUI read jobs didn't touch pacing at all (inherited the 3 ms default).

Fix (`src/app.rs`): all four job starters now compute the effective pacing
flag before spawning the worker — `start_write` / `start_write_cal`:
`fast = fast_write || fast_baud_active.is_some()`; `start_read` /
`start_read_cal`: `fast = fast_baud_active.is_some()` with
`set_write_delay(if fast { 0 } else { 3 })` around the read (reset after,
mirroring the write jobs). `fast_baud_active` is set by `Msg::FastBaud`
(CLI `--fast-baud` on connect or the Live-tab button) and cleared on
failure / link loss / reconnect, so it is the authoritative "link is fast"
state. The live loop already did this (unchanged).

Not E2E'd through the egui window (needs a display + UI driving); the
worker calls the identical `write_full`/`read_full` with pacing 0 that the
bench pty E2E proved in Session 36 (36.1 s write / 24.3 s read, byte-exact).
89 tests passing, clippy clean. FINDINGS §5 pacing note updated.

### Session 38 (2026-11-18) — live Hz counter: producer-side 1 s window

User report: the live-logging Hz counter read unrealistically high at fast
baud, and at 9600 it overshot then took a long time to settle to the real
~8–9 Hz. Root cause: the rate was measured on the **UI thread** —
`dt = now - live_last` with `now` taken when the message was *processed*,
not when the cycle *completed*. egui drains the channel in frame-sized
bursts (~60 fps): at fast baud (100+ Hz producer rate) several `Msg::Live`
land in one frame, microseconds apart → `dt ≈ 0` → `hz = 1/dt` in the
hundreds of thousands, and the 0.8/0.2 EMA (first sample taken as-is,
`live_last` initialized at app construction) pinned the display high and
decayed spikes over ~15 samples.

Fix (`src/app.rs`): the rate is now measured in the **live thread**, where
the cycle time is real. `live_rate_update()` keeps a 1 s sliding window of
cycle-completion stamps and returns the true mean rate — (n−1) intervals /
window span; 0.0 until two samples. `Msg::Live` carries the Hz
(`Live(Vec<(u16,u16)>, f32)`); the UI just displays it (keeps the previous
value for the 0.0 first sample). `live_last` removed. Being a plain window
average it cannot overshoot, and it shows the real number after a few
samples (exact from sample 2 at steady cycle time; fast-start anomalies age
out of the window within 1 s).

Tests: 91 passing (+2: `app::tests::live_rate_window_is_the_true_mean`,
`live_rate_window_ages_out_fast_start`). Clippy clean.

### Session 39 (2026-11-21) — fake ECU parity with the 2026-11-19 real-car capture

Analyzed the full real-car run log (`captures/dde3-20261119-222226.log`,
323 TX/RX pairs) with the new `tools/analyze_run_log.py` (TX/RX pairing,
LEN/XOR validation of every frame, command classification, reply-shape
summary, `--image` flash verification). All 323 RX frames validate. Full
write-up: `docs/SESSION_2026-11-19.md`.

Findings → fixes (`src/sim.rs`):
- UPROG_EIN (`0x0D`) → `12 4C A0` + **72-byte region/segment table** — not
  stored in flash (firmware assembles it; the tail 24 B are the flash
  region table at 0x3FC9A..0x3FCB2). Sim now serves the captured bytes
  verbatim (`UPROG_EIN_DATA`).
- Seed telegram = the **ident string verbatim** (42 B) — so Z = 0x07 gives
  key `9C A0 97 96`, exactly like the real car (was a Z-seeded pseudo
  pattern). Key-check OK = **`12 05 A0 00`** (status + one 0x00 byte), sets
  a new `unlocked` latch (cleared by DIAGNOSE_ENDE `0x9F`).
- Flash services (0x06 read/erase, 0x07 write) gated on
  `in_prog && unlocked` (`flash_session_open()`); outside the session they
  answer the **`12 04 A2` NACK stub** — proven by the 3-command capture
  `captures/actual ecu UPROG_EIN response.txt` (UPROG_EIN done, no
  seed/key → FLASH_LESEN → `12 04 A2`).
- 0x0A (`12 05 A0 00`) and IDENT already matched — unchanged. **DTC
  handlers untouched on purpose** — the toolkit's DTC parse/clear
  algorithm is known-wrong and stays as-is (separate effort).

Tests: 92 passing (new `uprog_ein_and_seed_key_real_car_parity` +
`unlock()` helper; 8 sim tests updated for the new shapes/gating).
E2E: `respond` on a socat pty pair + `read-cal` — the full bootstrap
(IDENT → UPROG_EIN `12 4C A0 …` → AIF → seed/key → `12 05 A0 00`) then
256 × 128 B reads, 32768 bytes written, exit 0. The `--sim` flag is
outdated — `respond` is the only real sim now (candidate for removal).

### Session 40 (2026-11-20) — old `--sim` flag and break-mode removed

User: remove the outdated `--sim` flag + its unused code, and remove
break-mode **altogether** — DS2 is the only protocol the ECU can actually
use. `respond` (the only real sim) must remain fully functional.

- **`--sim` gone**: the flag in `main.rs` + all 11 `set_sim` injection
  blocks (incl. the sim-only read-back verify in `write`/`write-cal`), the
  `BenchArgs.sim` field, and the in-process sim plumbing in `protocol.rs`
  (`sim` field, `set_sim()`, the reply injection in `do_request`,
  `verify_echo()`). `Kline` is now purely wire-based; the GUI `--demo`
  (fake UI state, no `SimEcu`) is untouched.
- **Break-mode gone**: `src/breakmode.rs` (813 lines, 12 tests), the
  `break-probe` command, the `--fast` flag (break-probe-only),
  `Kline::raw_write`/`raw_break` (break-only; `raw_read` **kept** —
  `switch_fast_baud` uses it to drain the 0x91 ACK at the new rate),
  `docs/break-mode.md`.
- **Docs**: README (feature bullet, quick start now shows the `respond`
  pty-pair workflow, layout, doc table, known limits), AGENTS.md, HANDOVER
  (doc index + open item #3 closed), `docs/simulator.md`,
  `docs/respond-mode.md`, `docs/wire-protocol.md`, `docs/session-log.md`,
  `sim.rs` module doc (now "the state machine behind `respond`").
- `src/sim.rs` behaviour unchanged — still the full DS2 state machine.
  Tests: 80 passing (was 92; the 12 removed were breakmode's).
- **`respond` E2E verified untouched**: socat pty pair +
  `read-cal --flash captures/actual_flash.bin` — full bootstrap (IDENT →
  UPROG_EIN `12 4C A0 …` → AIF → SEED_KEY), 256 × 128 B reads, 32768
  bytes written, **byte-exact** vs `actual_flash.bin` over the calibration
  region, exit 0.

### Session 41 (2026-11-22) — MPPS-style calibration write (224-B blocks, `07 0F`, DTC prep)

User request: the calibration write should mimic **MPPS** (100% on-car
success, per the user) instead of the Galletto flow. New branch
`feature/mpps-style-calibration-write`.

- **224-byte calibration blocks** (`CAL_WRITE_BLOCK = 0xE0`): 146×224 + 1×64
  = 32768 (the 32 KB calibration area). Distinct from §32's rejected
  192-byte *program-area* blocks — the MPPS capture
  (`captures/mpps capture/mppswrite.txt`) proves 224-byte *calibration-area*
  blocks. `write_full` keeps 64-byte blocks.
- **`07 0F` write-end** (`frame_flash_write_end(addr)`): sent **once** before
  the erase, at **0x03FCE0** (MPPS sends it twice; the address was the
  session-31 open question, now resolved by the capture).
- **DTC read+clear prep** (`read_and_clear_dtcs()`): `12 05 04 00` (FS_LESEN
  entry) + `12 04 05` (FS_LOESCHEN), sent **once** via a new `dtc_prep` flag
  on `begin_flash_session` — runs before reads (`read_full`/`read_range`) and
  writes (`write_full` first phase, `write_calibration`), matching MPPS.
- `DIAGNOSE_ENDE` kept (harmless; MPPS omits it, Galletto sends it).
- Tests: `flash_write_end_matches_mpps`, `cal_write_block_covers_32kb`,
  `sim::tests::mpps_style_cal_write_preamble` (83 passing).
- **E2E** (socat pty pair, `bench --write-cal` vs `respond`): 147 FLASH_SCHREIBEN
  + 1 FLASH_SCHREIBEN_ENDE + 1 FS_LESEN + 1 FS_LOESCHEN + 1 DIAGNOSE_ENDE;
  the reconstructed 0x38000..0x40000 span is **byte-identical** to the source
  image.
- Docs: `docs/wire-protocol.md` §33, `docs/FINDINGS_mpps_vs_galletto_calibration.md`.

### Session 42 (2026-11-23) — FrameParser `MAX_BUF` 192 → 512 (the 233-B MPPS write echo)

On-car `write-cal` (session 41's feature) froze mid-write with
`no echo of service 0x07 (12 E9 07 02 03 84 60 E0 …) … line dead or ECU not
listening` after a few 224-byte blocks. Root cause: each 224-byte block is one
**233-byte** wire frame (LEN `0xE9`), but `FrameParser::MAX_BUF` was still 192
(sized in §21 for the ≤73-B Galletto frames). With a monitor tap, the ECU
reflects our own TX back, so the 233-B write frame hit the parser, overflowed
192, and the resync drained the front — destroying the `12 E9` header. The
parser re-anchored on a `0x12` *inside the data tail* (a phantom anchor that
never completes) and swallowed the ECU's reply → `got_echo` never set → 3
resends → the error. Data-dependent (only when the drained tail held a `0x12`),
so it froze at one specific block, not every block.

- **Fix:** `MAX_BUF` 192 → **512** in `src/protocol.rs` (matches the
  `Framer` in `src/capture.rs`, already 512). Drain heuristic unchanged.
- **Regression test:** `protocol::tests::parser_holds_full_mpps_write_echo`
  (feeds the full 233-B frame + a trailing reply; fails on 192, passes on 512).
- **E2E proof:** faithful K-line T-topology tap (two raw ptys; bridge forwards
  each TX to the peer *and* taps it back to the sender's reader — no pty
  `ECHO`, so no feedback loop) + `respond` + `bench --write-cal` on an image
  with a `12 FC` trap in block 5's tail: **old code** → the exact `no echo`
  error; **fixed** → all 147 blocks, dump byte-identical.
- Docs: `docs/wire-protocol.md` §34 (+ §33 caveat that the socat E2E had no
tap echo). `cargo test` → 84 passing.

### Session 43 (2026-11-23) — first real-car cal write: K-line sleep/wake found, pacing set to MPPS speed

First full real-car calibration write (32 KB, MPPS-style flow). The write
**succeeded** (car started afterwards) but only after two failed bootstrap
IDENTs and a manual reconnect; the write also ran ~2.5× slower than MPPS.
Analyzed with the new `tools/analyze_realcar_write.py` (run-log + MPPS
reference parser; `--lines N` to bound the log, `--mpps <file>` for the
cadence comparison).

- **Root cause of the two dropped IDENTs:** the ECU's K-line module sleeps
  after ~20 s of line silence and ignores plain data frames while asleep;
  only a line-level pulse (break) wakes it. Attempts 1 & 2 (23.3 s and
  12.8 s of idle) were dropped *with echo* (frames reached the bus, ECU
  silent); the user's reconnect (port close+open → line pulse) woke it, and
  attempt 3 + the whole write ran clean. "faster 9600" was a red herring
  (live-loop only). → `Kline::wake_kline()` (break + settle + drain) and a
  one-shot IDENT retry after wake in `begin_flash_session`; `identify()`
  timeout 500 → 1000 ms (real ECU answers ident in 473–516 ms).
- **Pacing:** the write ran at the old 3 ms/byte → 830 ms/frame vs MPPS's
  327 ms (130 s vs ~48 s for 32 KB). At 9600 a byte is 1.04 ms on the wire,
  so host pacing below that is wire-invisible — `start_write` /
  `start_write_cal` now pass `data_delay_ms = 0`, `write_frame` has a
  bulk-write fast path, and the now-redundant `fast write` checkbox /
  `--fast-write` flag are removed.
- Docs: `docs/wire-protocol.md` §35 (timeline, mechanism, measured latencies
  for timeout sizing). `cargo test` → 84 passing.
- **On-car re-verification pending:** one more `write-cal` with the new
  code — expect ~330–350 ms/frame (32 KB in ~50 s) and, if the ECU is
  asleep, a logged `IDENT no reply (…) — K-line wake break, retrying` line
  followed by a successful bootstrap.

### Session 44 (2026-11-23) — signed 16-bit live/logger values decoded correctly

User finding: the value logger showed negative parameters as huge positives —
SB injection start setpoint/actual read **655.34** instead of e.g. **-0.02**
(Grad KW). Root cause: every ADF value was decoded as unsigned
(`raw as f64 * factor + offset`), but the ECU reports some ADFs as
two's-complement i16.

- **Signedness source:** the tool32 ADF export's `DATA_TYPE` column —
  **7 = signed i16, 5 = unsigned u16** (22 of the 81 variables are type 7;
  exactly the negative-capable set: SB injection start setpoint/actual,
  SBR PI output, acceleration, MSR intervention, M_E correction/quantity
  deltas, friction quantity). Cross-checked against the user's on-car
  observation (0x0F52/0x0F53 are type 7).
- `LiveVar` gains `signed: bool` + `LiveVar::decode(raw)` (sign-extend,
  then `raw * factor + offset`); `tools/gen_live_rs.py` emits both from the
  CSV and `src/live.rs` was regenerated (also emits the `mod tests` hook so
  regeneration can't drop it).
- Both decode sites use it: the live display update (`Msg::Live`) and the
  CSV logger (`LogWriter::write_row`) — the logger tab, live tab, charts
  and CSV all flow from those two points.
- Tests: `live::tests` — the 655.34 → -0.02 regression case, unsigned
  full-range kept (WTF 3000 → 26.86 °C, 0xFFFF stays positive),
  monotonicity over each variable's domain, and the 22-id signed set pinned
  against the ADF export (88 passing, clippy clean).

The branch was merged to `main` via PR #2 and shipped as **v0.5**
(`scripts/release.sh 0.5`, tag `v0.5`, Gitea release with the linux binary
+ .sha256).

### Session 45 (2026-11-23) — live/logger presets + Live view modes (branch `feature/live-presets-views`)

User request (on a dedicated branch, main untouched): presets for the
variable selections — remember the last session, plus named presets
(create / rename / delete) — and more Live-data display options beyond the
single graph.

Design (user decisions, asked via questionnaire):
- **Presets are variable sets only**, shared by both tabs. The **Value
  logger** keeps its own selection (labels with changing values, unchanged
  behaviour); the **Live** tab has its own selection **and** a view-mode
  setting, and can adopt the same presets for parameter selection. The two
  selections are **separate** (previously one shared `live_enabled` vec —
  now `live_enabled` + `logger_enabled`).
- **Four Live view modes** (toolbar buttons, persisted): **Line** (the old
  graph card), **Labels** (plain name/value/unit rows, two columns on wide
  windows), **Gauges** (in-car style 240° arc gauges, auto-scaled over the
  recent history window, needle + value), **Cards** (the original
  sparkline cards — the old default look).
- Storage: `~/.config/dde3-toolkit/presets.json` (`$DDE3_CONFIG` >
  `$XDG_CONFIG_HOME/dde3-toolkit` > `~/.config/...`), one file with the
  named presets + both selections + view mode, stored as **ADF ids**
  (stable across `live.rs` regeneration), saved on every change
  (selection, view mode, preset ops). Load is tolerant: unknown ids
  dropped, corrupt file → defaults.

Implementation:
- New `src/presets.rs` (`Preset`, `Settings`, `LiveView`,
  `load`/`save`, `ids_to_enabled`/`enabled_to_ids` + 5 tests).
- `src/app.rs`: `PickerTarget {Logger, Live}` replaces the single
  `show_var_picker` bool — the picker window is now per-tab ("Logger
  variables" / "Live variables") with a preset row (combo + Apply /
  Save as… / Rename… / Delete, inline name dialog, duplicate names
  refused); `show_graph` → `live_view: LiveView`; new `labels_card` /
  `gauges_grid` / `gauge_card` / `arc_points` renderers (egui 0.31 has no
  arc primitive — arcs are polylines, ~5°/segment); `start_live` polls the
  **union** of both selections while logging (the CSV writer filters to
  its own ids); `save_session()` persists on change; `apply_selection_change`
  keeps the old "changes apply after logging stops" rule.
- GUI smoke-tested via `--demo` (live tab, picker up, logger tab — no
  panics). Tests: 93 passing (was 88), clippy clean.

Open: on-desk visual pass of the four view modes + preset CRUD with the
user; merge to main when satisfied.

## ← CURRENT TASK (take over from here)

**MPPS-style calibration write (sessions 41–43) — on-car re-verification of
the session-43 fixes pending.** The calibration write mimics MPPS:
224-byte blocks (146×224 + 1×64), a single `07 0F` write-end @0x03FCE0
before the erase, and a one-shot DTC read+clear prep (also before reads).
It ran to completion on the real car (session 43, car started afterwards),
but at the old 3 ms/byte pacing and only after a manual reconnect fixed the
slept K-line. Session 43 added: unpaced (0 ms/byte) data frames — MPPS
parity, expect ~330–350 ms/frame — and the self-healing bootstrap
(`wake_kline()` break + IDENT retry). Next on-car: `dde3-toolkit write-cal
<cal.bin> --port /dev/ttyUSB1`; confirm the cadence in the log
(`tools/analyze_realcar_write.py`) and that a cold ECU bootstraps on the
retry. The 224-B size is proven by the MPPS capture (wire-protocol §33);
§32's 192-B rejection was the *program* area, a different case.

**Closed (session 33): 192-byte block mode.** Tried on the car; the ECU
OKAYed two 192-byte write blocks, then hung on the third (no reply, no
echo, line dead) — the mode was removed. 64 B writes / 128 B reads are the
only verified sizes (wire-protocol §32).

**Real-car verification of the session-28 parser fix and the session-30
two-session write flow.** The LEN-validated `FrameParser` + echo-wait
retransmit were verified end-to-end against the fake ECU (`respond --flash`),
and the two-session write (program → DIAGNOSE_ENDE → re-auth → calibration)
was verified against the fake ECU via a pty loopback — neither has been
verified against the real car yet. Next on-car:

1. `dde3-toolkit read /tmp/full.bin --port /dev/ttyUSB1` — the bootstrap
   (IDENT → UPROG_EIN → AIF → SEED_KEY) must pass on the first try (it used
   to need spamming), and the 256 KB chunk loop must run clean.
2. **Write verification (destructive — do it last, with a verified backup
   in hand):** `dde3-toolkit write <backup>.bin --port /dev/ttyUSB1` must
   show the two phases in the log: program area 0x10000..0x38000,
   DIAGNOSE_ENDE, re-auth (second UPROG_EIN + SEED_KEY), erase @0x38000,
   calibration 0x38000..0x40000, DIAGNOSE_ENDE — 3072 × 64-byte writes, no
   `07 0F`.
3. If the real ECU ever replies with a frame whose LEN disagrees with the
   trailing-XOR position, the log will show a `no response` bail — the
   parser drops one byte and resyncs, so look for repeated 1-byte drops in
   the RX log before changing anything.

Note (session 31): the requested per-zone `FLASH_SCHREIBEN_ENDE` (`07 0F`)
failsafe was **not** implemented — the job decompilation doesn't determine
the address field or the call timing (FINDINGS §2.1). Rule: don't implement
a command the decompilation doesn't fully explain. If a real MPPS/PC-
software capture ever shows a `07 0F` frame, that capture resolves both
open questions.

**Fake-ECU FS jobs: serve the real M47 error record; clear empties memory.**
Done in session 25 (`src/sim.rs` `default_faults()` → real M47 reference
fault record; FS_LESEN sequence + clear re-verified via
`fault_memory_sequence` / `fault_replay_assembles_full_records`).
Still open from session 23/25:

1. Re-run MPPS against the fake ECU (`respond --flash
   ../../captures/actual_flash.bin`) and confirm the VIN read + read/write
   buttons gate pass.
2. On-car: confirm the single-error memory + clear against the real M47.

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
3. ~~**Break-mode T2.1 capture**~~ — **removed** (session 40): the
   break-entered service is not a protocol the ECU can actually use; DS2 is
   the only protocol. `breakmode.rs`, the `break-probe` command and
   `docs/break-mode.md` are gone.
4. ~~**0x91 patch bench test**~~ — the fake-ECU half is done (session 35:
   `respond --patched-baud` mirrors the patch, `baud` / `bench --fast-baud`
   verified end-to-end over a pty loopback). On-car half remains: flash
   `e46_baud125000.bin`, `baud --rate 125000`, power-cycle revert check
   (`docs/baud-0x91-patch.md`).
