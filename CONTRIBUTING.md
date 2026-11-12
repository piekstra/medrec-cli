# Contributing

## Setup

```sh
brew install poppler tesseract rclone    # Linux: poppler-utils tesseract-ocr rclone libarchive-tools
make verify                              # fmt-check, clippy, tests, smoke
```

## Data

Use synthetic data only. `cargo run --example synth -- <DIR>` writes scans,
DICOM discs and a fake provider CLI. Never attach a real record, scan,
study, DICOM header or screenshot to an issue or pull request, scrubbed or
not ([PRIVACY.md](PRIVACY.md)).

A new fixture is invented: `Testperson` people, `Example` organizations,
dummy identifiers, DICOM UIDs under `2.25.0`. List it in
`tests/fixtures/README.md`.

## Changes

- One topic per pull request, with `make verify` green.
- A change to a file format in the records folder updates
  [docs/data-contract.md](docs/data-contract.md) in the same pull request.
  A change that breaks an existing folder raises `FORMAT` in `src/model.rs`.
- A change that adds a single-household assumption adds a row to
  [docs/GENERALITY.md](docs/GENERALITY.md).
- No new dependency on a FHIR, SMART, DICOM or health-data library, or on a
  network service ([AGENTS.md](AGENTS.md), invariant 3).
- Commit messages and pull request text carry no personal data.

## Security issues

Report them privately ([SECURITY.md](SECURITY.md)), not in a public issue.
