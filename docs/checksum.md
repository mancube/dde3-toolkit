# Flash checksums (TDI V3.1)

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

