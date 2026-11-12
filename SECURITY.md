# Security

medrec files a household's medical records, the most sensitive data a
household has. This page says where those records are, who can read them in
each place, and what medrec does and does not do with them.

## What medrec touches

| Where | What | Who reads it |
|---|---|---|
| The records folder in Google Drive (`<remote>:Medical Records/`) | Every document, study, raw record, the index and extracted text, notes | The Google account, anyone it shares the folder with, and Google |
| `~/.config/rclone/rclone.conf` | rclone's OAuth token for that Google account | Anyone who can read the file |
| `~/.cache/medrec/` (or `$XDG_CACHE_HOME/medrec`) | The read cache (`household.json`, `index.json`, `text.json` and files read recently) and opened copies (`open/`, removed after 12 hours) | This Mac's user; mode 0700 directories and 0600 files |
| A private temporary directory per command | A document or study being staged before upload | This Mac's user; removed when the command ends |
| `~/.config/medrec/config.json` | The folder's location and settings; no secret | This Mac's user |

medrec holds **no credentials**. Provider and insurer CLIs keep their own
logins in the macOS Keychain under their own names. medrec runs them as
bounded subprocesses and passes only a patient id, an organization and a
date. It reads Drive through `rclone`, which owns the Drive token.

## What medrec never does

- It never sends a record, a document or extracted text to a third-party
  service. Text extraction and OCR run locally (`pdftotext`, `tesseract`).
  There is no telemetry, no cloud OCR and no AI service.
- It has no MCP server and is not registered in `cli-mcp`. An MCP tool returns
  records to the model that calls it, which is a third-party service.
- It never prints or logs a credential, and never takes one on argv.
- It never runs a disc's bundled viewer. `images open` hands the viewer a
  local folder (Weasis through `weasis://$dicom:get -l <folder>`), and makes no
  network call with study data. Weasis's own update check is a Weasis setting;
  turn it off in Weasis's preferences.
- It never edits a filed document, a DICOM file or a raw record. A record a
  source changed is written beside the previous version, which is kept.
- It deletes a source file only when it was moved (the inbox, `~/Downloads`,
  `--move`), and only after Drive's MD5 of the filed copy matches and the index
  has been written and read back.

## Threat model

The records folder is a plain, private Drive folder without encryption
(PLAN D1 and D9). That choice puts trust in Google and in the Google account.

| If this is exposed | Its holder can read | What limits it |
|---|---|---|
| **Google** (the company, its systems, a legal request to it) | The whole folder | Google's terms and law. medrec adds no encryption. Keep the folder private and unshared. |
| **The Google account** (password, session or recovery taken over) | The whole folder, and the rest of the Drive | 2-Step Verification or a passkey on the account; review "Sharing" on the folder |
| **The rclone token** (`rclone.conf` copied off the Mac) | The whole Drive, read and write, until revoked; its scope is Drive, not just this folder | Revoke rclone's access in the Google account's third-party connections. `medrec doctor` checks the file is owner-only. `rclone config` can encrypt the file with a password, at the cost of typing it for every command. |
| **This Mac, unlocked** (malware or someone at the keyboard) | Everything above: the token, the cache, the mounted Drive if Drive for desktop is used | macOS's own protections; lock the screen |
| **A lost or stolen laptop, powered off or locked** | Nothing while FileVault is on and the account is locked | `medrec doctor` checks FileVault. On loss, revoke the rclone token and sign out of the Google account's sessions. |
| **A backup copy** (`medrec backup --to`) | The whole folder, as of that backup | It is a second copy with the same contents. Keep it on an encrypted disk, or in a remote as private as the first. |
| **This repository** (public) | No personal data | [PRIVACY.md](PRIVACY.md) and the tests that enforce it |

`medrec cache clear` removes every copy on this Mac (the read cache and
opened copies). The records folder is not touched.

## Supply chain

- Shared crates are pinned to tags: cli-common and medical-common, both the
  family's own. No third-party FHIR, SMART, DICOM or health-data library is
  used. The records stream comes from medical-common; the DICOM header reader
  and the FHIR normalizer are this repo's own code.
- CI runs `cargo audit`, `cargo deny` and `gitleaks` on every pull request,
  on GitHub-hosted Linux runners. `make install` builds with `--locked` from
  the reviewed `Cargo.lock`.
- Release binaries are built by the release workflow from a version tag, with a SHA-256 file beside each.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on this repository ("Report a
vulnerability" under Security). Do not put a record, a screenshot or a
patient's name in any report or issue.
