# Full DS2 ECU simulator (`SimEcu`) + job-replay test suite

The `--sim` mode of the toolkit runs an in-process ECU model
(`src/sim.rs`, `SimEcu`). Since the full-DS2 upgrade it implements the
**complete DS2 service set** of the Bosch EDC15M / DDE 3.0 (M47) ECU, as
decompiled in `~/Desktop/edc15m_out/research/` (diag_jobs.md,
routines_part3.md). The DDE 3.0 M47 job file (177 jobs, `DDE30.prg`) is the
reference "job catalogue": every service the jobs use has a handler, and
`src/jobs.rs` replays all of them as a test suite.

Source of truth for the service list:

* `research/diag_jobs.md` §1 — DS2 command dictionary (from the 177 jobs).
* `research/routines_part3.md` — DS2 handler map
  (dispatcher 0x290D4, one handler per command at 0x2654A–0x29090).
* `research/diag_id_map.csv` — 107-entry diag-ID → RAM table
  (flash 0x10400 of `e46_orig.bin`).
* `research/measurements.csv` — variable names, factors, units.
* This repo: `docs/wire-protocol.md` (framing, reply convention).

## Framing (recap, from wire-protocol.md)

* On the wire: `<payload> <XOR of payload>`; no B8/F1 envelope.
* Tester frames: `12 <len> <cmd> …` — ECU address `0x12`, `len` = total wire
  length **including** the trailing XOR byte.
* ECU replies: `12 <len> <SB> [<data …>]` + trailing XOR. `SB` (offset [2])
  is the JOBRESULT status byte: `A0` OKAY, `B0` generic ack/wait, `B2`
  out-of-sequence, `FF` NACK (command not supported). Data from offset [3]
  (fault/flash), [4] (block reads: one header byte at [3]), [5]
  (`0B 10`/`0B 11`: two-byte header `10 00`/`11 00` at [3..5]).

The `SimEcu::handle(&[u8]) -> Vec<u8>` API is unchanged: in = request
payload **without** the trailing XOR, out = reply payload **without** the
XOR. `Kline` adds the XOR and the echo bookkeeping.

## Service map (all handlers in `SimEcu::handle`)

| Telegram | Service | Reply data (offset [3] unless noted) |
|---|---|---|
| `12 04 00` | IDENT | ident ASCII string (42 chars, zero-padded) |
| `12 05 02 00/FF` | AIF read (programming record) | **46-byte** AIF record from cal 0x7CB2 (first of 14 46-byte slots), data at [3..49]; reply `12 32 A0 <46 B>` + XOR (LEN 50). Param byte unused by the ECU. Empty slot (first byte 0xFF) → short `12 05 A0 FF` (AIF_NICHT_PROGRAMMIERT) |
| `12 04 05` | clear fault memory | status only |
| `12 05 14 01` / `12 05 04 00` / `12 05 04 01` | fault memory read (count+header / entry / shadow) | 4-step chunked protocol, see `docs/dtc-fault-protocol.md` |
| `12 04 0A` | coding checksum (TDI V3.1) | u16 BE stored calibration checksum |
| `12 04 0D` | UPROG_EIN / test-code read (PRUEFCODE) | status only; enters programming mode |
| `12 04 0E` | Pruefstempel (test stamp) read | 3 bytes |
| `12 07 0F b1 b2 b3` | Pruefstempel write | status only |
| `12 05 0B 03` | measurement block MW (25 words) | 1 header byte [3], 25× u16 BE at [4..54], order = id list at cal 0x3AF04 |
| `12 05 0B 04` | digital input block | 1 header byte [3], 4 data bytes at [4..8]; bits table `BITS` below |
| `12 <5+2n> 0B 10 id…` | MW_SELECT read by diag id | 2-byte header `10 00` at [3..5], u16 BE per id (real-car confirmed) |
| `12 06 0B 12 NN` | PSG (pump controller) value select | status only |
| `12 05 0B 11` | PSG value read | 2-byte header `11 00` at [3..5], u16 BE (job: resp[5..6]) |
| `12 08 0C idH idL v m` | actuator override ("Steuern") | status only; m: `00` set value v, `55` hold, `AA` release; selective release = `… 30 55` (STEUERN_ENDE_SELECTIV), KWH off = `… 00 AA` |
| `12 05 40 NN` | adjustment value read | values directly at [3] (no header — sim convention, real layout unverified) |
| `12 07 41 NN hi lo` / `12 06 41 NN b` | adjustment value set (RAM copy) | status only |
| `12 05 42 NN` | adjustment value store → EEPROM | status only |
| `12 04 9E` | keep diagnostic session | status only |
| `12 04 9F` | end diagnostic, release overrides | status only |
| `12 08 90 42 4D 57 Z` | seed request (Z ≤ 0x28) | see seed/key below |
| `12 08 90 K0 K1 K2 K3` | key check | status only; sets unlocked |
| `12 05 6C 00` | EWS sync start | status only |
| `12 04 6D` | EWS sync status | status byte(s) |
| `12 09 06 A0 A1 A2 n` | FLASH_LESEN | n flash bytes at [3] (toolkit read path, real-car confirmed) |
| `12 09 06 05 A2 A1 A0 n` | 24-bit memory read (STATUS_MW5: 13 bytes @ 0x02FB56) | n bytes at [3] |
| `12 09 06 06 A2 A1 A0 n` | EEPROM read (24C02, 256 B) | n EEPROM bytes at [3] |
| `12 09 07 06 A0 A1 A2 n` | flash erase (n=0 → to end of image) | status only, needs programming mode |
| `12 09 07 02 A0 A1 A2 …` | flash write | status only, needs programming mode |
| `12 09 07 0E 42 41 50 00` | DATENBEREICH_LOESCHEN (BAP clear) | status only, clears fault memory |
| `12 09 07 0F …` | flash write end | status only |
| `12 0B 07 10 00 00 00 02 4B 9A` | STEUERN_TESTPLATZ (cmd 07 write-type, semantics unknown on the ECU; job sends after unlock) | status only (accepted if unlocked) |
| `32 04 00` | EGS (transmission, addr 0x32) ident probe (ECU_CONFIG job) | EGS ident ASCII |

