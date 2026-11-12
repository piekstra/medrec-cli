# Privacy: this repository is public

This repository is public. Everything committed here, in code, fixtures,
docs, commit messages, pull requests and issues, is published. It carries no
personal data, and no data derived from a real record.

Runtime is different. Real names, birth dates, results and diagnoses flowing
through medrec into the records folder are the point of the tool. They stay
in the records folder ([docs/data-contract.md](docs/data-contract.md)), on
the user's own Drive, and nowhere else.

## Never committed

- A real record, document, scan, imaging study, DICOM header or screenshot,
  under any extension, scrubbed or not.
- A real provider or insurer response, including one with values replaced.
  The fixtures are invented, not scrubbed captures.
- Names, birth dates, addresses, phone numbers and email addresses of real
  people, including clinicians.
- Medical record numbers, member and subscriber ids, claim numbers,
  accession numbers, and DICOM UIDs from real studies.
- Real dollar amounts from bills or EOBs, and real dates of service.
- Credentials, tokens, cookies, `rclone.conf`.

## Where examples come from

- People are `Testperson` (Alex, born 1990-01-01, and Robin, born
  2020-02-02), or `Example` in prose.
- Organizations are `Example Health`, `Example Clinic`, `Example Imaging`,
  at `example.org` and `example.com`.
- Identifiers are obvious dummies: `pat-0001`, `MEMBER-0000000001`.
- DICOM UIDs are under `2.25.0`, which no real device issues.
- Clinical codes (LOINC, ICD-10, RxNorm, CVX, CPT) are real, because they are
  public vocabularies that identify no one.

[tests/fixtures/README.md](tests/fixtures/README.md) lists the fixtures.

## Enforcement

1. `tests/no_personal_data.rs` walks `git ls-files` and fails on
   personal-data shapes: phone-like and long digit runs, email addresses,
   street addresses, and DICOM UIDs outside the synthetic and DICOM-standard
   roots. It checks shapes, not a list of real values, because such a list
   would publish what it protects.
2. `tests/fixture_shapes.rs` checks that every name and birth date in the
   records-stream fixture is a known synthetic one.
3. `.gitignore` blocks `*.dcm`, captures, `*.har` and raw dumps.
4. CI runs gitleaks on every pull request.
5. Before a commit, read the diff for anything that looks real.

## Development sessions

Development, including any by an AI agent, uses synthetic data only
(`cargo run --example synth -- <DIR>` makes a full set). A check against a
real records folder reports counts, statuses and exit codes, never contents,
or is run by the user in their own terminal.
