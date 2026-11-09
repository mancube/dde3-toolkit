# DDE3 Toolkit — Handover Document

> For the agent taking over. Read this fully before starting.
> **Read §0b (session-4 status) first — the plan was re-scoped there.**
>
> ⚠️ §0–§13 were **reconstructed on 2026-11-09** after an editing accident
> truncated this file. The reconstruction uses the session-3/4 summary plus
> re-derived facts (job list re-dumped from the .prg, frames.md intact).
> Where the original wording was lost, content is faithful but paraphrased.

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

## 0. STATUS — updated 2026-11-09 (session 3)

| Task | State |
|------|-------|
| VM bugs fixed, `tools/test_uprog_ein.py` passes ALL | ✅ done |
| `analysis/frames.md` — non-param-dependent job frames | ✅ done (145 lines) |
| Parameter-dependent frames (FLASH_LESEN, BAUDRATEN_*) | 🔄 partial — FLASH_LESEN + BAUDRATEN_LESEN extracted with param-supplied runs |
| dde4-toolkit reference read (protocol/app/checksum/main/patches) | ✅ done |
| DDE3 wire-level framing (DS2 params from `xsetpar`, timing, baud switch) | ⬜ open |
| Rust scaffold | ⬜ not started |

## 1. Mission

Build a **DDE3 (BMW EDC15 DDE3.0) K-line diagnostic tool in Rust** that
replicates the functional scope of the existing `dde4-toolkit`:
full/partial flash read & write, live data, protocol logging, and
fault-code read/clear.

The DDE3 protocol is reverse-engineered from `10DDE30.prg` (a WinkFP
bytecode program) using a custom Python VM to extract wire-level frame
structures. **`dde4-toolkit` is the architectural and feature reference** —
do NOT implement every job in the .prg (session-4 re-scope, §0b/§14).

## 2. What is in this folder

```
tools/prg_vm.py               # WinkFP VM: Prg loader + VM executor (~581 lines)
tools/extract_frames.py       # runs all jobs, dumps frames -> analysis/frames.md
tools/extract_param_frames.py # param-supplied runs (flash read etc.)
tools/test_uprog_ein.py       # VM regression test — PASSES
analysis/frames.md            # extracted frames per job (145 lines)
analysis/prg_ocnames.json     # opcode -> name table
analysis/prg_sizes.json       # job sizes
analysis/winkfp_proto.txt     # notes on the WinkFP instruction set
HANDOVER.md                   # this file
```

Key external sources (see §8): `10DDE30.prg`, `decompiled.txt`,
`decompiled_winkfp.txt`, `jobresult.txt` in `/home/ilija/Desktop/logger/`.

## 3. `.prg` file format — FULLY SOLVED

- EDIABAS **OBJECT** header; code section is **XOR 0xF7** encoded (same for
  DDE3 and DDE4 job programs — `12DDE40.prg` loads as-is in the DDE3 VM).
- **Job table @ 0x11b00**: name/addr entries for the 33 jobs (list in §4).
- WinkFP instruction stream; integer registers **I0–I15** plus mode-7
  floating/string registers; **tab** ops (tabset/tabseek/tabget) for result
  tables (e.g. JOBRESULT status table).
- `xsend` = send telegram (frame template, data fields may be patched from
  the response); `xreps` = expect reply; `xconnect`/`xstopf`/`xsetpar`/
  `xawlen` = EDIABAS-side connection/DS2-transport setup (no telegram).

## 4. The 33 jobs (job list @ 0x11b00 — re-verified session 4)

