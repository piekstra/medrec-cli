# AGENTS.md: working in this repo

This is the canonical agent guide. `CLAUDE.md` points here.

## What this is

`medrec-cli` (binary `medrec`) is a household medical-records orchestrator.
It files scans, documents, imaging studies and provider records into one
plain records folder ([docs/data-contract.md](docs/data-contract.md)) and
answers questions over them. It conforms to `piekstra-cli/1`
(`~/Dev/cli-common/DESIGN.md`) with `info.auth.required = false`: it holds no
credentials. Provider CLIs (mychart, kp, athena, uhc) fetch records; medrec
files them.

## Invariant 1: never read a real record

Medical data is the most sensitive data a household has, and an agent
session is a third-party service.

- Never read, print or summarize a real record, document, study or patient
  name: not from the user's records folder, not from a provider CLI, not
  from Drive. Do not run medrec against the user's real folder to look at
  output.
- Develop and dogfood on synthetic data only:
  `cargo run --example synth -- "$TMPDIR/medrec-dog"` writes scans, DICOM
  discs and a fake provider (`fakeprov`). Point a config at it with
  `XDG_CONFIG_HOME` and `XDG_CACHE_HOME` in that directory.
- A check against real data reports counts, statuses and exit codes only,
  or is left to the user in their own terminal.
- No MCP server, and no registration in `cli-mcp` (the family-cli shipping
  step is skipped on purpose): an MCP tool returns records to a model.

## Invariant 2: this repo is public

Nothing personal in code, fixtures, docs, commits, pull requests or issues
([PRIVACY.md](PRIVACY.md)). Fixtures are invented, not scrubbed
(`tests/fixtures/README.md`). `tests/no_personal_data.rs` fails on
personal-data shapes in tracked files. Never weaken it or `.gitignore`.

## Invariant 3: build it ourselves

No FHIR, SMART, DICOM or health-data libraries or SDKs, no aggregators, no
other people's MyChart or Kaiser integrations, and no cloud OCR or AI.
General-purpose crates are fine (clap, serde, reqwest, rustls, sha2, md-5,
quick-xml, lopdf), and so are local tools (poppler, tesseract, bsdtar).

## Layout

| File | What it owns |
|---|---|
| `src/main.rs` | The clap tree, output and exit codes |
| `src/store.rs` | The records folder's backend: an rclone remote behind the read cache, or a local directory |
| `src/folder.rs` | The folder layout, file naming, read-back writes, the lock and `rev` checks |
| `src/model.rs` | Every JSON format in the folder |
| `src/intake/` | Document intake: file types, text and OCR, detection with scores and evidence, duplicates |
| `src/review.rs` | Filing what intake held back |
| `src/import.rs`, `src/stream.rs`, `src/normalize.rs` | Provider import: the bounded subprocess, the `records-stream/v1` reader (medical-common's `pk-med-records`), FHIR to index records |
| `src/dicom.rs`, `src/imaging.rs`, `src/view.rs` | DICOM headers (medical-common's `pk-med-dicom`, re-exported), study intake and export, private copies and viewers |
| `src/views.rs`, `src/notes.rs`, `src/people.rs` | Timeline, search, record views, notes, people |
| `src/maint.rs` | `init`, `doctor`, `verify`, `backup`, `cache clear` |
| `src/testdata.rs` | The synthetic data generator (tests and `examples/synth.rs`) |

The stream reader and the `provenance/v1` types are medical-common's
`pk-med-records` (`src/stream.rs` and `src/model.rs` re-export them).
DICOM headers, DICOMDIR and the synthetic study writer are its
`pk-med-dicom` (`src/dicom.rs` re-exports it; feature `test-util` is on
because `src/testdata.rs` is built in normal builds). Every `pk-med-*`
crate is on one medical-common tag, and every `pk-cli-*` and `pk-med-*`
crate must resolve to one cli-common tag, or two copies of `pk-cli-core`
are built.

## Rules of the code

- A filed document, DICOM file or raw resource is never modified. Changes
  are new files, and an upload never replaces a file (`store::Existing::Keep`,
  `rclone copy --immutable`).
- On a local folder, medrec's folders are 0700 and its files 0600
  (`store::private_dirs`, `store::private_copy`).
- Every write is read back by MD5 before the index points at it. A moved
  source is removed only after that.
- A command that rewrites `household.json`, an `index.json` or
  `review.json` reads it fresh (`Reads::Fresh`) and holds the `FolderLock`.
- Arguments are validated before any rclone call or subprocess, so `--help`
  and bad input never touch the folder.
- One JSON document per invocation. A command that has printed its result
  and still fails (doctor, verify, backup, a partial intake) exits 1
  directly.
- Mutations that discard something (`review drop`) need `--force` without a
  terminal, and exit 6 otherwise.

## Commands

```sh
make verify      # fmt-check, clippy -D warnings, tests, smoke: the CI gate
make install     # signed release build into ~/.cargo/bin
cargo test --test flow   # the end-to-end flow on synthetic data
```

Tests never read the Keychain, never use the network and never use rclone.
OCR and poppler assertions are skipped when those tools are missing.

## Process

- Pull requests only; `main` is protected. Every pull request body ends with
  the line `twapp pr marker` prints, and every commit carries
  `--trailer "$(twapp pr marker --trailer)"`.
- `cr` (as `piekstra-dev`) reviews. Merge once it approves with checks green.
- Never force-push. Never set `core.hooksPath`, add `.pii-allowed` or opt out
  of the PII guard.
