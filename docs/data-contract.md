# Data contract: the records folder

The records folder is one directory tree, normally a private Google Drive
folder reached through an rclone remote (`gdrive:Medical Records`), or a
local directory. medrec reads and writes it only through this contract.
Every file is plain: PDFs, images and DICOM as received, JSON, and Markdown.
A person can open any of it in the Drive app without medrec.

## Layout

```text
README.md                     what this folder is, and its rules (written by `init`)
household.json                people, organizations, sources
inbox/                        drops waiting for `medrec intake --inbox`
_review/                      documents intake could not file, and review.json
<Person>/
  notes.md                    dated notes; this file is their source
  index.json                  the person's index (documents, records, studies, imports)
  text.json                   extracted text per document
  Documents/<YYYY>/<YYYY-MM-DD>_<type>_<org>_<details>_<original name>
  Claims/<YYYY>/              EOBs and bills, named the same way
  Imaging/<YYYY-MM-DD>_<modality>_<body part>_<facility>/
    S<series:03>/<instance:05>.dcm   DICOM files exactly as received
    study.json                       the study's summary (medrec-study/v1)
  Records/<source>/<ResourceType>/<id>.json   FHIR R4 resources, verbatim
```

- `<Person>` is the person's name when they were added (`Robin Example`). It
  never changes afterwards, even when the person is renamed; `household.json`
  maps the person's id to it.
- `<source>` is the provider CLI and organization, as `<cli>-<org slug>`
  (`mychart-example-health`).
- A file name's descriptors are lowercase ASCII words joined by `_`: the type
  is at most 24 characters, the organization and details at most 40, and the
  original name's stem at most 80. The original name is kept as the suffix so
  a filed document can be traced to what was received.
- A study folder without a date is `undated_…`. A DICOM file whose series or
  instance number is missing is named by its SOPInstanceUID.
- A raw resource that changed at its source is written as the new
  `<id>.json`. The previous version stays beside it as
  `<id>~<first 8 hex of its MD5>.json`.

## Rules

1. **Filed bytes are never changed.** Documents, DICOM files and raw
   resources are written once and never edited. A re-scan, a second disc or
   a corrected record is a new file. An upload never replaces a file: one
   already at its name with the same bytes is taken as that upload (a run
   that stopped before writing its index left it there), and one with other
   bytes stops the command. A new study never shares a folder that exists
   in Drive but not in the index.
2. **Every write is read back.** A file is uploaded, its MD5 (as Drive
   reports it) is compared with the local one, and only then does the index
   point at it. A moved source is removed only after that.
3. **One writer at a time per machine**, through a lock file under the
   per-machine cache. Across machines, `household.json`, `index.json` and
   `review.json` carry a `rev` counter. A write whose `rev` no longer matches
   the folder's is refused, and the command is run again. Import and
   `images intake` check the `rev` before uploading, too, and save each
   study's index entry as soon as it is filed.
4. **A newer layout is refused.** `household.json` declares `format: 1`. A
   medrec that finds a larger number stops and says to update.
5. **Rename or move files through medrec**, so the index stays right.
   `medrec verify` reports any file that is missing, changed (by MD5, or by
   SHA-256 with `--deep`) or not in an index.

## household.json (`medrec-household/v1`)

```json
{
  "schema": "medrec-household/v1",
  "format": 1,
  "folder_id": "f_…",
  "rev": 7,
  "created_at": "2026-01-01T00:00:00Z",
  "people": [{
    "id": "p_…", "name": "Robin Example", "folder": "Robin Example",
    "birth_date": "2020-02-02", "adult": false, "guardians": ["p_…"],
    "aliases": ["Robbie Example"],
    "identifiers": [{"source": "mychart:example-health", "value": "pat-0001", "kind": "patient"}],
    "links": {"huishouden_health_person": "…", "baby_profile": "…"},
    "created_at": "…"
  }],
  "organizations": [{"id": "g_…", "name": "Example Health", "aliases": [], "fhir_base": "https://fhir.example.org/r4"}],
  "sources": [{
    "id": "s_…", "kind": "fhir", "person": "p_…", "cli": "mychart",
    "organization": "example-health", "organization_name": "Example Health",
    "fhir_base": "…", "patient_id": "pat-0001", "last_pull": "…"
  }]
}
```

Ids are a one-letter kind (`p` person, `g` organization, `s` source, `d` document,
`r` record, `m` study, `i` import, `n` note, `q` review item, `f` folder), `_`, and 20 random base-32
characters (100 bits). An id names nothing about the person or the record. `links`
are fields only: medrec reads and writes nothing in Huishouden.

## index.json (`medrec-index/v1`)

One per person: `{schema, person, rev, documents, records, studies,
imports}`.