```
0x0000a0  EDIC_RESET              0x0000db  INITIALISIERUNG       0x000189  INFO
0x0002e7  UPROG_EIN               0x000acf  UPROG_LESEN           0x00121c  BAUDRATEN_UMSTELLUNG
0x001644  BAUDRATEN_LESEN         0x002209  UPROG_AUS             0x002259  BLOCKLAENGE_MAX
0x002931  DATEN_REFERENZ          0x00337b  HW_REFERENZ           0x003ba5  ZIF
0x00469f  ZIF_BACKUP              0x0051ed  AIF_LESEN             0x008445  AIF_SCHREIBEN
0x00c080  STATUS_CODIER_CHECKSUMME 0x00c33c  FLASH_LESEN         0x00c79f  FLASH_LOESCHEN
0x00ce81  FLASH_SCHREIBEN         0x00d782  FLASH_SCHREIBEN_ENDE  0x00dde6  STATUS_LESEN
0x00e538  DATENBEREICH_LOESCHEN_0E 0x00e786  SEED_KEY             0x00f3b4  IDENT
0x010050  ADAPTION_LOESCHEN       0x01007a  FS_LESEN              0x010559  FS_LOESCHEN
0x0106bb  DIAGNOSE_ENDE           0x01081d  SCHALTE_PROTOKOLL_DS2_NACH_K2
0x010887  SCHALTE_K2_DS2          0x010b63  SET_EDIC_BAUDRATE     0x010de3  STEUERN_SYNC_MODE
0x0115bf  STATUS_SYNC_MODE
```

## 5. Checksums (TDI V3.1 model, from edc15m-checksum-tool)

- Flash image checksum computed over the image with the TDI V3.1 algorithm;
  dde4-toolkit's `checksum.rs` (244 lines) is the working reference — same
  family, adapted to DDE3 base/size (DDE3 flash read base 0x00000, 256 KB).
- In-band status: STATUS_CODIER_CHECKSUMME job → `12 04 0A` frame (ECU-side
  result), analogous to DDE4's `31 01` post-program verify.

## 6. STATUS BYTE TABLE (JOBRESULT, from jobresult.txt)

| idx | SB | STATUS_TEXT |
|-----|----|-------------|
| 0 | 0xA0 | OKAY |
| 1 | 0xA1 | BUSY |
| 2 | 0xA2 | ERROR_SG_REJECTED |
| 3 | 0xB0 | ERROR_SG_PARAMETER |
| 4 | 0xB1 | ERROR_SG_FUNCTION |
| 5 | 0xB2 | ERROR_SG_NUMBER |
| 6 | 0xFF | ERROR_SG_NACK |
| 7 | 0x00 | ERROR_SG_UNBEKANNTES_STATUSBYTE |

Jobs parse the ECU reply's status byte through this table into `JOB_STATUS`.

## 7. UPROG_EIN — fully understood flow (ground truth from decompiled_winkfp.txt lines 91–150 + byte dumps)

- Connection setup (EDIABAS-side, no telegram): `xconnect` → `xstopf` →
  `xreps 0` → `xsetpar {06 00 80 25 12 00 00 00 00 00 E8 03 19 00 14 00 05 00}`
  → `xawlen {FF FF 00 00}`.
- `xsend 12 04 0D` — enter programming mode (DS2 payload: ECU addr 0x12,
  len 04, service 0D).
- Reply is parsed: status byte → JOBRESULT table; on OKAY a second telegram
  `12 09 06 …` reads back the UPROG status word (see frames.md: UPROG_EIN
  sends `12 04 0d` @0x308 and `12 09 06 00 00 00 00 02` @0x680 with the
  generic OK reply).
- UPROG_AUS: local-only (`xprog 0` / xawlen teardown), **no telegram** —
  matches DDE4 behaviour (exit programming is a transport-state change).

## 8. ⚠️ IMPORTANT — source artifacts (were volatile in /tmp, now on disk)

Copied from /tmp into `/home/ilija/Desktop/logger/` (persistent):
- `decompiled.txt` — full disassembly of `10DDE30.prg`
- `decompiled_winkfp.txt` — WinkFP job listings (line numbers cited in §7/§15)
- `jobresult.txt` — JOBRESULT status table (reproduced in §6)
- `testo customjobs.txt` — custom job descriptions
- `10DDE30.prg` — the target program (env `DDE3_PRG`)
- `DDE30DS0.prg` — 684 KB sibling (differs from 10DDE30.prg; not the target)

## 9. The Python VM (`tools/prg_vm.py`) — status and KNOWN BUGS

Fixed in sessions 1–3 (all verified by `tools/test_uprog_ein.py`):
- pc double-advance bug
- tabset/tabseek/tabget semantics
- strcmp
- mode-7 registers (float/string)
- NUL stripping of string results
- float-into-register conversion