Unknown service → `12 <len> FF` NACK (handler map 0x26900 in the decomp).

**Coverage status (2026-11-17, MPPS follow-up):** every service row above is
now answered by the fake ECU — including the previously-missing ones
(test stamp `0E`/`0F`, actuator override `0C`, EWS sync `6C`/`6D`, adjustment
values `40`/`41`/`42`, session keep `9E`, MW sub-services `03`/`04`/`11`/`12`,
and STEUERN_TESTPLATZ `07 10`) — so a foreign tool (MPPS/Galletto) no longer
hits a mid-session NACK and stalls waiting for a response it will never get.

## Seed/key (`0x90`)

From the SEED_KEY job bytecode (`research/diag_jobs.md` §1, reconstructed):

1. Tester sends `12 08 90 42 4D 57 Z` ("BMW" + Z, Z ≤ 0x28).
2. Already unlocked → **short ack** `12 05 A0 00 00` (job checks
   `resp[1]==5 && resp[3]==0`).
3. Otherwise the ECU answers a **seed telegram** whose data area is
   ≥ 0x2D bytes. The job computes the 4-byte key from the reply
   (`resp` = full telegram, `resp[0]==0x12`):
   `T[i] = resp[0x12+i] + resp[Z+i]`, `K[i] = T[i] + resp[0x29+i]` (byte
   adds, i = 0..3).
4. Tester sends `12 08 90 K0 K1 K2 K3`; ECU checks, answers status.
5. **Measured on the wire** (DDE3 sessions 2026-11-17, three Galletto
   runs): the keys the real job sends are
   `Z=0x07 → 39 3C 3F 42`, `Z=0x10 → 82 85 88 8B`, `Z=0x16 → B6 B9 BC BF`
   (the last byte of the key frame, e.g. `F2` after `… 42`, is the frame's
   trailing XOR — the key is exactly 4 bytes). These match the
   **unbiased** formula of step 3 byte-for-byte on all three vectors, so
   there is **no bias** in the DDE3 seed/key exchange. An earlier build
   assumed a `SEED_KEY_BIAS = 5` after misreading a capture (the `Z=0x10`
   vector happened to sit 5 below the formula on the sim's *old* seed
   table); the bias was removed 2026-11-17 and all three vectors —
   including the previously rejected `Z=0x07` — then unlocked on the first
   key send.

The sim implements the ECU side of this: the seed reply is a 46-byte seed
telegram `12 2E A0 <42 data bytes> <XOR>` (LEN 0x2E = 46 wire, the ≥0x2D
the job wants) whose data is a deterministic pattern seeded by Z. The
expected key is then computed **on the final 45-byte payload** with the
job's formula `K[i] = resp[0x12+i] + resp[Z+i] + resp[0x29+i]` (byte adds,
no bias) — so the key the sim will accept is exactly what a tester
computing the job's formula on the bytes it actually receives will send, for
**every** Z (including Z where the `resp[Z+i]` window overlaps the
header or the other key windows; both sides read the same final bytes). The
sim has **no "already unlocked" latch** (item 2 is the job's own check, not
sim behaviour): every `BMW` seed request gets a fresh seed telegram + fresh
expected key. An earlier build answered the short ack `12 05 A0 00` after
the first unlock, but the flashing tool *retries the whole exchange* and
does not stop on the short ack — it sent key `00 00 00 00`, got `B0` back
and stalled (capture 2026-11-17). Always issuing a fresh telegram is what
lets the retry proceed. The
ECU-side algorithm is grounded in the M47 decomp `sub_028B5E` (see
`research/out/disasm.txt`), which derives the accepted key from the bytes it
just placed in the reply plus the request's Z. A wrong key is answered with
`B0` and leaves the ECU locked (the ECU stays in the pending-key state until
the correct key arrives). Note the real ECU's key *material* (the CC0A..CC0D
/ CC21..CC24 constants in the decomp) is unknown; the sim's key only has to
be consistent with the job's reconstruction so the round-trip closes.

## Variable model (`0B 10` / MW block / PSG)

