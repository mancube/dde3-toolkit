# DDE3 Toolkit — Handover Document

> For the agent taking over. Read this fully before starting.
> **Read §19 first — session 8: the checksum false-BAD bug is fixed,
> `checksum.rs` is re-ported line-for-line from the Python tool with
> byte-for-byte parity, and EDC15M == DDE3 (same ECU) is documented.**
> Then §18 (2026-11-11 status — fault read/clear solved, DTC tab, bench
> runner, git repo), then §0b (session-4 status, where the plan was
> re-scoped).
>
> ⚠️ §0–§13 were **reconstructed on 2026-11-09** after an editing accident
> truncated this file. The reconstruction uses the session-3/4 summary plus
> re-derived facts (job list re-dumped from the .prg, frames.md intact).
> Where the original wording was lost, content is faithful but paraphrased.

## 21. ⚠️ REAL-CAR FINDING (session 12) — ECU replies with `0x12`, NOT `0x92` ← **READ THIS FIRST**

The toolkit was developed against a `frames.md`/WinKFP assumption that the ECU
answers with the DS2 response prefix `0x92` (`ECU_ADDR | 0x80`). **A real-car
capture (`dde3 toolkit log.txt`, 2026-11-09) proved that wrong.** On this
ECU the reply is prefixed with the ECU's own address **`0x12`** — not `0x92`.
This single mismatch was why ident, fault read, and live/battery all failed to
parse against the real car.

**Real-wire reply convention (authoritative, from the capture):**

```text
<12> <len> <SB> [<data ...>]   + trailing XOR on the wire
```

- Prefix byte = `0x12` (ECU address), **not** `0x92`.
- Status (JOBRESULT) at offset **[2]**; data from offset **[3]** (fault/flash)
  or offset **[5]** (MW live).
- `len` (offset [1]) = total wire length **including** the trailing XOR byte.
- The trailing XOR still covers all payload bytes (unchanged).

**Other real-wire facts discovered from the same capture:**

1. **IDENT is a bare ASCII string**, not a `12 …` envelope. `TX 12 04 00 16`
   → `RX 38 38 37 35 32 30 31` = `"8875201"` (XOR-terminated on the wire).
   `ident_valid()` now accepts a 4–48 byte printable string.
2. **The ECU emits a periodic ~16-byte live block** on the K-line that
   interleaves with request/reply: `12 10 A0 10 00 00 00 00 00 03 DE 0B 26 02
   4A` (DE/DF and 25/26 cycle each tick). This is the ECU's own polling of its
   last-selected live set, NOT a reply to us. It must be skipped when selecting
   the small fault/battery replies.
3. **MW live reply** to `12 0F 0B 10 <5 ids>`: `12 10 A0 10 00 <u16 BE per id>
   ` — for ids `[0F83 0F10 0F40 0F00 0F65]` the values were `[0, 0, 990, 2854,
   586]` = PWG 0, N 0, boost 990 hPa, WTF 2854 (→12.3 °C), batt 586 (→11.9 V)
   (engine-off, ~12 °C — consistent).
4. **Battery (single-id) reply** is smaller: `12 1B A0 …` / `12 08 A0 10 00
   <u16>` (27-byte / 8-byte forms seen). Distinguished from the 16-byte block
   by its `len` field.

**Code changes (all in `src/protocol.rs` + `src/sim.rs`):**

- Reply prefix checks changed from `ECU_ADDR | 0x80` (`0x92`) to `ECU_ADDR`
  (`0x12`): `live_values`, `read_dtcs`, `clear_faults_ds2`,
  `check_flash_read_reply`. `ident_valid()` rewritten for the bare string.
- **`FrameParser`** rewritten from tail-anchored `take_frame` to a
  front-anchored 3-case extractor using `xor_end()` (shortest-valid-prefix
  XOR), `MAX_BUF=192` (was `MAX_FRAME=64`) so echo+reply fit, and a
  tail-half resync on overflow.
- **`do_request(payload, timeout, accept)`** + **`request_filtered`**: drain
  *all* frames per chunk, log each as `RX`/`SKIP`, and return only the first
  frame satisfying `accept`. `is_not_live_broadcast(f)` (skip `f[1]==0x10 &&
  f[3]==0x10`) is used for the small fault/battery replies; `live_values` uses
  a **length-matching** filter (`f[1] == 6 + 2*n`) so a 5-id poll (whose reply
  equals the broadcast) still resolves while a 1-id battery read skips the
  16-byte block.
