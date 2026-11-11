# Session log (historical status)

Status snapshots and implementation notes from earlier sessions, kept for
history. Superseded by `docs/wire-protocol.md` (framing),
`docs/dtc-fault-protocol.md` (faults), `docs/checksum.md`, and
`docs/break-mode.md` where they overlap.

## 0. STATUS — updated 2026-11-09 (session 3)

| Task | State |
|------|-------|
| VM bugs fixed, `tools/test_uprog_ein.py` passes ALL | ✅ done |
| `analysis/frames.md` — non-param-dependent job frames | ✅ done (145 lines) |
| Parameter-dependent frames (FLASH_LESEN, BAUDRATEN_*) | 🔄 partial — FLASH_LESEN + BAUDRATEN_LESEN extracted with param-supplied runs |
| dde4-toolkit reference read (protocol/app/checksum/main/patches) | ✅ done |
| DDE3 wire-level framing (DS2 params from `xsetpar`, timing, baud switch) | ⬜ open |
| Rust scaffold | ⬜ not started |


## 0b. STATUS — updated 2026-11-09 (session 4) ← READ THIS

| Task | State |
|------|-------|
| Scope re-check (user directive) | ✅ done — plan narrowed, see §14 |
| DDE4 source .prg identified + parallels | ✅ done — see §15 |
| DDE3 param-frame extraction (FLASH_LESEN etc.) | ✅ done — `tools/extract_param_frames.py` written; runs logged in `analysis/frames.md` ("Parameterized runs") |
| DDE3 SEED_KEY flow | ✅ decoded from listing — see §15.3 |
| DDE3 wire framing | ✅ CONFIRMED by hardware capture 2026-11-09 — **no B8 prefix**: `<DS2 payload> <XOR>`; see §17 |
| Rust scaffold | ✅ done — `src/protocol.rs`, `src/checksum.rs`, `src/main.rs`, `src/app.rs`; 10 unit tests passing; GUI with 7 tabs |

**Scope decision (user, session 4):** do NOT chase all 33 jobs. The toolkit must
only clone the dde4-toolkit feature set: **full read, partial (calibration)
read/write, protocol logging, live data, fault read/clear.** Everything else
(baud jobs, AIF/ZIF/FS/ADAPTION, sync-mode control, reference tables) is DROPPED
unless needed to make the essential flow work. Detail in §14.

**Essential DDE3 jobs (kept):** UPROG_EIN, SEED_KEY, FLASH_LESEN,
FLASH_LOESCHEN, FLASH_SCHREIBEN(+ENDE), DATENBEREICH_LOESCHEN_0E (fault clear),
STATUS_LESEN / STATUS_SYNC_MODE (live data/status — unresolved, §14),
STATUS_CODIER_CHECKSUMME, IDENT, DIAGNOSE_ENDE, INITIALISIERUNG/EDIC_RESET
(connect).


## 13. First session plan (superseded by §0b/§14 — kept for history)

1. Fix VM bugs → 2. extract all frames → 3. decode DS2 wiring →
4. Rust scaffold → 5. flash read/write → 6. live/faults/log/tests.


## 16. Rust implementation status (session 6)

### What was built

| File | Lines | Role |
|---|---|---|
| `Cargo.toml` | — | manifest (serialport, anyhow, hex, eframe/egui, rfd, crossbeam-channel) |
| `src/main.rs` | ~330 | CLI subcommands + `--gui` default launching egui window |
| `src/protocol.rs` | ~700 | frame codec (`B8 12 F1 LEN payload XOR`), `FrameParser`, `Kline` serial connection, all 12 fixed frames + 4 parameterized builders + read_full/read_range/write_full/write_calibration/live_values, `JobStatus` enum from JOBRESULT table |
| `src/checksum.rs` | ~300 | **VERIFIED** TDI V3.1 hash/verify/correct ported line-for-line from `edc15m_checksum.py` — content-based discovery (id at 0xB7A + slot scan): calibration 0x3FBFC / 0x7BFC, code 0x13FFC; byte-for-byte parity with the Python tool on real reads |
| `src/app.rs` | ~1950 | egui GUI: Overview, Read flash, Write flash, Checksums, Live data, Value logger, Protocol log tabs |
| `README.md` | — | build/run instructions, protocol constants, frame catalog, safety notes |

### Wire framing hypothesis (in `src/protocol.rs`)

