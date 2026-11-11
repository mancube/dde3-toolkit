# K2 protocol, the mode/baud jobs, and the full "what can I talk to this ECU with" map

Source: `decompiled_winkfp.txt` (10DDE30.prg, 33 jobs), `decompiled.txt`
(DDE30DS0.prg, 177 jobs), VM runs of the jobs via `tools/prg_vm.py`, and
`edc15m_out/research/out/disasm.txt` (ECU firmware, C166).
Companion to `FINDINGS_baud_service_protocol.md` (DS2 framing, break-protocol
decode) and `FINDINGS_faster_logger.md` (sampling plan).

Question answered: *"There are jobs that switch to K2 / DS2 and jobs that
change baud rates — what are the other protocol modes, what do these jobs
actually do, and what besides DS2 can I talk to the ECU with?"*

---

## 1. K2 — the second protocol

**K2 is Bosch's legacy 16-bit, word-oriented test-bench K-line protocol.**
It is the same protocol world previously found in the ECU disassembly as the
break-entered 10.4 k / 38.4 k state machine (states 3–6 of the S0 ISR at
`0x1E904`, the `0x1F6D2` command interpreter, the PEC bulk transfer with its
`0x55` fill byte). The WinKFP job set names it: the ECU speaks **DS2 and K2**,
and two jobs toggle between them.

### 1.1 `SCHALTE_PROTOKOLL_DS2_NACH_K2` (10DDE30.prg @ `0x1081D`)

One DS2 frame, no reply validation, job always reports OKAY:

```text
12 05 90 56
```

- service **`0x90`** — the same service family as the seed-key
  (`12 08 90 42 4D 57 05 <Z>`, "BMW" + Z).
- sub-parameter **`0x56`** = "switch to K2".
- i.e. **K2 entry may be requestable in-band**, not only by a UART break.

**ECU-side verification status (honest):** the only `0x90` handler visible in
the application region is the seed-key handler at `0x28B5E`
(`CD42h == 8` length check at `0x28B72`, `CD4Fh == 2` at `0x28B86`, then the
4-byte key compare at `0x28BB6`–`0x28BD8`). A length-5 `90 56` frame does
**not** match that handler's checks, so on this variant the sub-parameter
`0x56` path is either rejected or handled in the **missing boot block**
(`0x0000–0x7FFF` not in the dump). Untested on hardware — it is a 2-minute
test: unlock, send `12 05 90 56`, watch the tap port.

### 1.2 `SCHALTE_K2_DS2` (10DDE30.prg @ `0x10887`)

The reverse switch, driven from the K2 side. Sequence:

1. `xsetpar` with a **16-bit/word-oriented line descriptor**
   `{02 00 00 00 | 10 00 00 00 | 00 00 D0 07 | C8 00 14 00}`
   (tester-side line reconfig for the K2 word framing).
2. `xsend` K2 frame **`{08 00 02 53 01 0C 20 02}`** —
   expects reply byte[2] == **`0x09`** (else `ERROR_NACK`).
   The `53` = "S" word is the K2 protocol-switch command.
3. `xsend` K2 frame **`{03 00 06}`** —
   expects reply byte[2] == **`0x10`** (else `ERROR_NACK`).
4. `xreset`.

**This is the verified K2 → DS2 exit sequence** — exactly what
`BreakHandle::exit()` in `dde3-toolkit/src/breakmode.rs` was missing.

### 1.3 Frame-shape comparison

| | DS2 | K2 |
|---|---|---|
| Orientation | bytes | 16-bit words (u16 LE) |
| Length field | 1 byte, = total wire bytes incl. trailing XOR | u16 LE (e.g. `08 00`, `03 00`) |
| Checksum | trailing XOR byte | trailing checksum word |
| Address byte | `0x12` ECU address | none in the switch frames |
| Baud | 9600 8E1 (fixed) | 10.4 k / 38.4 k (state 3/6 of the S0 ISR) |
| ECU state | ISR states 0–2 | ISR states 3–6, `0x1F6D2` interpreter |
| Purpose | diagnostics (flash, DTC, live, coding) | test-bench bulk access: RAM/EEPROM/PEC |

## 2. The three "baud" jobs — all red herrings for K-line speed

### 2.1 `BAUDRATEN_UMSTELLUNG` (10DDE30.prg @ `0x121C`)

Builds and sends one DS2 frame, status-checked via the JobResult table with
BUSY retry:

```text
12 08 91 00 <baud 24-bit BE> <p2>     ; template 12 08 91 00 25 80 03
```

- param 1 = requested baud; bytes [3..5] patched with it 24-bit big-endian
  (`0x2580` = 9600 default, `0x4B00` = 19200, `0x9600` = 38400).
- Service **`0x91`** is **not in the ECU's command map** (routines_part3:
  `00 02 04 05 0A 0B 0C 0D 0E 0F 40/41/42 6C/6D 90 9E/9F`) — **unsupported on
  this variant.** The job exists for other ECU families.

### 2.2 `BAUDRATEN_LESEN` (10DDE30.prg @ `0x1644`)

1. Sends `12 04 0D` (service `0x0D`, the programming/test-code area).
2. On OK, extracts an EEPROM address from the reply and reads 2 bytes of the
   stored baud (`12 09 06 <addr> 02` template).
3. Fallback string in the job: **`"KEIN BAUDRATEN LESEN MIT DIESEM SG MOEGLICH"`**
   ("baud reading not possible with this ECU") — the job itself concedes this
   ECU does not support it.

### 2.3 `SET_EDIC_BAUDRATE` (10DDE30.prg @ `0x10B63`)