Known remaining gaps (see also §15.5):
- `parw` crashes if a **bytes** param is passed where an int is expected
  (`self.param(v1) & 0xFFFF`) — pass ints for `parw`/`parl`, strings only
  for `pary` (bit #15.5).
- Some opcodes unimplemented (e.g. 0x32, 0x6a — both hit only on DDE4 jobs).
- `xstopf`/`xsetpar` in DDE4's INITIALISIERUNG hit a bytes−int TypeError
  (not needed for DDE3 jobs).

Usage:

```python
vm = VM(prg, params={1: 0x38000, 2: 0x80}); vm.feed = fake_reply
vm.run('FLASH_LESEN')
# vm.requests -> [(pc, frame), ...];  vm.vars -> job result variables
```

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

## 12. Reference: DDE4 toolkit (architecture only!)

`/home/ilija/Desktop/logger/EDC15C4-decomp/dde4-toolkit/` (Rust, ~4900 lines):

| File | Lines | Role |
|------|-------|------|
| `src/protocol.rs` | 870 | frame codec `B8 12 F1 LEN … XOR`, K-line I/O, 9600 8E1 default, 125000 fast baud (svc 0x23), ECU 0x12 / tester 0xF1 / fmt 0xB8 |
| `src/app.rs` | 3057 | 5 tabs: Read, Write, Live, Logger, Log; live = 16-bit register IDs + factor/offset scaling; faults UDS 0x19/0x14 |
| `src/checksum.rs` | 244 | TDI V3.1 checksum |
| `src/main.rs` | 130 | CLI entry |
| `src/patches.rs` | 434 | image patching |

Key DDE4 constants: `ECU_ADDR=0x12`, `TESTER_ADDR=0xF1`, `FMT=0xB8`,
`BASE=0x80000`, `IMAGE_SIZE=0x80000`, `CAL_START=0x70000`, `CAL_LEN=0xC000`.

**Do not copy DDE4 service codes into DDE3** — different service space
(§15.2). Architecture and UX only.

## 13. First session plan (superseded by §0b/§14 — kept for history)

1. Fix VM bugs → 2. extract all frames → 3. decode DS2 wiring →
4. Rust scaffold → 5. flash read/write → 6. live/faults/log/tests.

## 14. Session-4 re-scope: which jobs actually matter

Feature → DDE3 job(s) / frame (from VM runs + decompiled_winkfp.txt):

| Feature (dde4-toolkit parity) | DDE3 job(s) | Frame (DS2 payload) |
|---|---|---|
| Connect / ECU info | INITIALISIERUNG (xconnect/xsetpar, EDIABAS-side), IDENT | `12 04 00` |
| Enter programming mode | UPROG_EIN | `12 04 0D` |
| Security / seed-key | SEED_KEY | `12 08 90 42 4D 57 05 <Z>` (§15.3) |
| Full read (256 KB) | FLASH_LESEN (p1=L addr, p2=I count) | `12 09 06 <A_lo A_mid A_hi> <count>` |
| Partial cal read (0x38000–0x3FFFF) | FLASH_LESEN with addr/count | same |
| Erase before write | FLASH_LOESCHEN (p1=str addr, p2=str count) | `12 09 07 06 <A lo/mid/hi> <count>` |
| Write block | FLASH_SCHREIBEN (p1=str addr, p2=str data) | `12 09 07 02 <A lo/mid/hi> <data…>` |
| End write | FLASH_SCHREIBEN_ENDE | `12 09 07 0F <A lo/mid/hi> <…>` |
| Fault clear | DATENBEREICH_LOESCHEN_0E | `12 09 07 0E 42 41 50 00` ("BAP") |
| Fault read | **UNRESOLVED** — no dedicated fault-read job in the DDE3 .prg. Fault CLEAR via `DATENBEREICH_LOESCHEN_0E` (`12 09 07 0E 42 41 50 00`) is implemented. STATUS_DIGITAL/STATUS_MW* jobs exist for status monitoring but no fault DTC read. |
| Live data | ✅ **RESOLVED** — DDE3 uses service `0x0B` (MW/Messwert) via `MW_SELECT_LESEN` (`12 05 0B 10 <id_hi> <id_lo>`) and `MW_SELECT_LESEN_NORM`/`MW_SELECT_LESEN_NORM_EINZEL`. Measurement table extracted from `tool32_edc15m_export.csv` (from `decompiled.txt`). Implemented in `protocol.rs::live_values()` and `src/app.rs` LIVE_VARS table. |
| Checksum status in-band | STATUS_CODIER_CHECKSUMME | `12 04 0A` |
| Leave / end session | DIAGNOSE_ENDE | `12 04 9F` (UPROG_AUS is local-only: `xprog 0`, no telegram) |

**DROPPED** (not needed): BAUDRATEN_UMSTELLUNG/LESEN, SET_EDIC_BAUDRATE,
SCHALTE_K2_DS2, SCHALTE_PROTOKOLL_DS2_NACH_K2, STEUERN_SYNC_MODE,
STATUS_SYNC_MODE (pending live-data decision), AIF_LESEN/SCHREIBEN, ZIF,
ZIF_BACKUP, DATEN_REFERENZ, HW_REFERENZ, ADAPTION_LOESCHEN, FS_LESEN,
FS_LOESCHEN, BLOCKLAENGE_MAX (info-only), UPROG_LESEN (dup of UPROG_EIN
status read), INFO (static strings). Baud/protocol switching returns only if
the wire-framing work proves it needed for reads.

Consequence for Rust: `protocol.rs` needs ~12 fixed frames + 4 parameterized
builders (flash read/erase/write/end) + JOBRESULT table + 3 checksums — not
"all 33 jobs".

## 15. Session-4 findings: DDE4 parallels

### 15.1 DDE4 toolkit source = `12DDE40.prg`
- `EDC15C4-decomp/analysis_artifacts/reference/testo_extract/app/12DDE40.prg`
  (TESTO extract, 149,910 bytes, 35 jobs; confirmed by PROJECT_NOTES.md and
  `analysis_artifacts/data/flash_protocol.md` — the full verified DDE4
  protocol write-up). Other TESTO DDE4 prgs: `DDE40KW0.prg`, `12DDE409.prg`,
  `12DDE41A/B.prg`.
- **The DDE3 VM loads 12DDE40.prg as-is** (identical .prg format, XOR 0xF7).
  Running its jobs in `tools/prg_vm.py` yields DDE4 reference frames:
  UPROG_EIN/BLOCKLAENGE_MAX `b8 12 f1 02 21 01` (DS2 service **0x21**) ·
  START_DIAGNOSTIC_SESSION `b8 12 f1 03 10 85 01` · STATUS_LESEN
  `b8 12 f1 03 30 eb 01` · STATUS_CODIER_CHECKSUMME `b8 12 f1 02 31 01` ·
  IDENT `b8 12 f1 01 a2` · DIAGNOSE_ENDE `b8 12 f1 01 82` ·
  FLASH_SCHREIBEN `b8 12 f1 01 36` · FLASH_SCHREIBEN_ENDE `b8 12 f1 01 37`.
  SEED_KEY crashes the VM (oc 0x6a unimplemented) but is fully documented in
  flash_protocol.md §1.2: `27 02 "BMW" Z` → seed R → `k_i = R[0x15+i] +
  R[0x2C+i] + R[z]` → `27 04 k0..k3`.

### 15.2 Crucial structural difference: DDE4 xsend = full wire frame; DDE3 xsend = bare DS2 payload
- DDE4 jobs embed the **complete KWP2000\* frame** in the xsend template
  (`B8 12 F1 LEN …`; the XOR byte is added by the EDIABAS transport, so it is
  absent from the template).
- DDE3 jobs send only the **DS2 payload** (`12 04 0D` …, starting with ECU
  address 0x12). The on-wire frame comes from the DS2 K-line transport set up
  by INITIALISIERUNG's `xsetpar {06 00 80 25 12 00 00 00 00 00 E8 03 19 00
  14 00 05 00}` — byte meanings **not yet decoded**. Open: is the wire format
  still `B8 12 F1 LEN payload XOR`? baud/bit format? how does the DS2
  payload-length byte map to the wire LEN? **Do not assume DDE4 framing for
  DDE3 until this is resolved** (old Task 3).
- Service spaces differ entirely: DDE4 = UDS-flavoured (0x27/0x10/0x30/0x31/
  0x34/0x36/0x37/0x23/0x2C/0xA2/0x82); DDE3 = DS2 (0x0D/0x06/0x07/0x0E/0x90/
  0x9F/0x00/0x0A/0x6D). Same ECU addr 0x12 / tester 0xF1 / B8 family.

### 15.3 DDE3 SEED_KEY decoded (decompiled_winkfp.txt @ 0xE786)
- `time S5` → current time string; seconds byte → `Z = seconds mod 0x2E` (46), exported
  as `Z_ZAHL`. (DDE4 analogue: `seconds mod 0x27 + 3`.)
- Sends one telegram **`12 08 90 42 4D 57 05 <Z>`** (service **0x90**,
  "BMW"+05+Z); reply checked via the usual JobResult status byte [2].
- The DDE4-style seed→key sum (`R[0x15+i]+R[0x2C+i]+R[z]`) is **not** in this
  job. Either the 0x90 exchange is self-contained, or the seed rides in
  UPROG_EIN's longer response (bytes [3..5] already patch the flash-read
  address there). **Open:** real capture needed to confirm whether a separate
  key telegram precedes FLASH writes.
- 0x90 recurs in BAUDRATEN (`12 05 90 56`) and STEUERN_SYNC_MODE
  (`12 05 6C 00 …`) — likely a general DDE3 "control" service family.

### 15.4 DDE3 constant-frame catalog (all fixed xsend templates in 10DDE30.prg)
`12 04 0D` UPROG enter (×20) · `12 04 00` IDENT · `12 04 0A` checksum status ·
`12 04 05` FS_LOESCHEN · `12 04 6D` STATUS_SYNC_MODE · `12 04 9F` DIAGNOSE_ENDE ·
`12 05 02` AIF · `12 05 02 FF` AIF (write area) · `12 05 04 01` FS_LESEN ·
`12 05 90 56` baud DS2 · `12 05 6C 00 …` STEUERN_SYNC_MODE ·
`12 08 90 42 4D 57 05 <Z>` SEED_KEY · raw K2 switch `08 00 02 53 01 0C 20 02`
· parameterized templates: flash read `12 09 06 A A A n` · erase `12 09 07 06 …`
· write `12 09 07 02 …` · write-end `12 09 07 0F …` · fault clear
`12 09 07 0E 42 41 50 00`.
Note: the DS2 payload length byte appears to count the total bytes after the
address byte (e.g. `12 09 07 06 A A A n` is 8 template bytes; confirm the
padding/trailing-byte rule on a real capture).

### 15.5 Parameterized extraction status + tool bugs
- `tools/extract_param_frames.py`: FLASH_LESEN {1: 0x38000, 2: 0x80} → OK,
  frame @ 0xC4D7 (logged in `analysis/frames.md` → "Parameterized runs");
  BAUDRATEN_LESEN {1: 0} → frame @ 0x16E0; BAUDRATEN_UMSTELLUNG /
  SET_EDIC_BAUDRATE → no frame before stop (dropped jobs anyway).
- **Bug in the extraction script (mine):** `AIF_LESEN {1: b'0'}` crashes the VM
  parw handler (`param & 0xFFFF` on bytes). AIF_LESEN p1 is `parw I0,#$1.I` =
  an **integer** (AIF number 1..14), not a string — pass `1: 1`. Dropped job.
