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


## 5b. Cold-start program chain — the second code-area check (session 14)

The TDI slots above are only half of the code-area story. The boot self-test
(`0x11708-0x11736`, verified on `e46_orig.bin`) hashes **DPP2 pages 4..0xD**
(`0x10000-0x37FFF`) and then `0x38000-0x38B7F` through the 16-bit window with
the TDI step, carrying state across pages, and **resets (log 0x1F, code 9)**
unless the final state is exactly `h=0x8631 g=0xEFCD`.

The two balancing words at `0x38B7C` (directly after the calibration block id
`23A6` at `0x38B7A`) make it end there. They are identical in all 31 BMW
calibration releases. Consequences:

- **Any edit in `0x10000-0x38B7F` changes the chain**, even when it is outside
  both TDI slots — e.g. the `0x91` baud hook at `0x293D0`, the EWS gate at
  `0x2A65E`. Correcting only the TDI slots leaves such an image in a reset
  loop; this was the original baud-patch bug.
- The TDI **code slot `0x13FFC` lies inside the chain range**, so the chain
  must be solved **after** the TDI slots. `checksum::correct()` now enforces
  that order; the three fields do not overlap, so it is not circular.

`src/checksum.rs` ports `progsum.py` (`prog_run` / `prog_solve`) and exposes:

- `program_chain(buf) -> Option<Checksum>` — `stored` is the 4 bytes at
  `0x38B7C`, `computed` is the word that makes *this exact buffer* end at the
  target, `ok` is `stored == computed` (what the ECU would do).
- `verify()` appends the chain row for a full image that also has a real TDI
  code block — so a 32 KB calibration read padded with `0xFF` (which carries
  the `23A6` id) is never mistaken for a full image and never gets the padded
  program area written.
- `correct()` writes the TDI slots first, then solves the chain over the
  corrected buffer. It is idempotent.

Validation: `progsum.py` cross-check on the LCG test vector (`fc520cf8`), a
stale-code-slot test proving order matters, a padded-partial test proving no
chain row/write, and byte-for-byte `OK` on the real images (`e46_orig.bin` +
all four shipped patch images). The CLI `checksum` subcommand and the GUI
Checksum tab pick the row up with no changes — it is a normal 4-byte slot at
`0x38B7C` for the "Stored (edit) / Apply / Fix" path.
