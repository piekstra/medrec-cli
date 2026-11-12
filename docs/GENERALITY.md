# Generality: any household, any subset of sources

medrec grew around one household, but nothing in it should need that
household. This page states the contract that lets a different household use
it with whatever providers, documents and tools it has. It also lists where
the code still assumes less than a general household needs, with what that
household would need instead.

Keep this page current. A change that adds a single-household assumption adds
a row below, and a change that removes one deletes its row.

## The contract

- **Personal specifics live in the records folder, not in code.** People,
  birth dates, guardians, aliases, identifiers, organizations and sources are
  in `household.json`. Code holds the document-type vocabulary, detection
  rules, the FHIR normalizer and the folder layout.
- **Every source is optional.** A household with no provider CLI files scans
  and downloads, and imaging discs. A household with only a provider CLI
  imports and never scans. A view over a kind of record nobody has is an
  empty list, not an error.
- **Providers are found by profile, not by name.** Any CLI whose `info --json`
  lists `health-records/v1` and that writes `records-stream/v1` can be
  imported from (`medrec config set providers <cli>,<cli>`). The four names
  medrec probes by default (`mychart`, `kp`, `athena`, `uhc`) are a
  convenience, not a list of what is supported.
- **Every decision has a hand path.** What detection cannot decide goes to
  `_review/`, and `medrec review file` takes the person, type, date and
  organization by hand. `intake --person --type --date --org` overrides
  detection.
- **Every tool is optional except a place to keep the folder.**

  | Missing | What still works |
  |---|---|
  | rclone | A local directory as the records folder (`medrec init <dir>`), such as a Drive for desktop mount or an encrypted disk |
  | `pdftotext` (poppler) | Intake files by hand-given values; detection reads only file names |
  | `tesseract` | Text PDFs are read; scanned pages go to `_review/` |
  | `bsdtar` | Studies from folders and DICOMDIRs; `.zip` and `.iso` are refused |
  | Weasis | `images open --no-launch` prints the private copy's path; `config set viewer` picks another viewer |
  | FileVault (not macOS) | `doctor` reports `skip` and says to use full-disk encryption |

- **Any household shape.** Any number of people, adults or children. A child
  can have several guardians, and nobody has to be one. Two people can share
  a name; their birth dates tell them apart, and `--person` settles any tie.

## Where the code still assumes one kind of household

| Assumption | Where | What another household needs |
|---|---|---|
| Documents are in English | Document-type phrases and labels in `src/intake/classify.rs`; `ocr_languages` defaults to `eng` | Phrases per language, chosen by `ocr_languages`, and the tesseract language packs |
| Numeric dates are month first (`03/04/2026` is March 4) | `all_dates` in `src/intake/classify.rs` | A day-first setting, used when both orders are valid dates |
| Payer documents are US explanations of benefits and bills | The `eob` and `bill` types, `Claims/`, the EOB fields | Other payer document types, mapped to the same FHIR `ExplanationOfBenefit` fields |
| The records folder's remote reports MD5 | Every write is checked by MD5 (`check_md5` in `src/folder.rs`) | Remotes without MD5 (OneDrive, Dropbox, Box) fail the read-back. Comparing SHA-256 after a download would serve them at the cost of a download per write. |
| The machine is a Mac, or Linux with the same tools | `doctor`'s FileVault check, `open` and `xdg-open` for viewers, `/usr/bin/bsdtar` | A Windows build would need its own viewer launch, archive reader and disk-encryption check |
| Document organizations are matched by the names in `household.json` | Organization detection | Nothing more: add each organization's names as aliases |