- VM gaps hit while running 12DDE40.prg (irrelevant for DDE3): oc 0x32
  (DDE4 FLASH_LOESCHEN), oc 0x6a (DDE4 SEED_KEY) unimplemented;
  INITIALISIERUNG xstopf/xsetpar hits a bytes−int TypeError in the VM.

## 16. Rust implementation status (session 6)

### What was built

| File | Lines | Role |
|---|---|---|
| `Cargo.toml` | — | manifest (serialport, anyhow, hex, eframe/egui, rfd, crossbeam-channel) |
| `src/main.rs` | ~330 | CLI subcommands + `--gui` default launching egui window |
| `src/protocol.rs` | ~700 | frame codec (`B8 12 F1 LEN payload XOR`), `FrameParser`, `Kline` serial connection, all 12 fixed frames + 4 parameterized builders + read_full/read_range/write_full/write_calibration/live_values, `JobStatus` enum from JOBRESULT table |
| `src/checksum.rs` | ~200 | **VERIFIED** TDI V3.1 hash/verify/correct ported from `edc15m_checksum.py` — blocks: application 0x00000-0x37FFF comp 0x37FFC, calibration 0x38000-0x3FFFF comp 0x3FFFC) |
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
checksum <img> Verify/correct TDI V3.1 checksums (UNVERIFIED)
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