- **`src/sim.rs`** updated to the `0x12` convention so `--sim` stays consistent
  (status/nack/ident/mw all emit `0x12`, not `0x92`).
- **5 new real-car fixture tests** (bytes taken verbatim from the capture):
  `real_car_live_reply_splits_echo`, `real_car_live_values_decode`,
  `real_car_ident_reply_is_raw_string`, `ident_valid_accepts_plain_string`,
  `is_not_live_broadcast_distinguishes`.

**Status:** `cargo build` clean, `cargo test` → **48 passed / 0 failed**
(43 pre-existing + 5 new). **Re-test against the real car is the next step** —
ident, fault read, and live/battery should now parse correctly end-to-end.

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

## 5. Checksums (TDI V3.1, ported from edc15m-checksum-tool)

- **EDC15M and DDE3 are the same ECU** — Bosch EDC15M is the chip/family
  name, DDE 3.0 the hardware name. The Python tool in
  `edc15m-checksum-tool/edc15m_checksum.py` works on the exact reads this
  toolkit produces; `src/checksum.rs` is a line-for-line port of it.
- Bosch **TDI V3.1**: 32-bit little-endian checksum over a block range,
  seeded 16-bit rolling algorithm (h/g with rol 11/9, −h, ⊕g). Checksums are
  **discovered by content** (2-byte block id at `0xB7A` + slot scan), not
  from a fixed offset table:
  - 256 KB full: calibration slot `0x3FBFC` covers `[0x38B80, 0x3FC00)`,
    seeds `8631 EFCD 8631 DF9B`; code slot `0x13FFC` covers `[0x10000, 0x14000)`,
    seeds `0000 0000 8631 DF9B` (only while the field is non-blank).
  - 32 KB partial read: calibration slot `0x7BFC` covers `[0x0B80, 0x7C00)` —
    the partial read is the last 32 KB of the full image, so the same
    checksum sits `0x38000` earlier.
- Verified byte-for-byte against the Python tool on real reads (good reads
  verify OK; corrupted reads produce identical `.fixed` files).
- In-band status: STATUS_CODIER_CHECKSUMME job → `12 04 0A` frame (ECU-side
  result), analogous to DDE4's `31 01` post-program verify. A real-ECU
  reply has not been captured yet — confirm on hardware before flashing
  corrected images.

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

---

## 18. Session 6 findings: fault memory SOLVED, DTC tab, bench, new git repo (2026-11-11)

### 18.1 ECU identity (captured 2026-11-09, `analysis/captures/ecu_identity.txt`)

- Reply to `12 04 00 16`: `92 04 A0 44 44 45 33 2E 30 20 46 55 45 52 20 4D 34 37`
  = **"DDE 3.0 fuer M47"** — the real ECU is an M47 engine DDE 3.0.
- The real ECU answered all services the same day (ident, DS2 fault read,
  seed/key `12 08 90 42 4D 57 05 00`, and a **full 256 KB flash write** that
  completed 11:21–11:24).

### 18.2 DTC protocol — SOLVED (was the big open item, §14/§17)

DDE3 has no dedicated fault-READ job in `10DDE30.prg`; the fault memory
lives in the **diagnostic program `DDE30DS0.prg`** (the DDE4 analogue of the
DS2 shell). Its 177-job table was decoded with the same VM
(`tools/prg_vm.py`). Fault services are DS2 sub-commands (decoded from the
2026-11-09 11:18–11:26 captures + VM):

| Service | Request | Reply (SB=0xA0) |
|---|---|---|
| Fault count | `12 05 14 01` | `<n>` |
| Fault entry (iterates; pos advances, wraps to 1 after last) | `12 05 04 00` | `<ort> <art> <uw1:2 BE> <uw2:2 BE>` |
| Shadow/freeze values | `12 05 04 01` | 11 × u16 BE (per-fault, after entry request) |
| Clear fault memory (DS2) | `12 04 05` | — |

(The K-line BAP clear `12 09 07 0E 42 41 50 00` from 10DDE30.prg is a second,
independent clear path — both are implemented.)

**The DTC text tables are embedded in the `DDE30DS0.prg` body** (XOR 0xF7
encoded, NUL-terminated strings at fixed offsets) — unlike DDE4, which
ships them in external CSV/SGBD files:

