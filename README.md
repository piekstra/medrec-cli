# medrec

A household's medical records in one plain, private Google Drive folder.
`medrec` files scans and downloaded documents, imaging studies from CDs and
download links, and the records that provider and insurer CLIs fetch. It then
answers questions over all of it: a timeline, search, results, medications,
conditions, allergies, immunizations and notes.

`medrec` holds no provider credentials and makes no network calls of its
own, except to Google Drive through `rclone` and to GitHub for `self-update`.
Provider CLIs fetch records; `medrec` files them.

Part of the `piekstra-cli/1` family. It is built on
[cli-common](https://github.com/piekstra/cli-common) and
[medical-common](https://github.com/piekstra/medical-common).

> Unofficial. `medrec` is not affiliated with or endorsed by Google, Epic,
> Kaiser Permanente, athenahealth, UnitedHealthcare or the Weasis project.
> It is not a medical device and gives no medical advice.

## Install

```sh
make install                          # from a checkout, signed for macOS
brew install poppler tesseract        # text extraction and OCR (local)
brew install rclone                   # the Drive folder
```

Weasis (https://weasis.org) is the default DICOM viewer. Another viewer is
set with `medrec config set viewer <app or executable>`, or `none` to only
copy a study.

## Set up

```sh
rclone config                                   # a Drive remote, e.g. `gdrive`
medrec init "gdrive:Medical Records"            # creates the folder layout
medrec people add "Alex Example" --birth-date 1990-01-01 --adult
medrec people add "Robin Example" --birth-date 2020-02-02 --guardian Alex
medrec doctor                                   # FileVault, rclone, free space, tools
```

`init` adopts an existing medrec folder, and refuses a folder that holds
other files unless given `--allow-other-files`. The folder's layout and every file format
are in [docs/data-contract.md](docs/data-contract.md).

## Use

```sh
medrec intake ~/Downloads/lab.pdf               # read, classify, file
medrec intake --inbox                           # drain the folder's inbox/
medrec intake --drive "gdrive:Old scans"        # the back catalogue, copied
medrec review list                              # what intake could not decide
medrec review file <id> --person Robin --type lab_report --date 2026-03-04

medrec images intake /Volumes/CDROM             # a disc, folder, DICOMDIR, .zip or .iso
medrec images list
medrec images open <id>                         # a private copy, opened in Weasis

medrec providers list                           # provider CLIs on PATH
medrec import --from mychart --patient <id> --person Robin

medrec timeline --person Robin --since 2026-01-01
medrec search hemoglobin
medrec results --code 718-7
medrec meds --active
medrec note add Robin "Ask about the recheck" --tag question --about <record id>

medrec verify                                   # every indexed file against the index
medrec backup --to "/Volumes/Backup/Medical Records"
```

Every command takes `--json` (one document on stdout) and `--help`. Every
list (`timeline`, the record views, `documents list`, `note list`,
`images list`, `review list`, `people list`) takes `--limit N`; it is whole
by default, and `search` stops at 50 hits. Exit codes: 0 ok, 1 a check
failed, 2 usage, 3 a provider needs a sign-in, 4 not found, 5 upstream, 6
confirmation required.

### Settings

`medrec config set <key> <value>`, kept in `~/.config/medrec/config.json`:

| Key | What it sets | Default |
|---|---|---|
| `folder` | the records folder, `<remote>:<path>` or a local directory | set by `init` |
| `mount` | where Drive for desktop mounts the folder, for `doctor` to compare | none |
| `inbox` | the inbox `--inbox` drains | `inbox/` in the folder |
| `me` | the person id who writes notes on this machine | none |
| `viewer` | the DICOM viewer, or `none` | Weasis |
| `ocr_languages`, `ocr_max_pages` | tesseract languages, and pages read per document | `eng`, 20 |
| `max_file_mb` | the largest file intake or import keeps, MiB | 200 |
| `import_max_mb` | the largest records stream one import reads, MiB; an import holds it until the trailer | 4096 |
| `import_timeout_secs`, `rclone_budget_secs` | how long one provider export, and all of one command's rclone calls, may take | 1800, 1800 |
| `providers` | provider CLIs to consider besides the known ones | none |

### What intake copies and what it moves

| Source | Policy | Why |
|---|---|---|
| the inbox (`--inbox`) | move | the inbox is a drop box; a filed document leaves it |
| a file under `~/Downloads` | move | a download is a transient copy |
| a file given with `--move` (the organize-scans hand-off) | move | the caller asked |
| a Drive folder (`--drive`) | copy | the back catalogue stays where it is |
| any other local file | copy | medrec does not delete what it did not receive |

`--keep` turns every move into a copy, and a symlink is always copied. A
moved file is removed only after the filed copy's MD5, as Drive reports it,
matches, and the index that points at it has been written and read back.

### How a document is filed

Intake extracts text locally, with `pdftotext` first and `tesseract` for
scanned pages. It then detects the person, date, document type and
organization, each with a score and the evidence behind it. A document is
filed when all four are certain, and goes to `_review/` otherwise. Values
given on the command line win over detection. Values from another classifier
(`--classified-by organize-scans`) are checked: when medrec is certain of
something else, the document goes to `_review/` with both answers.

Duplicates are found by SHA-256 and by Drive's MD5, so a duplicate in Drive
needs no download. Re-scans of the same page are linked as near-duplicates,
by text similarity.

### Imaging

A study's DICOM files are kept byte for byte. A study is identified by its
StudyInstanceUID and each image by its SOPInstanceUID, so a second disc of
the same study adds only the images not yet filed. `images open` copies the
study to a private cache folder (mode 0700, removed after 12 hours) and hands
the viewer that local folder. A disc's bundled viewer is never run.

## Not in this version

- `claims`, `costs`, `summary` and `export` (PLAN phase 2).
- The Huishouden bridge. People carry link fields for it, and medrec writes
  nothing to Huishouden.
- An MCP server. medrec is not registered in `cli-mcp` and has no `mcp`
  command, because an MCP tool would return records to a model, which is a
  third-party service.

## Docs

- [SECURITY.md](SECURITY.md): the threat model (Google, this Mac, a lost
  laptop, a lost rclone token or Google account).
- [PRIVACY.md](PRIVACY.md): what this public repo may and may not contain.
- [docs/data-contract.md](docs/data-contract.md): the folder layout and
  formats.
- [docs/GENERALITY.md](docs/GENERALITY.md): any household, any subset of
  sources.
- [AGENTS.md](AGENTS.md) and [CONTRIBUTING.md](CONTRIBUTING.md): working on
  medrec.

## License

MIT or Apache-2.0, at your option.
