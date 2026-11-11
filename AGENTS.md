# AGENTS.md — guide for new agents/sessions

**What this is:** `dde3-toolkit` is a Rust CLI + egui GUI that talks to BMW
EDC15 "DDE 3.0" engine ECUs over the K-line (9600 8E1, DS2). It reads/writes
flash (256 KB or 32 KB calibration), reads/clears fault codes, shows live
data, verifies TDI V3.1 checksums, and logs the raw protocol. The protocol
is proprietary and was reverse-engineered — the findings in `docs/` **are**
the knowledge base; don't re-derive or guess against it.

## Read order

1. **`HANDOVER.md`** — current status + the task at hand. Start here.
2. **`docs/wire-protocol.md`** — authoritative on-wire facts (framing,
   `0x12` reply prefix, XOR, timing). Read before touching `protocol.rs`.
3. Other `docs/` as needed (see index in `HANDOVER.md`).

## Where things live

| Need | File |
|---|---|
| Git rules (commit format, when to commit) | `GIT.md` — commit after every major, verified change |
| Cutting a release (linux+windows in `dist/`) | `scripts/release.sh <version>` — see `GIT.md` "Releases" and README "Releases" |
| Handing over to the next agent | `HANDOVER.md` — when instructed, reduce it to: status + task at hand; push deep detail into `docs/` first |
| Wire facts | `docs/wire-protocol.md` |
| Fault/DTC protocol | `docs/dtc-fault-protocol.md` |
| Checksums / break-mode / 0x91 patch | `docs/checksum.md`, `docs/break-mode.md`, `docs/baud-0x91-patch.md` |
| RE background (VM, .prg, jobs) | `docs/reverse-engineering.md`, `tools/prg_vm.py` |
| Historical context | `docs/session-log.md`, `docs/SESSION_2026-11-09.md` |

## Working rules

- **Verify before committing:** `cargo build` clean, `cargo test` passes.
  Commit per `GIT.md`; update `HANDOVER.md` (and the matching `docs/`
  file) in the same commit when the change moves project state.
- **No hardware?** Use `--sim` (any subcommand) or the GUI `--demo` — the
  in-process ECU sim mirrors the real wire convention.
- **Never commit** runtime artifacts (captures, `*.log`, csv logs) — they
  are git-ignored on purpose.
- **Don't change framing assumptions** (`B8`-free, `0x12` prefix) without a
  real-car capture; `docs/wire-protocol.md` is what real captures proved.
- Reference tools: `tools/monitor.py` (passive wire tap), `tools/prg_vm.py`
  (WinkFP VM), `tools/gen_dtc_rs.py` / `gen_live_rs.py` (regenerate
  `src/dtc.rs` / `src/live.rs`).