- Params: baud string (e.g. `"38400"`) + type string (e.g. `"EDIC"`).
- **Contains no `xsend` at all** — only tester-side `xsetpar` line reconfig:
  - `"38400"` + not-EDIC → blob `… 00 96 12 00 … E8 03 00 00 14 00 …`
    (`0x9600` = 38400, `0x03E8` = 1000, `0x14` = 20)
  - `"38400"` + EDIC → blob `… 00 96 12 00 …` variant
  - else (9600) → blob `… 80 25 12 00 … E8 03 32 00 14 00 …` (`0x2580` = 9600)
- Confirms the target is the **tester's EDIC (engine bus / CAN) channel**,
  not the K-line and not the ECU. Nothing changes in the ECU.

## 3. `STEUERN_SYNC_MODE` / `STATUS_SYNC_MODE` — EWS, not a bus mode

| Job | Frame |
|---|---|
| `STEUERN_SYNC_MODE` | `12 05 6C <mode>` — DS2 service **`0x6C`** |
| `STATUS_SYNC_MODE` | `12 04 6D` — DS2 service **`0x6D`** |

Per the ECU command map, `6C/6D` are **EWS sync/status** — the immobilizer
(EWS) interface synchronization used when pairing/matching the immobilizer.
"SYNC MODE" is EWS state, not a communication-mode switch.

## 4. The full "what can I talk to this ECU with" map

| # | Channel | Baud | How entered | What it does |
|---|---------|------|-------------|--------------|
| 1 | **DS2** (K-line) | 9600 8E1, fixed | default after boot | Diagnostics: ident, AIF, coding, faults, flash read/write, live data (`0B`), seed-key (`90`), test codes (`0D`), test-bench cmd (`07`), session keep/end (`9E/9F`) |
| 2 | **K2/PEC** (K-line) | 10.4 k / 38.4 k | UART **break** + `0xA5`/`0xA6` handshake (verified in disasm), *or possibly in-band* `12 05 90 56` (untested) | Test-bench bulk access: 24-bit RAM/EEPROM reads, checksum, PEC bulk (`0x55`-fill, `0x3E` opcode) — the 38.4 k logger path |
| 3 | **EDIC / CAN engine bus** | (CAN) | separate physical bus | Engine-network bus; the .prg files only ever reconfigure the *tester* side of it (`SET_EDIC_BAUDRATE`) |
| 4 | **EWS interface** | in-band via DS2 `6C/6D` | — | Immobilizer sync/status |

In-band DS2 services that touch the *other* worlds: `0x90` subcommands
(seed-key `0x42` "BMW" path; K2-switch `0x56`), `0x6C/0x6D` EWS, `0x07`
(unknown semantics, used by the test-bench-mode job `STEUERN_TESTPLATZ`:
`12 0B 07 10 00 00 00 02 4B 9A` after a seed-key unlock).

## 5. Consequences for dde3-toolkit

1. **K2 → DS2 exit is now known**: K2 frames `{08 00 02 53 01 0C 20 02}`
   (expect `[2]==0x09`) then `{03 00 06}` (expect `[2]==0x10`), plus the
   word-oriented `xsetpar` line descriptor. Add to
   `BreakHandle::exit()` in `src/breakmode.rs`.
2. **Two K2 entry candidates**: the break (disasm-verified) and the in-band
   `12 05 90 56` (job-verified, ECU-side unconfirmed — boot block blind
   spot). Cheap HW test; if it works it's a cleaner entry than the break.
3. **No K-line baud change is possible via any DS2 service on this ECU**
   (`0x91` not implemented, `BAUDRATEN_LESEN` self-disables). The 38.4 k
   logger path in `FINDINGS_faster_logger.md` (Tier 2, K2/PEC direct RAM
   reads) remains the only legitimate fast link.
4. **`SET_EDIC_BAUDRATE` is tester-side only** — anything replicating the
   .prg in the toolkit must not expect an ECU reply from it.

## 6. Verification status (honest list)

| Claim | Status |
|---|---|
| DS2/K2 are the two K-line protocol worlds | job set + disasm agree |
| K2 = states 3–6 / `0x1F6D2` interpreter / PEC | disasm (previous session) |
| DS2→K2 in-band via `12 05 90 56` | **unverified** — visible `0x90` handler expects seed/key frames; `0x56` path may live in the missing boot block |
| K2→DS2 exit frames | from the job decompilation — **not yet driven on hardware** |
| `0x91` (baud change) not implemented here | not in the ECU command map; consistent with the job's sibling ECU families |
| K2 on-wire byte order / checksum word layout | inferred from the 8-byte and 3-byte switch frames; the 3-byte frame `{03 00 06}` layout in particular needs a capture to pin down (is `06` a checksum word low byte? a length?) |

## 7. References

| Thing | Where |
|---|---|
| `SCHALTE_K2_DS2` decompilation | `decompiled_winkfp.txt` @ line 14653 (`0x10887`) |
| `SCHALTE_PROTOKOLL_DS2_NACH_K2` | `decompiled_winkfp.txt` @ line 14630 (`0x1081D`) |
| `BAUDRATEN_UMSTELLUNG` / `BAUDRATEN_LESEN` / `SET_EDIC_BAUDRATE` | `decompiled_winkfp.txt` @ lines 918 / 1149 / 14811 |
| `STEUERN_SYNC_MODE` / `STATUS_SYNC_MODE` | `decompiled_winkfp.txt` @ lines 14921 / 15269; VM: `12 04 6D` |
| ECU `0x90` seed-key handler | `disasm.txt` `0x28B5E` (routine table row `028B5E`) |
| ECU command map | `edc15m_out/research/routines_part3.md` |
| K-line baud ladder, DS2 framing, break decode | `FINDINGS_baud_service_protocol.md` |
| 38.4 k logger plan | `FINDINGS_faster_logger.md` |
| Job VM (frame extraction) | `tools/prg_vm.py` |
