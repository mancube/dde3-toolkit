# MPPS vs Galletto vs dde3-toolkit — calibration-area read/write

**Date:** 2026-12-29
**Question:** for the **calibration area** (`0x38000..0x40000`, 32 KB), how do
MPPS, Galletto, and the dde3-toolkit actually behave on the wire — and does the
toolkit need to change anything?

**Sources (all `respond` fake-ECU captures, so RX-in-log = the tool's request):**

| File | Tool | Operation |
|---|---|---|
| `captures/mpps capture/mppsread.txt` | MPPS | calibration **read** |
| `captures/mpps capture/mppswrite.txt` | MPPS | calibration **write** |
| `captures/galletto capture with respond tool/testread.txt` | Galletto | full **read** |
| `captures/galletto capture with respond tool/fakewritegalleto.txt` | Galletto | full **write** |
| `src/protocol.rs` | toolkit | `read_full`/`read_range`, `write_full`/`write_calibration` |

Analyze with: `dde3-toolkit log sum <capture.txt>`.

Constants that anchor everything: `CAL_START=0x38000`, `CAL_LEN=0x8000`
(→ calibration is exactly `0x38000..0x40000`), `READ_CHUNK=0x80` (128 B),
`WRITE_BLOCK=0x40` (64 B), `FLASH_WRITE_START=0x10000`, `IMAGE_SIZE=0x40000`.

---

## 1. What each tool actually does

### MPPS calibration **read** (`mppsread.txt`, 268 requests)

```
IDENT, IDENT, UPROG_EIN, AIF_LESEN(02), FS_LESEN(04), FS_LOESCHEN(05)   ← session A: DTC read+clear
IDENT, IDENT, UPROG_EIN, AIF_LESEN(02), SEED_KEY(Z=0x1D), KEY           ← session B: unlock
FLASH_LESEN ×256  span 0x038000..0x040000  block=128                     ← cal only
```

- Reads **only the calibration region** `0x38000..0x40000` (32 KB), 128-B
  chunks, 256 reads. Does **not** read the full 256 KB.
- Session A does a **DTC read (`04`) + DTC clear (`05` sub 0x13)** before the
  flash work — Galletto and the toolkit do **not** do this.
- Unlock uses **seed Z=0x1D**.

### MPPS calibration **write** (`mppswrite.txt`, 168 requests)

```
IDENT, IDENT, UPROG_EIN, AIF_LESEN(02), FS_LESEN(04), FS_LOESCHEN(05)   ← session A: DTC read+clear
IDENT, IDENT, UPROG_EIN, AIF_LESEN(02), SEED_KEY(Z=0x1D), KEY           ← session B: unlock
FLASH_LESEN ×6   (region-table reads: 0x03FF60/2, 0x03FF46/1, 0x037FB0/12,
                 0x03FF47/1, 0x03FCB2/46, 0x03FCE0/46)                  ← layout verify
FLASH_SCHREIBEN_ENDE(07 0F) @0x03FCE0  ×2                                ← write-end, BEFORE erase
FLASH_LOESCHEN(07 06) @0x038000                                             ← erase cal
FLASH_SCHREIBEN(07 02) ×147 span 0x038000..0x040000  block=[224, 64]     ← 146×224 + 1×64
```

- Writes **only the calibration region** `0x38000..0x40000`.
- **224-byte (0xE0) write blocks** — the frame max — with a final 64-byte tail
  (146×224 + 1×64 = 32768). **Not** 64-byte blocks.
- **Sends `FLASH_SCHREIBEN_ENDE` (`07 0F`) twice at `0x03FCE0`, *before* the
  erase** — the session-31 open question about the `07 0F` address is answered
  by this capture: MPPS uses **`0x03FCE0`**.
- Reads the **region/segment table** (6 small `FLASH_LESEN`) before writing —
  a layout-verification step the toolkit skips.
- **No `DIAGNOSE_ENDE`** at the end — the capture ends on the last
  `FLASH_SCHREIBEN` (0x03FFC0, 64 B → 0x40000).
- Unlock uses **seed Z=0x1D**.

### Galletto **read** (`testread.txt`, 2053 requests)

```
IDENT, UPROG_EIN, AIF_LESEN(02), SEED_KEY(Z=0x07), KEY
FLASH_LESEN ×2048  span 0x000000..0x040000  block=128                     ← FULL 256 KB
```

- Reads the **entire 256 KB** (0x000000..0x40000), 128-B chunks.
- No DTC read/clear, no region-table reads, no `07 0F`.

### Galletto **write** (`fakewritegalleto.txt`, 3096 requests) — two sessions

```
# phase 1: program area
IDENT, UPROG_EIN, [IDENT, UPROG_EIN], IDENT, UPROG_EIN, AIF_LESEN(02), SEED_KEY(Z=0x10), KEY
FLASH_LOESCHEN @0x010000
FLASH_SCHREIBEN ×2560  span 0x010000..0x038000  block=64
DIAGNOSE_ENDE
# phase 2: calibration area
IDENT, UPROG_EIN, [IDENT, UPROG_EIN], IDENT, UPROG_EIN, AIF_LESEN(02), SEED_KEY(Z=0x16), KEY
FLASH_LOESCHEN @0x038000
FLASH_SCHREIBEN ×512  span 0x038000..0x040000  block=64
DIAGNOSE_ENDE
```

- **Two sessions**, each: bootstrap → erase → 64-B blocks → `DIAGNOSE_ENDE`.
- **No `07 0F`** anywhere.
- Phase 2 is the calibration area `0x38000..0x40000`, 512 × 64-B blocks.

### dde3-toolkit (`src/protocol.rs`)

- **`read_full` / `read_range`** → `read_flash`: `begin_flash_session()`
  (IDENT → UPROG_EIN → AIF read(02) → SEED_KEY **Z=0x07** → key) then
  `FLASH_LESEN` **128-B** chunks. `read_full` = 0..0x40000 (256 KB);
  `read_range(CAL_START, CAL_LEN)` = calibration only.
- **`write_full`**: two phases `[0x10000,0x38000)` then `[0x38000,0x40000)`;
  each: `begin_flash_session()` → erase @start → **64-B** blocks →
  `DIAGNOSE_ENDE`.
- **`write_calibration`**: `begin_flash_session()` → erase @0x38000 → **64-B**
  blocks → `DIAGNOSE_ENDE`.
- **No `07 0F`** (deliberate, session 31), **no DTC read/clear** in the
  bootstrap, **no region-table reads**.

---

## 2. Side-by-side (calibration area)

| Aspect | MPPS | Galletto | Toolkit |
|---|---|---|---|
| **Region** | cal only `0x38000..0x40000` | cal = phase 2 of full write | cal via `read_range`/`write_calibration`; full via `read_full`/`write_full` |
| **Read chunk** | 128 B | 128 B | 128 B (`READ_CHUNK`) |
| **Write block** | **224 B** (+64 tail) | 64 B | **64 B** (`WRITE_BLOCK`) |
| **Sessions** | 2 (DTC session + flash session) | 2 (program + cal), re-auth each | 2 in `write_full` (program + cal), re-auth each |
| **DTC read/clear in bootstrap** | **yes** (`04`+`05`) | no | no |
| **Region-table pre-read** | **yes** (6 reads) | no | no |
| **`07 0F` write-end** | **yes, ×2 @0x03FCE0, before erase** | no | no |
| **End-of-write signal** | none (ends on last block) | `DIAGNOSE_ENDE` per phase | `DIAGNOSE_ENDE` per phase |
| **Seed Z** | 0x1D | 0x07 (read) / 0x10, 0x16 (write) | **0x07** (hardcoded) |
| **AIF read (02)** | yes (both sessions) | yes | yes |

---

## 3. Discrepancies that matter (and ones that don't)

1. **Write block size — 224 B (MPPS) vs 64 B (toolkit).** MPPS writes the
   calibration area in 224-byte blocks; the toolkit uses 64. This is the single
   biggest behavioral gap. **Caveat:** the MPPS capture is against the *fake*
   ECU, which accepts any block size. The on-car 192-byte experiment
   (session 33) hung on the real ECU, so "the real car accepts 224-byte blocks"
   is **unproven**. 64 B is the known-safe size (Galletto uses it and it works
   on-car). → Do **not** raise `WRITE_BLOCK` to 224 without an on-car test.

2. **`07 0F` write-end.** MPPS sends it (×2 @0x03FCE0, before erase); Galletto
   and the toolkit don't. The toolkit deliberately omits it (session 31). Galletto
   — which works on-car — also omits it, so omitting is *safe*; MPPS's use of it
   is an alternative, not a requirement. The capture **does** resolve the
   session-31 open question: the address MPPS uses is **`0x03FCE0`**.

3. **End-of-write signal.** MPPS ends on the last block (no `DIAGNOSE_ENDE`);
   Galletto and the toolkit send `DIAGNOSE_ENDE`. The toolkit matches Galletto
   (the on-car-proven tool), so this is fine.

4. **DTC read/clear + region-table pre-read in the bootstrap.** MPPS-only.
   Neither Galletto nor the toolkit does it, and Galletto works on-car, so this
   is not required. (It *is* a nice-to-have: MPPS clears faults as part of the
   flash prep.)

5. **Seed Z.** A free parameter (≤ 0x28); the key formula works for any Z.
   Toolkit's hardcoded `0x07` is valid (it's literally one of Galletto's
   on-car vectors). MPPS's `0x1D` is equally valid. **Not a bug.**

6. **Read scope.** MPPS reads cal-only; Galletto reads full; toolkit does both
   (`read_range` for cal, `read_full` for full). **No gap** — the toolkit
   already covers both.

---

## 4. Bottom line

- The toolkit's calibration read/write **already matches Galletto** (the
  on-car-proven reference) in every respect that matters: 128-B reads, 64-B
  writes, two-session re-auth on full write, `DIAGNOSE_ENDE` close, no `07 0F`.
- The **only** places MPPS differs in a way that could be a feature gap:
  - 224-B write blocks (faster, but unproven on-car — risky to adopt blindly),
  - `07 0F` @0x03FCE0 (MPPS-only; Galletto proves it's optional),
  - DTC read/clear + region-table pre-read in the bootstrap (MPPS-only niceties).
- **Recommendation:** no change is *required* for correctness. If you want an
  MPPS-style mode, the safe, low-risk additions are (a) a DTC read/clear in the
  flash bootstrap and (b) a region-table pre-read — both are additive and match
  what MPPS does *before* it touches flash. Raising the write block to 224 B or
  adding `07 0F` should only be done behind an on-car test.