| Table | Offset (decoded body) | Rows | Meaning |
|---|---|---|---|
| ORT (fault classes) | `0x06b725` | 48 | class text, 144 B rows |
| FArtMatrix | `0x06bf7a` | 47 × 16 | ORT slot → FArt code |
| FArtTexte | `0x06cf69` | 97 | fault text, 160 B rows |
| FUmweltTexte | `0x06df52` | 11 | freeze params + `value = A*raw + B` scaling (raw = u16 BE) |

Tools added: `tools/extract_ds_frames.py` (fault-service frames from the VM),
`tools/disasm.py` (full EDIC15/EDIABAS disassembler: 118 opcodes, 16
addressing modes — used to locate the tables), `tools/gen_dtc_rs.py`
(regenerates `src/dtc.rs` from the .prg; run with
`python3 tools/gen_dtc_rs.py --prg /path/to/DDE30DS0.prg`).

`src/dtc.rs` (generated): ORT/FART/UWELT/MATRIX constants, `Dtc` struct,
`text()`, `uw_values()` (scaling), `battery_uw()` (0x08). 6 unit tests.

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

## 19. Session 8: checksum false-BAD fixed, checksum.rs re-ported, EDC15M==DDE3 documented

User report: the Checksum tab flagged blocks as BAD on images whose
checksums were actually correct. Root cause had three layers:

1. **Carried-over dde4-style fixed table.** The first `checksum.rs` port kept
   a `BLOCKS` table (`application` 0x37FFC, `calibration` 0x3FFFC, …) with a
   `TARGET` value because the GUI indexed results by that table. The real
   EDC15M/DDE3 flash uses the TDI V3.1 layout with *discovered* positions
   (calibration `0x3FBFC` in full / `0x7BFC` in partial reads, code
   `0x13FFC`), so the table never matched reality.
2. **Marker-path key bug (the actual false-negative).** `find_marker` read
   the id as `[buf[m+0x3FFA], buf[m+0x3FFC]]` — a non-contiguous pair — while
   the Python reference reads `buf[m+0x3FFA : m+0x3FFC]`. On real reads the
   key never matched, `find_checksums` returned `[]`, the CLI printed a
   vacuous "all checksums OK", and the GUI (empty results → `unwrap_or`)
   showed every block BAD. Fixed to the contiguous `0x3FFA/0x3FFB` pair;
   the Python tool was already correct and needed no change.
3. **GUI rewired to discovery.** The Checksum tab no longer uses any fixed
   table: it renders `checksum::verify()` results (Block / Pos / Stored /
   Computed / Status / Stored-edit / Apply / Fix), with a proper
   "no block found" state for blank or foreign images. `sim.rs`'s checksum
   readout and the CLI were updated to the same API. `bench.rs` write
   path corrects all discovered blocks.

New/changed tests (29 passing): LCG + both selftest regression vectors,
marker-path full image (code + calibration discovered), padded 32 KB
partial read (only calibration at 0x3FBFC), blank image → no blocks.
Byte-for-byte parity with `edc15m_checksum.py` verified on real 256 KB and
32 KB reads, including corrupted-byte round trips (Rust `.bin` == Python
`.fixed.bin`).

**Naming (user directive):** EDC15M and DDE3 are the same ECU. Noted in the
`dde3-toolkit` README, the `edc15m-checksum-tool` README, and §5 — so the
mixup never happens again.

---

## Session 10 — DS2 baud analysis, batched live data, break-entered service protocol

### What was asked
Confirm the ECU's DS2 handling for the *fastest* read/write/logging, batch
live data into one telegram (as other loggers do), and plan + implement the
break-entered service protocol.

### Findings (full write-up: `FINDINGS_baud_service_protocol.md`)
- The K-line is one C166 S0 UART with a **3-value baud ladder**: `S0BG` =
  `0x33` (9600, default), `0x2F` (~10.4 k), `0x0C` (~38.4 k).
- **The DS2 diagnostic protocol is fixed at 9600** — the receive state machine
  at `0x1E904` (states 0–2) never re-rates the port, and there is no in-band
  DS2 command that does. DS2 read/write/log ceiling = 9600 8E1.
- **DS2 length-byte rule** (receiver `0x1EB2C`/`0x1EB46`): the length byte
  equals the **total wire byte count** (address byte + payload + trailing XOR),
  capped at 0x80.
- The **tested** WinKFP logger (`testo customjobs.txt`, job
  `MW_SELECT_LESEN_NORM`, disasm `0x5FE5F`) sends the whole live set in ONE
  telegram: `12 <LEN> 0B 10 <ids…>` with `LEN = 5 + 2·n` (the job computes it
  with `slen + 5`).
