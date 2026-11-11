# DTC / fault memory protocol

The fault memory of the DDE3 lives in the diagnostic program
`DDE30DS0.prg` (not in `10DDE30.prg` — the latter has no fault-read job).
The protocol and the embedded text tables were decoded from the VM runs,
decompiled jobs, and the 2026-11-09 real-car captures.

## DTC protocol — SOLVED (was the big open item, §14/§17)

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

### 50-byte fault record layout (matches `analysis/actual error.txt` M47 reference)

| Off | Len | Field |
|-----|-----|-------|
| 0   | 1   | ORT (fault class, FORTTEXTE key) |
| 1   | 1   | ART mask — bit *i* selects FARTMATRIX column A(i+1) |
| 2   | 1   | error frequency (0..100 %) |
| 3   | 19  | freeze/environment words (raw; per-ORT scaling via SHADOWFEHLER + FUmwelt) |
| 22  | 4   | entry chunk (runtime counters) |
| 26  | 11  | shadow chunk (SHADOWFEHLER freeze data) |
| 37  | 13  | tail (counters / checksum tail) |

Reference (M47, `analysis/actual error.txt`): `08 C8 3C 00 38 9C E3 00 37 9D DE C4 C1
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