The DDE3 xsetpar block `{06 00 80 25 12 00 00 00 00 00 E8 03 32 00 14 00 03 00}`
differs from DDE4's `{06 00 80 25 B8 00 00 00 00 00 E8 03 20 00 32 00 04 00}`
at position 4: DDE3 carries ECU addr 0x12 where DDE4 carries FMT 0xB8. The
working hypothesis is that DDE3 uses the same `B8 12 F1 LEN payload XOR` wire
format with the DS2 payload (starting with 0x12) as the frame payload. **This
must be confirmed with a real hardware capture.**

### CLI commands

```
ident          Connect + 12 04 00 ident
read <out>     Full 256 KB flash read (128-byte chunks via 12 09 06)
read-cal <out> Calibration area read (0x30000-0x3FFFF)
write <img>    Erase + write full flash (WRITE confirmation required)
write-cal <img> Erase + write calibration area only
seed-key <Z>   12 08 90 42 4D 57 05 <Z>
fault-clear    12 09 07 0E 42 41 50 00 ("BAP")
checksum <img> Verify/correct TDI V3.1 checksums (32 KB or 256 KB; auto-corrects in place, write path needs hardware confirmation)
uprog-ein      12 04 0D enter programming mode
diagnose-ende  12 04 9F leave session
```

Options: `--port`, `--baud`, `--data-delay`.

### Tests

`cargo test` — 8 tests passing:
- `protocol::frame_round_trip` — build/parse round-trip
- `protocol::parser_skips_garbage_and_resyncs` — parser resync after bad checksum
- `protocol::fixed_frames_match_vm_extraction` — all fixed frames match frames.md
- `protocol::parameterized_frames_match_vm_extraction` — param frames match frames.md
- `protocol::jobstatus_lookup` — JOBRESULT table lookup
- `checksum::selftest_calibration_vector` — matches Python regression vector
- `checksum::selftest_code_vector` — matches Python regression vector
- `checksum::correct_makes_correctable_blocks_valid` — idempotent correction
- `checksum::rol_ror_are_inverses` — bit rotation correctness
- `checksum::find_checksums_returns_blocks` — block discovery

### Resolved items

- **Checksum algorithm**: Ported the verified Python `edc15m_checksum.py` TDI V3.1 algorithm to Rust. Self-tested against regression vectors (`0x007BC5EB` calibration, `0x2477887F` code).
- **Live data**: DDE3 uses service `0x0B` (MW/Messwert) via `MW_SELECT_LESEN` (`12 05 0B 10 <id_hi> <id_lo>`). Measurement table extracted from `tool32_edc15m_export.csv` (from `decompiled.txt`) with IDs, scaling factors (FACT_A/FACT_B), and units.
- **Fault clear**: `DATENBEREICH_LOESCHEN_0E` (`12 09 07 0E 42 41 50 00`) implemented.
- **GUI**: All 7 tabs implemented (Overview, Read, Write, Checksums, Live, Logger, Log).

### Still unresolved

- **Fault code READ**: No dedicated fault-read job in the DDE3 .prg. STATUS_DIGITAL/STATUS_MW* jobs exist for status monitoring but no DTC read.
- **Wire framing**: Needs real K-line capture to confirm `B8 12 F1 LEN payload XOR`.

### What's next

See §10. Immediate blocker: wire framing needs a real K-line capture before
any flash write is attempted on hardware.


## 10. Remaining work (PRIORITIZED — do in this order)

> Rust scaffold is done (§16). Remaining: hardware validation of wire framing,
> checksum verification, GUI, live data / fault read.

1. **Wire framing hardware validation** — capture real DDE3 K-line traffic to
   confirm `B8 12 F1 LEN <DS2 payload> XOR` at 9600 8E1 and decode the
   xsetpar timing bytes (§15.1/§15.2).
2. **Seed/key hardware confirmation** — verify whether a separate key telegram
   precedes FLASH writes (§15.3).
3. **Fault code READ** — no dedicated job exists; determine if any STATUS_*
   job can be used for DTC reading, or document as clear-only.
4. **Hardware validation** — validate checksum, live data, and write paths
   against real ECU.
5. **More unit tests + protocol log enhancements.**


## 11. Open questions / unknowns (do not guess — resolve from sources)

- Exact on-wire DS2 frame for DDE3 (B8 header family confirmed, payload
  padding/length rule not — §15.2/§15.4).