## 17. Session-5 findings: wire framing CONFIRMED by hardware capture (2026-11-09)

### Method

Passive K-line sniffer (`EDC15C4-decomp/analysis_artifacts/tools/kline_sniff.py`,
9600 8E1, read-only) on `/dev/ttyUSB0` while WinKFP on the other adapter drove
the bus. The sniffer sees exactly what the ECU receives (tester TX only — no
ECU replies visible at this tap). Captures: `analysis_artifacts/tools/
sniff_session{,2,3,4}.raw`, `sniff_bg.raw`.

### What was on the wire

| Time (s) | Raw bytes | Job (from decompiled_winkfp.txt / frames.md) |
|---|---|---|
| 2.56 | `12 04 0d` + `1b` | UPROG_EIN `@ 0x02e7` |
| 7.27 | `12 04 00 16` | IDENT `@ 0xf3b4` |
| 20.80 / 27.77 | `12 04 0d 1b` | UPROG_EIN |
| 46.71 | `12 05 90 56` + `d1` | SCHALTE_PROTOKOLL_DS2_NACH_K2 `@ 0x1081d` |
| (sess. 3) | `1a 04 0d 1b` | UPROG_EIN with a 1-bit flip in byte 0 (`12`→`1a`); XOR no longer validates — checksum catches line errors |
| (sess. 2/4) | `12 04 0d 1b` | UPROG_EIN |