- **The bug:** `frame_mw_select` hard-coded `LEN = 0x05` and the loop chunked
  in 10s. The sim ignored the length byte, so it only worked against the sim.
- The 10.4 k / 38.4 k bauds live in a **separate break-entered service
  protocol** (states 3–6 of `0x1E904`): UART break → 10.4 k → handshake
  (`0xA5`, gated on internal unlock flags `C0DF.0`/`C3A9==0xA5`) → optional
  38.4 k. `BAUDRATEN_*` jobs target the CAN/EDIC bus, not the K-line.
- **Open (needs HW):** the secondary command opcodes at `0x1F6D2` (the
  byte-addressed RAM/EEPROM/PEC interpreter) — not decoded, not guessed.
  Also flagged: the flash read/write frames use a 2-byte count in the reference
  (`12 09 06 <A:3> <n:2>`) vs the toolkit's 1-byte count + hard-coded `LEN=0x09`;
  left unchanged pending a capture (a full 256 KB write reportedly worked on HW,
  so the framing model for that path is unresolved — do NOT change without a capture).

### Changes
- **`src/protocol.rs`**
  - `frame_mw_select` now computes `LEN = 5 + 2·n` (with a `MW_BATCH_SIZE` = 50
    guard, under the 0x80 cap) instead of hard-coding 0x05.
  - Added raw transport for break mode: `Kline::raw_write` / `raw_read` /
    `raw_break` (uses `serialport` `set_break`/`clear_break`).
- **`src/app.rs`** — the live loop now sends the selected set in as few
  telegrams as the cap allows (one per `MW_BATCH_SIZE`) instead of one per 10;
  dropped the dead `defined`/`single`/`redefine` scaffolding.
- **`src/breakmode.rs`** (new) — the break-entered service protocol: `s0bg`
  baud constants, `enter(kline, fast)` (break → 10.4 k → `0xA5` handshake →
  optional 38.4 k), `BreakHandle` raw transport + `exit()`, `BreakSim`, unit
  tests. CLI: `dde3-toolkit break-probe [--fast]`.
- **Tests** — new `mw_select_batched_length` (protocol) and `mw_live_batch`
  (sim) lock in the batched framing; `breakmode` baud-ladder + handshake-gate
  tests. **33 passing** (release + debug).

### Next step (hardware)
Run `dde3-toolkit break-probe --port /dev/ttyUSB1` (after UPROG_EIN + seed/key)
with `tools/monitor.py` capturing `/dev/ttyUSB0`, to (a) confirm the handshake
byte/rate that selects 38.4 k, and (b) capture the secondary command opcodes at
`0x1F6D2`. Then add the byte-addressed read/write commands to `breakmode.rs`.

## 20. Session 11 — break-mode secondary command set DECODED + implemented (offline RE)

With no ECU on the bench, the `0x1F6D2` interpreter was decoded fully from
`/home/ilija/Desktop/edc15m_out/research/out/disasm.txt` (ground truth) and
cross-checked against `research/routines_part2.md`.

### What is now known (and implemented in `src/breakmode.rs`)
- **Frame shape**: raw 4-byte blocks, no DS2 length/XOR —
  request `[0x01, OPC, xx, DATA]` (first byte MUST be 0x01, gate `0x1F6EC`),
  reply `[0x00, TAG, RESP, HI]` (TAG 0x02/0x01/0x05, HI 0x01 data / 0x00 default).
- **Opcode map** (slot = `(table_addr − 0x1F724)/4`, 0x51 entries, clamped to 0x50):
  0x02 echo+checksum, 0x04/0x07 addr low/high, 0x0B/0x0E/0x0D word-aware RAM
  reads, 0x10 block-buffer read, 0x13 fault/DTC read (`+0xCE7A`), 0x18 EEPROM
  16-bit read, 0x1B checksum, 0x25/0x26 set addr, 0x2A block handshake/reset
  (`DATA` must be 0x11), 0x31 block store, 0x3E PEC bulk, 0x4F set block size.
- **Shared-register subtlety**: the address register is the 24-bit value at
  `C804..C806` and the **partner cache** is the 24-bit value at `C806..C808` —
  they **share byte C806**; the word-read path stores `C806..C808 ← addr −
  0x0FF1E` before the 24-bit `C804 −= 1` decrement. `BreakSim` models this with
  a 5-byte shared block.