* Diag ids are **not** RAM addresses — the ECU translates id → RAM word via
  the table at flash 0x10400 (`research/diag_id_map.csv`, 107 entries).
  `SimEcu` embeds that table; unknown ids read 0.
* Default engine state = **engine off, cold-ish**, matching the real-car
  capture in `docs/wire-protocol.md` §21: N=0, boost/ADF 990 hPa,
  WTF 2854 (≈12.3 °C), battery 586 (≈11.9 V), PWG 0, all quantities 0.
  Named variables carry plausible values (factors/units from
  `research/measurements.csv`); values are plain u16 words in RAM,
  replies are big-endian.
* MW block (`0B 03`) word order (id list at cal 0x3AF04,
  `research/diag_jobs.md` §5): `0F10 0E10 0F55 0F52 0F53 0F8B 0F8A 0F80
  0F9A 0F60 0F34 0F40 0F32 0F30 0F30 0F08 0F00 0E11 0F65 0F67 0F61 0F62
  0F41 0F66 FFFE`.
* Digital block (`0B 04`) BITS bits (job table, data bytes at [4..8]):
  byte0 `S_PWGL 0x01 | S_BRT 0x10 | S_DIA 0x40`, byte1 `S_BRL 0x80 |
  S_PN 0x40`, byte2 `S_AC 0x01 | S_KO 0x02`, byte3 MFL
  `AUS 0x20 | TGL 0x80 | EIN+ 0x12 | EIN- 0x40 | WA 0x09`.
* PSG (`0B 12 NN`): NN = 0..10 → PHI_SV_IST, TV_SV, T_MV_ON, T_MV_OFF,
  SV_SOLL, AE_IWZ, AB_IWZ, ME_SOLL, FB_SOLL, D_AD_TK_N, D_AD_NW_N.

## Adjustment values (`40/41/42`)

Items (diag_jobs.md): 03 Startmenge, 04 Begrenzungsmenge, 05 LL-Regelung,
06 AGR Rueck, 07 Ladedruck, 09 Abgleichmenge (12 words), 0A FGR verbaut,
0B Kennung Klimaanlage, 13 Mengendrift (max+min), 12/14 checksum/aux bytes
for #09/#13, 15 Pumpenabgleich-Flag, 18 KIC, 99 timer (2 words).
`42` copies the RAM copy into the EEPROM model; `40` reads the RAM copy
(initialised from EEPROM at `SimEcu::new()`).

## EEPROM model

256-byte 24C02 image, layout (sim convention — the real layout is only
partially known from the jobs):

| Offset | Content |
|---|---|
| 0x7CB2–0x7CE0 | AIF programming record slot 0 (46 B; the 0x02 handler's source — cal area, in the 256 KB `flash` model) |
| 0x20–0x22 | Pruefstempel (`04 0E` / `07 0F`) |
| 0x30–0x7F | adjustment items (word 0 of item NN at 0x30+16·index; the 12-word item 09 spans 24 bytes) |

`06 06 A2 A1 A0 LEN` reads this image (looped while the ECU would answer
BUSY on a real ECU — the sim answers immediately).

## Job replay suite (`src/jobs.rs`)

`jobs.rs` encodes the DDE 3.0 M47 job sequences (the telegram lists from
`research/diag_jobs.md` §2) as data: each job = name + ordered telegrams +
per-step expectations (status byte, data shape, round-trip values).

* `cargo test --test ds2_jobs` — runs the whole catalogue against a fresh
  `SimEcu` and asserts every step.
* CLI: `dde3-toolkit ds2jobs [job]` — `job` optional; without it, runs the
  full suite and prints one PASS/FAIL line per job (exit status = failures).
  Use it to watch the sim behave when you add a service.

Jobs that are pure no-telegrams (INFO, HOLE_EMA_FELD) or pure table
lookups are listed with an empty sequence and PASS by construction; the
interesting set is the 21 telegram families.

## What is NOT in the sim

* The break-entered 10.4 k / 38.4 k second service protocol (0x1F6D2
  jump table) — low confidence in the decomp, and it lives on a different
  baud/channel (see `docs/break-mode.md`).
* CAN-side behaviour (PSG over CAN, MFL, IHKA): the sim answers the DS2
  shadow of it from RAM only.
* The `0x91` baud-change patch: the sim always answers the current baud;
  `baud` subcommand flows through as an ack.

## Real-car verification status

Real-car confirmed (wire-protocol.md §21): reply prefix `0x12`, `0B 10`
reply shape `12 <len> A0 10 00 <u16 BE…>`, ident envelope, fault 4-step
protocol, flash read/write/erase telegrams, PRUEFCODE/UPROG_EIN `12 04 0D`.
**Sim convention (to verify on the real ECU before trusting the data):**
MW block header byte at [3] and 25-word order (order from cal 0x3AF04,
header byte inferred), `0B 04` data at [4..8] (offsets from the
STATUS_DIGITAL bytecode: 4 bytes copied from S2[4..8]), `0B 11` data at
[5], adjustment reply "no header" assumption,
EEPROM layout, seed/key key-function, EGS ident string.