- DDE3 default baud/bit format at connect (DS2 typical 10.5 kbit vs DDE4's
  9600 8E1).
- Fault-code READ: no dedicated job; DDE4 toolkit has none either.
- Live-data service: STATUS_LESEN vs STATUS_SYNC_MODE (`12 04 6D`) — decide
  from listing deep-dive or hardware.
- Whether UPROG_EIN's long response carries a seed (see §15.3).


### 18.3 GUI: 8th tab "Fault codes"

- Fault cards: ORT class + FArt text + freeze values (scaled), shadow row
  collapsible, battery-voltage display (ADF 0x0F65, scale 0.02037243/LSB).
- Armed two-step clear button (Clear → Confirm), mirroring dde4.
- Post-write modal (ported from dde4): after a successful write the user
  power-cycles, then `start_post_write_clear()` waits 5 s, clears the DS2
  fault memory (with 5 s link-recovery retry) and re-reads the fault list.
- Pre-write battery snapshot logged to the protocol log.
- Battery pill (`battery_pill`) on the Read/Write tabs (critical/low/OK
  colouring, DDE3 scale), and a voltage snapshot on connect + before
  every read/write (dde4 parity).
- GUI automation flags ported from dde4: `--autoconnect`, `--auto-read`,
  `--auto-cal`, `--auto-live`, `--auto-log <path>`, `--auto-write-cal`,
  `--auto-write`, `--fast-write`, `--show-vars`, `--tab <name>`,
  `--width/--height`, `--demo` (fake ECU mode). After connect, first
  match wins: write > write-cal > read > cal > log > live.
- Not ported (deliberately): `patches` tab/state (no DDE3 patches),
  `freeze_frames`/`dtc_read_at` (DDE3 stores raw u16 per DTC and scales
  on the fly instead of pre-reading per-DTC freeze frames).
- Error handling now matches dde4: `catch_unwind` worker guard,
  `lock_or_recover`, `is_link_error` classification, `recover_link` after
  connect errors, fatal-link-loss handling in the live loop.


### 18.4 Bench runner (headless, parity with dde4 `bench.rs`)

`src/bench.rs` + `bench` subcommand: `--info | --read | --read-cal |
--write | --write-cal`, `--image`, `--out`, PROGRESS/WRITE_DONE/ELAPSED on
stderr, protocol-log dump on failure, link-recovery check after write.
`write_full`/`write_calibration` now enter programming mode (UPROG_EIN)
before erase (matches the decompiled job order; the sim NACKs without it).

Measured against the in-process sim (9600 8E1 + 3 ms/byte pacing):
read 256 KB ≈ 98 s, read-cal ≈ 12 s, write-cal (32 KB) ≈ 245 s, full write
≈ 8.5 min. **Write speed is protocol-inherent** (per-byte pacing at 9600
baud), not a bug; `--data-delay 0` speeds the sim runs up but is unverified
on real hardware.


### 18.5 Repo state

- dde3-toolkit is now its **own git repo** (`git init` 2026-11-11, branch
  `main`). The old IkacLogger repo was backed up to
  `/tmp/old-logger-repo-backup-20261109.tgz` before removal.
- Dual-adapter bench wiring (from §17/§14): `/dev/ttyUSB1` FT232R drives the
  K-line, `/dev/ttyUSB0` QinHeng UART-LPT taps it passively
  (`tools/monitor.py`); the ECU itself is simulated in-process.
- `cargo test`: 25 passing.

### 18.6 Still open

1. **Real-ECU DTC verification** — the protocol and tables are decoded from
   sources + one capture window; a live `dtc` read against the bench ECU
   (sim) is implemented; do it against a real M47 ECU to confirm the
   ORT/FArt/UMwelt mapping (esp. which of the 11 UMWELT slots are used by
   which faults — the shadow reply always carries all 11).
2. **Checksum hardware confirmation** — algorithm verified vs the Python
   reference and unit tests; a real-ECU checksum-status reply has not been
   captured yet.
3. **Live data (MW) ids** — the ADF table (`tool32_edc15m_export.csv`)
   comes from the DDE4/EDC15M export; ids must be confirmed on the real ECU
   (battery 0x0F65 is confirmed by capture).
4. Wire the GUI write path to the real 9600 pacing already used by the CLI
   (done — both pass 9600 now; the old 38400/125000 "fast" args were dde4
   leftovers silently ignored by the protocol; "fast write" now only drops
   the inter-byte pause).