- **Two unknown-opcode defaults**: `0xED` (slot A, `0x1FBBA`) / `0xEB` (slot B,
  `0x1FBD0`), `HI = 0x00`; per-slot A/B transcribed in `BreakSim::default_reply`.
- **Corrections to routines_part2.md**: checksum is **0x1B** (not 0x1C),
  16-bit EEPROM read is **0x18** (not 0x19).

### Changes
- **`src/breakmode.rs`** — added `opcode::*` constants, `request()` frame builder,
  `Reply`/`parse_reply()`, `BreakHandle::command()` + typed helpers
  (`reset_block`, `set_addr`, `get_addr`, `checksum`, `read_ram`), and the
  mailbox-faithful `BreakSim` (RAM + block buffer + checksum + addr/partner
  registers + default replies). `enter()` now sends the **second** handshake
  byte (`0xA6` = fast → 38.4 k latch, else `0xA5` = stay 10.4 k).
- **`src/main.rs`** — `break-probe` drives the verified sequence
  (`0x2A/0x11`, `0x1B`, `0x04/0x07`, legacy `0x51` → expected `00 02 ED 00`).
- **FINDINGS §4.4** (new) — the decoded command format + opcode table + the
  corrections above.
- **Tests** — 12 `breakmode` unit tests (was 2). **43 passing** (release+debug),
  build clean (no breakmode warnings).

### Next step (hardware, when the ECU returns)
1. **T2.1 capture**: `break-probe` + `tools/monitor.py` on `/dev/ttyUSB0` to
   confirm the on-wire 4-byte layout (byte order, echo behaviour, PEC framing).
2. Then wire the byte-addressed RAM/EEPROM/fault reads into the CLI/GUI and the
   `0x3E` PEC bulk stream (the only piece still capture-pending).

## 17. DS2 service 0x91 (baud change) — ECU patch + `baud` command (session 8)

Stock firmware does **not** implement DS2 `0x91` (no case in the dispatcher
chain 0x2933E–0x293D0, `research/out/disasm.txt`; falls through to the
unsupported path 0x295F2 → reply `12 04 FF`). The stock tester job
`BAUDRATEN_UMSTELLUNG` (10DDE30.prg @ 0x121C, template `12 08 91 00 BBBB
BBBB BB 03`) was never wired to it either — that is why nothing in the
stock DDE30 world can re-rate the DS2 link.

**ECU-side patch** (32 bytes, no TDI checksum impact):
- dispatcher hook 0x293D0: `EA 00 F2 95` → `EA 00 C3 18` (fallthrough now
  targets trampoline T @ 0x185C3 — a 65-byte erased, zero-reference block)
- T: `CMP R4,#91; JMPA cc_Z,H; JMP 0x295F2`
- H: `CALLS 0x2692C` (header) + `CBFA=A0` (ACK) + `MOV S0BG,#0x0C` (38.4 k)
  + `JMP 0x295F6` (common tail) — exact idioms of the 0x9E case and the
  S0BG encoding at 0x1E8F4.

Key wire semantics: the ACK is queued **after** the S0BG switch, so it
goes out at 38.4 k — that is the sync point; never expect a reply at the
old rate. Revert to 9600 = power cycle (v1 hard-codes 38.4 k; variant V2
honors the 24-bit baud field from CBF6+1..+3).

Full docs + applier + patched image:
`~/Desktop/edc15m_out/baud91_patch/{patch_0x91.py, PATCH_0x91.md, e46_baud91.bin}`
(checksum tool confirms both TDI sums still valid on the patched image).

**Toolkit changes:**
- `src/protocol.rs` — `frame_baud(baud, p2)` builder (0x91 frame).
- `src/main.rs` — `baud` subcommand: send 0x91 @9600 (expect silence) →
  settle → `set_baud(38400)` → drain pending ACK (raw_read) → verify with
  IDENT (3×) → on failure fall back to 9600 to distinguish "never switched"
  (stock firmware) from "dead" (power-cycle needed).
- 43 unit tests pass; `cargo check` clean (only pre-existing warnings).

**Bench test when the ECU is back:**
1. flash `e46_baud91.bin` (full image; pristine backup `e46_orig.bin`)
2. `dde3-toolkit ident --port /dev/ttyUSB1` (9600 sanity)
3. `dde3-toolkit baud --port /dev/ttyUSB1` → expect IDENT reply @38.4k
4. power cycle → `ident` again at 9600 (revert works)

## 22. Session 12 — FS_LESEN wire protocol verified, `read_dtcs()` rewritten (2026-11-11)