### Findings

1. **The `B8 12 F1 LEN payload XOR` hypothesis is WRONG.** No `0xB8` prefix,
   no tester address (`0xF1`), no length byte. The on-wire frame is
   **`<DS2 payload> <XOR of all payload bytes>`**. DDE3 does not use the
   KWP2000\* B8 envelope that DDE4 uses.
2. **XOR checksum confirmed**: `12^04^0d=1b`, `12^04^00=16`,
   `12^05^90^56=d1`. One bit flip breaks it — usable for resync/error
   detection.
3. **Timing**: WinKFP inserts a ~50 ms gap before the final XOR byte
   (e.g. `12 04 0d` at 2.557 s, `1b` at 2.607 s). Frames must be delimited by
   line silence > ~60 ms; a parser keyed on short silences will split frames.
4. **No ECU replies visible** at this tap — the sniffer only sees tester TX.
   Reply framing still needs a capture on the ECU side of the bus.
5. ECU internal temperature read: not supported (WinKFP fell back to the
   DS2→K2 protocol switch `12 05 90 56` instead).

### Code changes (this session)

`src/protocol.rs`:
- `build_frame(payload)` → `payload + xor_cs(payload)` (no B8/F1/LEN).
- `Frame` struct: dropped `target`/`source` (not present on the wire).
- `FrameParser`: now `feed()` accumulates, `take_frame()` validates the
  trailing XOR after a silence and resyncs by dropping leading bytes;
  incomplete frames (XOR byte still on its way) are kept across reads;
  buffer capped at 64 bytes.
- `Kline::request()` updated to the feed/take_frame flow.
- Removed `TESTER_ADDR`, `FMT` constants.

Tests: `cargo test` → 12 passing, including new `captured_winkfp_frames`
(byte-exact frames from the capture) and
`parser_keeps_partial_frame_across_reads` (the 50 ms gap case).
