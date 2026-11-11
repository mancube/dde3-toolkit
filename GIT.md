# Git workflow — dde3-toolkit

Rules for keeping this history useful for the next person (or agent) who
picks up the work.

## Commit after every major progress or change

Do not batch unrelated work into one commit. When a meaningful unit of
progress is done and verified, commit it:

- a protocol fix or new service implemented,
- a finding written up in `docs/`,
- a GUI feature, a tool in `tools/`,
- a handover/status update in `HANDOVER.md`.

A "major change" is anything that changes behaviour, adds a feature, fixes
a bug, or changes what the next session needs to know.

## Message format

```
<type>: <imperative summary, ≤ 72 chars>

Optional body: what changed and why, one blank line after the subject.
Keep it short — the docs/ files carry the detail.
```

Types:

| Type | Use for |
|---|---|
| `feat` | new feature (CLI subcommand, GUI tab, protocol service) |
| `fix` | bug fix (protocol parsing, checksum, GUI) |
| `docs` | README / docs/ / HANDOVER changes only |
| `test` | new tests, sim fixtures |
| `refactor` | no behaviour change |
| `chore` | build, deps, .gitignore, tooling |

Examples (from this repo's actual history):

```
fix: checksum false-BAD — marker key read contiguous 0x3FFA/0x3FFB pair
feat: GUI parity with dde4 v0.1.5 — embedded font, centered button labels
docs: README rewrite + HANDOVER §18 (2026-11-11 status)
```

## Before committing

1. `cargo build` is clean and `cargo test` passes (mention test count in the
   body when it changed).
2. No runtime artifacts in the commit: captures, `*.log`, `dde3_log.csv`,
   `dde3newlog.txt` are git-ignored on purpose — `git status` first.
3. If the change updates protocol knowledge, update the matching file in
   `docs/` **in the same commit** so code and docs never drift.
4. If the change moves the project to a new state the next session must
   know, update `HANDOVER.md` (current status + "task at hand") **in the
   same commit**.

## Releases

Releases are **git-tagged binary bundles in `dist/`**, not CI artifacts:

1. `scripts/release.sh <version>` does everything: bumps the `Cargo.toml`
   version, builds the Linux and Windows release binaries into `dist/` with
   `.sha256` files, commits `dist/` + `Cargo.toml`, tags `v<version>`,
   pushes branch + tag, and creates the **Gitea release** (via `gt`) with
   the four `dist/` files as assets.
2. Release commit message is always `release: v<version>` — the changes
   themselves are committed separately beforehand, per the rules above.
3. Never hand-edit files in `dist/`; only the release script touches them.
4. Prebuilt binaries consumers get by checking out the tag — see the
   "Releases" section of `README.md`.
5. Version labels: the GUI footer and the CLI usage line both print
   `env!("CARGO_PKG_VERSION")`, i.e. the same `Cargo.toml` the script
   bumps — nothing else to touch when releasing.

## Branches / pushing

- `main` is the only branch; push with `git push origin main`.
- Remote: `origin` → `https://git.nas.lan/ikac/dde3-toolkit.git`.
- If you need to reset the remote with force-push, ask the user first.