The old `read_dtcs()` mis-parsed the fault reply and reported the wrong ORT
(Mengensteller 0x01 instead of Gluehanlage 0x08). Root cause: the ECU returns
the 50-byte fault record in **chunks across several requests** (per the
decompiled FS_LESEN job in `DDE30DSO.prg`), and the old code treated the
reply as one flat blob.

### Verified wire protocol (DS2, 9600 8E1, replies start with ECU addr 0x12)

| Step | Request (TX)            | Reply (RX data after `A0`)                        | Record bytes |
|------|--------------------------|----------------------------------------------------|--------------|
| 1    | `12 05 14 01`            | `<anz> <rec 0..22>`                                | header 22 B  |
| 2    | `12 05 04 00`            | `<pos> <rec 22..26>`                               | entry 4 B    |
| 3    | `12 05 04 01`            | `<pos> <rec 26..37>`                               | shadow 11 B  |
| 4    | `12 05 14 01` (again)    | `<anz'> <rec 37..50> [<next rec 0..22> if anz'>0]` | tail 13 B    |

Step 4 is the only part not directly visible in the captures (old toolkit
never sent it); it is taken from the decompiled job (count request is re-used
after the entry/shadow pair, reply carries the record tail and, when more
faults remain, the next fault's 22-byte header inline). **Pending one real-car
confirmation** — the wire log (`dde3newlog.txt`) will show it on the next run.

### 50-byte fault record layout (matches `actual error.txt` M47 reference)

| Off | Len | Field |
|-----|-----|-------|
| 0   | 1   | ORT (fault class, FORTTEXTE key) |
| 1   | 1   | ART mask — bit *i* selects FARTMATRIX column A(i+1) |
| 2   | 1   | error frequency (0..100 %) |
| 3   | 19  | freeze/environment words (raw; per-ORT scaling via SHADOWFEHLER + FUmwelt) |
| 22  | 4   | entry chunk (runtime counters) |
| 26  | 11  | shadow chunk (SHADOWFEHLER freeze data) |
| 37  | 13  | tail (counters / checksum tail) |

Reference (M47, `actual error.txt`): `08 C8 3C 00 38 9C E3 00 37 9D DE C4 C1
04 00 48 00 99 03 00 76 DC 5B 03 4A 00 76 D1 7D 00 76 DC 5A 00 34 32 33 32 34
00 30 21 00 00 00 31 C5 00 00 22` → ORT 0x08 Gluehanlage, mask 0xC8 → bits
3,6,7 → FARTMATRIX[0x08] slots A4/A7/A8 = **0x32 "Gluehkerzen bzw.
Gluehrelais defekt"**, **0x05 "Fehler momentan vorhanden"**, **0x07 "Fehler
sporadisch"**; frequency 60. Tables cross-checked against the extracted
`fartmatrix.txt` / `farttexte.txt` / `forttexte.txt` / `fumwelttexte.txt` /
`shadowfehler.txt` / `jobresult.txt`.

### Changes
- `src/protocol.rs` — `read_dtcs()` rewritten for the 4-step chunked protocol
  (iterates all faults, advances via the next-header in step-4 replies,
  pads/truncates to 50 B, keeps the full record per DTC).
- `src/dtc.rs` — `fart_slot()` (ART mask bit → FARTMATRIX code) and
  `active_arts()`; `Dtc` extended with `art_mask`, `freq`, `arts`, `record`;
  `Dtc::from_record()` builds a DTC from the raw 50 bytes; `describe()`
  renders ORT/freq/ART texts + raw record.
- `src/sim.rs` — sim now stores full 50-byte records and answers the 4-step
  sequence (`fault_count/fault_entry/fault_shadow`).
- `src/app.rs` — DTC tab shows ORT + mask + freq + all active ART texts +
  freeze words + the full errorcode hex (same as the reference report).
- 57 unit tests pass, incl. `dtc_from_record_reference_m47` (reference record
  → ORT 0x08, arts [0x32,0x05,0x07]) and `fault_replay_assembles_full_records`.

### Bench test (real car)
1. `dde3-toolkit dtc --port /dev/ttyUSB1` → expect "Gluehanlage" + the three
   ART texts above, not "Mengensteller".
2. Check `dde3newlog.txt` TX/RX for the step-4 tail-count reply shape; if it
   differs from the table above, adjust the 2-line tail slice in
   `read_dtcs()` only — the rest of the pipeline is data-driven.