**Document**, a filed file (FHIR `DocumentReference` and `Binary`):
`id`, `path`, `sha256` (its identity), `md5` (Drive's hash, for `verify`
without a download), `size`, `media_type`, `pages`, `original_name`, `title`,
`doc_type`, `loinc`, `date` (of service), `organization`, `filed_at`,
`origin` (`{kind: file|inbox|drive|fhir|review, name, action: copied|moved}`),
`text` (`{chars, engines, ocr_pages, note}`), `near_duplicates`
(`[{document, similarity}]`), `minhash`, `classification` (what was detected,
with scores and evidence), and `provenance`.

Document types: `visit_summary`, `lab_report`, `imaging_report`,
`immunization_record`, `discharge_summary`, `referral`, `prescription`,
`letter`, `bill`, `eob`, `insurance_card`, `other`.

**Record**, one normalized clinical fact: `id`, `kind` (`encounter`,
`result`, `report`, `condition`, `medication`, `allergy`, `immunization`,
`procedure`, `coverage`, `claim`), `key` (kind, code and date, so the same
fact from two sources is one record), `date`, `code` (`{system, code,
display}`), `display`, `status`, `fields` (kind-specific, with FHIR element
names), `documents` (linked document ids) and `provenance` (one per source).

**ImagingStudy**: `id`, `study_uid`, `path`, `date`, `modalities`,
`body_part`, `description`, `facility`, `accession`, `referring`, `series`
(`[{uid, number, modality, description, instances: {SOPInstanceUID: file}}]`),
`instances`, `size`, `report` (a document or record id), `files` (each
filed file's MD5 by its path in the study folder, for `verify`), `filed_at`,
`provenance`.

**ImportRun**, one `medrec import`: `id`, `source_dir`, `stream_sha256`,
`stream_bytes`, `cli`, `cli_version`, `organization`, `patient_id`,
`pulled_at`, `imported_at`, `counts`, `skipped`, and the added, updated,
unchanged and written counts.

**Provenance** (the `provenance/v1` fields of medical-common's
`pk-med-records`): `source` (`{kind, cli, cli_version, organization_id,
fhir_base}`), `method` (`fhir-api`, `portal`, `scan`, `file`, `media`,
`manual`), `source_resource` (`{type, id, version_id, last_updated}`),
`fetched_at`, `filed_at`, `raw_path` (the verbatim file), `classified_by`
(`medrec`, `organize-scans`, `hand`), `derived_from`, `filed_by` (`medrec
<version>`).

## text.json (`medrec-text/v1`)

`{schema, person, texts: {<document id>: <text>}}`. It is kept apart from the
index so listing documents never downloads every page of text.

## _review/review.json (`medrec-review/v1`)

`{schema, rev, items}`. Each item: `id`, `path` (`_review/<date>_<name>`),
`sha256`, `md5`, `size`, `media_type`, `pages`, `original_name`,
`received_at`, `origin`, `reasons` (each field that was unsure, or a
disagreement with `classified_by`), `text`, `text_info`, `minhash`,
`classification`, `classified_by`.

## study.json (`medrec-study/v1`)

The index's ImagingStudy and `person`. It carries no patient name, birth
date or patient id from the DICOM headers; those stay in the DICOM files.

## notes.md

```markdown
# Notes: Robin Example

## 2026-03-05 (n_…)
Tags: question, follow-up
About: d_…

Ask about the recheck at the next visit.
```

Notes are sorted by date. The `Tags:` and `About:` lines are optional.
`notes.md` is the notes' only store; a note edited by hand in this format is
read back as edited.

## Import input: `records-stream/v1`

`medrec import --from <cli> --patient <id> [--org <org>] [--since <date>]`
runs `<cli> records export --patient <id> --ndjson [--org …] [--since …]`.
A provider is any CLI whose `info --json` lists the `health-records/v1`
profile. Its standard output is NDJSON:

1. A header: `{"schema": "records-stream/v1", "source": {cli, version},
   "organization": {id, name, fhir_base, vendor}, "patient": {id,
   organization, label, relationship}, "since", "pulled_at", "fhir_version",
   "method"}`.
2. One FHIR R4 resource per line. A `Binary` follows the `DocumentReference`
   that names it.
3. A trailer: `{"schema": "records-stream-end/v1", "complete", "counts":
   {<type>: n}, "skipped": [{resource_type, id, search, reason, code}],
   "finished_at", "error"}`.

medrec writes nothing unless the trailer is present, says `complete: true`,
its counts match the resources read, and the provider exits 0. The provider's
exit code maps to medrec's: 3 becomes 3 (a sign-in is needed, and medrec
names the `auth login` command), 4 becomes 4 (not found), 2 becomes 1 (medrec
sent arguments the provider rejected), and 5 or anything else becomes 5
(upstream).

The patient's birth date in the stream must match the person's in
`household.json`, unless `--allow-birth-date-mismatch`. A source's first import needs
`--person`; later imports find the person by the source's patient id.
