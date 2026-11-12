//! The records folder: its layout, and reading and writing it with a
//! read-back after every write (docs/data-contract.md).
//!
//! ```text
//! README.md                  what this folder is, and its rules
//! household.json             people, organizations, sources
//! inbox/                     drops awaiting `medrec intake --inbox`
//! _review/                   what intake could not decide, and review.json
//! <Person>/
//!   notes.md                 dated notes (the notes' source)
//!   index.json               the normalized index (derived)
//!   text.json                extracted text per document (derived)
//!   Documents/<YYYY>/<YYYY-MM-DD>_<type>_<org>_<details>_<original name>
//!   Claims/<YYYY>/           EOBs and bills
//!   Imaging/<YYYY-MM-DD>_<modality>_<body part>_<facility>/   study files + study.json
//!   Records/<source>/<ResourceType>/<id>.json                 raw FHIR, verbatim
//! ```

use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

use pk_cli_core::CliError;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::config::Config;
use crate::hash;
use crate::ids;
use crate::model::{
    Household, Note, Person, PersonIndex, PersonText, ReviewQueue, FORMAT, HOUSEHOLD_SCHEMA,
};
use crate::store::{Location, Reads, Store};

pub const README: &str = "README.md";
pub const HOUSEHOLD: &str = "household.json";
pub const INBOX: &str = "inbox";
pub const REVIEW_DIR: &str = "_review";
pub const REVIEW_FILE: &str = "_review/review.json";
pub const PROBE: &str = ".medrec-probe";
/// The README's first line, so a person (and `init`) can tell the folder is
/// medrec's.
pub const README_MARK: &str = "# Medical records (managed by medrec)";

/// A read-only command reads through the cache; a mutating one reads fresh
/// and holds the machine-wide folder lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

pub struct Folder {
    pub store: Store,
    pub folder_id: String,
    _lock: Option<FolderLock>,
}

impl Folder {
    pub fn open(cfg: &Config, access: Access) -> Result<Folder, CliError> {
        Folder::open_at(&cfg.folder_location()?, access, cfg.rclone_budget())
    }

    pub fn open_at(
        location: &Location,
        access: Access,
        budget: Duration,
    ) -> Result<Folder, CliError> {
        let lock = match access {
            Access::Write => Some(FolderLock::acquire(location)?),
            Access::Read => None,
        };
        let reads = match access {
            Access::Write => Reads::Fresh,
            Access::Read => Reads::Cached,
        };
        let store = Store::open(location, reads, budget);
        let h = read_household(&store)?.ok_or_else(|| {
            CliError::NotFound(format!(
                "no medrec records folder at {} (no {HOUSEHOLD}); run `medrec init {}`",
                location.display(),
                location.display()
            ))
        })?;
        Ok(Folder {
            store,
            folder_id: h.folder_id,
            _lock: lock,
        })
    }

    pub fn read_json<T: DeserializeOwned>(&self, rel: &str) -> Result<Option<T>, CliError> {
        read_json(&self.store, rel)
    }

    /// Write JSON, then read it back and compare.
    pub fn write_json<T: Serialize>(&self, rel: &str, value: &T) -> Result<(), CliError> {
        write_json(&self.store, rel, value)
    }

    pub fn household(&self) -> Result<Household, CliError> {
        let h = read_household(&self.store)?.ok_or_else(|| {
            CliError::Other(format!("{} is missing", self.store.display_path(HOUSEHOLD)))
        })?;
        if h.folder_id != self.folder_id {
            return Err(CliError::Other(
                "household.json was replaced while this command ran".into(),
            ));
        }
        Ok(h)
    }

    /// Refuse when another machine wrote the household since `h` was read.
    pub fn household_unchanged(&self, h: &Household) -> Result<(), CliError> {
        if let Some(current) = read_household(&self.store)? {
            if current.rev != h.rev {
                return Err(changed_elsewhere(HOUSEHOLD));
            }
        }
        Ok(())
    }

    /// Write the household, refusing when another machine wrote it since
    /// this command read it.
    pub fn save_household(&self, h: &mut Household) -> Result<(), CliError> {
        if let Some(current) = read_household(&self.store)? {
            if current.rev != h.rev {
                return Err(changed_elsewhere(HOUSEHOLD));
            }
        }
        h.rev += 1;
        self.write_json(HOUSEHOLD, h)
    }

    /// A person's index; an empty one when none was written yet.
    pub fn index(&self, p: &Person) -> Result<PersonIndex, CliError> {
        let rel = index_rel(p);
        let idx: PersonIndex = self
            .read_json(&rel)?
            .unwrap_or_else(|| PersonIndex::new(&p.id));
        if idx.person != p.id {
            return Err(CliError::Other(format!("{rel} names another person")));
        }
        Ok(idx)
    }

    /// Refuse when another machine wrote `p`'s index since `idx` was read.
    /// Checked before an upload, so a stale view does not file anything.
    pub fn index_unchanged(&self, p: &Person, idx: &PersonIndex) -> Result<(), CliError> {
        let rel = index_rel(p);
        let current: Option<PersonIndex> = self.read_json(&rel)?;
        if current.map(|c| c.rev).unwrap_or(0) != idx.rev {
            return Err(changed_elsewhere(&rel));
        }
        Ok(())
    }

    pub fn save_index(&self, p: &Person, idx: &mut PersonIndex) -> Result<(), CliError> {
        let rel = index_rel(p);
        let current: Option<PersonIndex> = self.read_json(&rel)?;
        if current.map(|c| c.rev).unwrap_or(0) != idx.rev {
            return Err(changed_elsewhere(&rel));
        }
        idx.rev += 1;
        self.write_json(&rel, idx)
    }

    pub fn text(&self, p: &Person) -> Result<PersonText, CliError> {
        Ok(self
            .read_json(&text_rel(p))?
            .unwrap_or_else(|| PersonText::new(&p.id)))
    }

    pub fn save_text(&self, p: &Person, t: &PersonText) -> Result<(), CliError> {
        self.write_json(&text_rel(p), t)
    }

    pub fn review(&self) -> Result<ReviewQueue, CliError> {
        Ok(self.read_json(REVIEW_FILE)?.unwrap_or_default())
    }

    pub fn save_review(&self, q: &mut ReviewQueue) -> Result<(), CliError> {
        let current: Option<ReviewQueue> = self.read_json(REVIEW_FILE)?;
        if current.map(|c| c.rev).unwrap_or(0) != q.rev {
            return Err(changed_elsewhere(REVIEW_FILE));
        }
        q.rev += 1;
        self.write_json(REVIEW_FILE, q)
    }

    pub fn notes(&self, p: &Person) -> Result<Vec<Note>, CliError> {
        let text = self.store.read_text(&notes_rel(p))?.unwrap_or_default();
        Ok(crate::notes::parse(&text))
    }

    pub fn save_notes(&self, p: &Person, notes: &[Note]) -> Result<(), CliError> {
        let rel = notes_rel(p);
        let text = crate::notes::render(&p.name, notes);
        self.store.write_text(&rel, &text)?;
        let back = self.store.read_text(&rel)?.unwrap_or_default();
        if back != text {
            return Err(CliError::Upstream(format!(
                "{} read back different from what was written",
                self.store.display_path(&rel)
            )));
        }
        Ok(())
    }

    /// Upload a local file to a free name at `dir/name` (appending `_2`, `_3`
    /// when taken), then check the store holds the same MD5. Returns the
    /// path it was filed at.
    ///
    /// A name that already holds these bytes is taken as this file: a run
    /// that stopped after its upload and before its index was saved left
    /// it there, and the run after adopts it instead of filing a second
    /// copy.
    pub fn put_new_file(
        &self,
        local: &Path,
        dir: &str,
        name: &str,
        md5: &str,
    ) -> Result<String, CliError> {
        let (stem, ext) = split_ext(name);
        for n in 1..1000 {
            let rel = if n == 1 {
                format!("{dir}/{name}")
            } else {
                format!("{dir}/{stem}_{n}{ext}")
            };
            match self.store.md5(&rel)? {
                Some(m) if m == md5 => return Ok(rel),
                Some(_) => continue,
                None if self.store.exists(&rel)? => continue,
                None => {
                    self.store.put_file(local, &rel)?;
                    self.check_md5(&rel, md5)?;
                    return Ok(rel);
                }
            }
        }
        Err(CliError::Other(format!("no free name for {name} in {dir}")))
    }

    /// Move a file already in the store to a free name at `dir/name`, then
    /// check it kept its MD5. Returns where it went.
    pub fn move_new_file(
        &self,
        from: &str,
        dir: &str,
        name: &str,
        md5: &str,
    ) -> Result<String, CliError> {
        let rel = self.free_name(dir, name)?;
        self.store.move_no_clobber(from, &rel)?;
        self.check_md5(&rel, md5)?;
        Ok(rel)
    }

    /// The store's MD5 for `rel` must be `md5`.
    pub fn check_md5(&self, rel: &str, md5: &str) -> Result<(), CliError> {
        match self.store.md5(rel)? {
            Some(got) if got == md5 => Ok(()),
            Some(_) => Err(CliError::Upstream(format!(
                "{} reads back with a different MD5 than was sent",
                self.store.display_path(rel)
            ))),
            None => Err(CliError::Upstream(format!(
                "{} is not there right after it was written",
                self.store.display_path(rel)
            ))),
        }
    }

    pub fn free_name(&self, dir: &str, name: &str) -> Result<String, CliError> {
        let (stem, ext) = split_ext(name);
        for n in 1..1000 {
            let candidate = if n == 1 {
                format!("{dir}/{name}")
            } else {
                format!("{dir}/{stem}_{n}{ext}")
            };
            if !self.store.exists(&candidate)? {
                return Ok(candidate);
            }
        }
        Err(CliError::Other(format!("no free name for {name} in {dir}")))
    }
}

pub fn read_json<T: DeserializeOwned>(store: &Store, rel: &str) -> Result<Option<T>, CliError> {
    let Some(text) = store.read_text(rel)? else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| CliError::Other(format!("{} is not readable: {e}", store.display_path(rel))))
}

pub fn write_json<T: Serialize>(store: &Store, rel: &str, value: &T) -> Result<(), CliError> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| CliError::Other(format!("serializing {rel}: {e}")))?;
    text.push('\n');
    store.write_text(rel, &text)?;
    let back = store.read_text(rel)?.ok_or_else(|| {
        CliError::Upstream(format!(
            "{} vanished right after it was written",
            store.display_path(rel)
        ))
    })?;
    if back != text {
        return Err(CliError::Upstream(format!(
            "{} read back different from what was written",
            store.display_path(rel)
        )));
    }
    Ok(())
}

pub fn read_household(store: &Store) -> Result<Option<Household>, CliError> {
    let Some(h) = read_json::<Household>(store, HOUSEHOLD)? else {
        return Ok(None);
    };
    if h.schema != HOUSEHOLD_SCHEMA {
        return Err(CliError::Other(format!(
            "{} has schema `{}`; this medrec reads `{HOUSEHOLD_SCHEMA}`",
            store.display_path(HOUSEHOLD),
            h.schema
        )));
    }
    if h.format > FORMAT {
        return Err(CliError::Other(format!(
            "the records folder is format {}, newer than this medrec (format {FORMAT}); run `medrec self-update`",
            h.format
        )));
    }
    if !ids::is_valid("f", &h.folder_id) {
        return Err(CliError::Other(
            "household.json's folder id is malformed".into(),
        ));
    }
    Ok(Some(h))
}

// ---- paths ------------------------------------------------------------------

pub fn index_rel(p: &Person) -> String {
    format!("{}/index.json", p.folder)
}

pub fn text_rel(p: &Person) -> String {
    format!("{}/text.json", p.folder)
}

pub fn notes_rel(p: &Person) -> String {
    format!("{}/notes.md", p.folder)
}

/// Where a document of this type and date goes: `Claims/<YYYY>` for EOBs
/// and bills, `Documents/<YYYY>` for everything else.
pub fn document_dir(p: &Person, doc_type: &str, date: &str) -> String {
    let year = date.get(..4).unwrap_or("undated");
    let top = if matches!(doc_type, "eob" | "bill") {
        "Claims"
    } else {
        "Documents"
    };
    format!("{}/{top}/{year}", p.folder)
}

pub fn records_dir(p: &Person, source: &str) -> String {
    format!("{}/Records/{source}", p.folder)
}

pub fn imaging_dir(p: &Person, folder: &str) -> String {
    format!("{}/Imaging/{folder}", p.folder)
}

/// `<YYYY-MM-DD>_<type>_<org>_<details>_<original name>`, the tax-cli and
/// organize-scans convention: a date prefix, lowercase descriptors, and the
/// original file name kept as the suffix for traceability.
pub fn filed_name(
    date: &str,
    doc_type: &str,
    org: Option<&str>,
    details: Option<&str>,
    original: &str,
) -> String {
    let mut parts = vec![date.to_string(), slug(doc_type, 24)];
    for extra in [org, details].into_iter().flatten() {
        let s = slug(extra, 40);
        if !s.is_empty() {
            parts.push(s);
        }
    }
    let (stem, ext) = split_ext(&safe_component(original));
    let stem = stem.trim_matches('_');
    if !stem.is_empty() {
        parts.push(stem.chars().take(80).collect());
    }
    let mut name = parts.join("_");
    name.push_str(&ext.to_ascii_lowercase());
    name
}

/// Lowercase ASCII words joined by `_`, at most `max` characters.
pub fn slug(s: &str, max: usize) -> String {
    let folded = crate::intake::classify::fold(s);
    let mut out = String::new();
    for w in folded.split_whitespace() {
        let w: String = w.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if w.is_empty() {
            continue;
        }
        if !out.is_empty() {
            if out.len() + 1 + w.len() > max {
                break;
            }
            out.push('_');
        } else if w.len() > max {
            return w[..max].to_string();
        }
        out.push_str(&w);
    }
    out
}

/// A file or folder name safe on Drive, macOS and Linux: no separators,
/// control characters, or leading dots.
pub fn safe_component(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    while out.starts_with('.') {
        out.remove(0);
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        "file".into()
    } else {
        out
    }
}

fn split_ext(name: &str) -> (String, String) {
    match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 5
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            (stem.to_string(), format!(".{ext}"))
        }
        _ => (name.to_string(), String::new()),
    }
}

/// A person's folder name: their name, made safe, never one of the
/// folder's own names, and unique.
pub fn person_folder(name: &str, taken: &[String]) -> String {
    let base = safe_component(name);
    let base = match base.as_str() {
        "inbox" | "_review" | "README.md" | "household.json" => format!("{base} (person)"),
        _ if base.starts_with('_') => base.trim_start_matches('_').to_string(),
        _ => base,
    };
    let lower: Vec<String> = taken.iter().map(|t| t.to_lowercase()).collect();
    if !lower.contains(&base.to_lowercase()) {
        return base;
    }
    (2..)
        .map(|n| format!("{base} ({n})"))
        .find(|c| !lower.contains(&c.to_lowercase()))
        .expect("an unbounded range finds a free name")
}

/// Create a records folder: README, `household.json`, `inbox/`, `_review/`.
pub fn create(store: &Store) -> Result<Household, CliError> {
    let h = Household::new(&ids::new("f"));
    store.write_text(README, README_TEXT)?;
    for dir in [INBOX, REVIEW_DIR] {
        store.write_text(&format!("{dir}/README.md"), &dir_readme(dir))?;
    }
    write_json(store, REVIEW_FILE, &ReviewQueue::default())?;
    // household.json last: a folder without it is not a medrec folder yet,
    // so an interrupted init can simply run again.
    write_json(store, HOUSEHOLD, &h)?;
    Ok(h)
}

fn dir_readme(dir: &str) -> String {
    match dir {
        INBOX => "Drop medical documents here; `medrec intake --inbox` files them and removes them from here.\n".into(),
        _ => "Documents medrec could not file on its own wait here; `medrec review` files them.\n".into(),
    }
}

pub const README_TEXT: &str = "# Medical records (managed by medrec)

This folder holds a household's medical records, filed by `medrec`
(https://github.com/piekstra/medrec-cli). Every file is a plain file you can
open in the Drive app.

- `household.json`: the people, organizations and sources.
- `inbox/`: drop documents here; `medrec intake --inbox` files them.
- `_review/`: documents medrec could not file on its own; `medrec review`.
- One folder per person:
  - `notes.md`: dated notes.
  - `Documents/<year>/`: scans and files, named
    `<date>_<type>_<organization>_<details>_<original name>`.
  - `Claims/<year>/`: explanations of benefits and bills.
  - `Imaging/<date>_<modality>_<body part>_<facility>/`: imaging studies,
    their DICOM files exactly as received, and `study.json`.
  - `Records/<source>/<resource type>/<id>.json`: records from providers
    and insurers, exactly as their systems sent them (FHIR R4).
  - `index.json`, `text.json`: medrec's index of the files above and their
    extracted text. medrec keeps them; do not edit them by hand.

Rules: medrec never edits a filed document or a raw record. Rename or move
files through medrec, so the index stays right; `medrec verify` checks every
file against the index.
";

/// One mutating medrec per records folder per machine at a time.
pub struct FolderLock {
    _file: File,
}

impl FolderLock {
    pub fn acquire(location: &Location) -> Result<FolderLock, CliError> {
        let base = pk_cli_drive::cache::cache_base("medrec").ok_or_else(|| {
            CliError::Other("cannot resolve a cache directory ($HOME unset)".into())
        })?;
        let dir = base.join("locks");
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&dir)
            .map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))?;
        let name = hash::sha256_hex(location.display().as_bytes());
        let path = dir.join(format!("{}.lock", &name[..16]));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| CliError::Other(format!("opening {}: {e}", path.display())))?;
        let start = Instant::now();
        let mut said = false;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(FolderLock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if start.elapsed() > Duration::from_secs(120) {
                        return Err(CliError::Other(format!(
                            "another medrec has been writing this records folder for 2 minutes (lock {}); \
                             try again when it finishes",
                            path.display()
                        )));
                    }
                    if !said {
                        eprintln!("note: waiting for another medrec writing this records folder");
                        said = true;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(std::fs::TryLockError::Error(e)) => {
                    return Err(CliError::Other(format!("locking {}: {e}", path.display())))
                }
            }
        }
    }
}

fn changed_elsewhere(rel: &str) -> CliError {
    CliError::Other(format!(
        "{rel} changed while this command ran (another machine wrote it), so medrec left it as it \
         is. Run the command again: a file it already filed is found and indexed, not copied again."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_file_adopts_its_own_earlier_upload_and_never_replaces_another() {
        let root = crate::testutil::temp_dir("folder-put");
        let loc = crate::store::Location::Dir(root.join("records"));
        let st = Store::open(&loc, crate::store::Reads::Fresh, Duration::from_secs(60));
        create(&st).unwrap();
        let f = Folder::open_at(&loc, Access::Write, Duration::from_secs(60)).unwrap();
        let (a, b) = (root.join("a.pdf"), root.join("b.pdf"));
        std::fs::write(&a, b"aaa").unwrap();
        std::fs::write(&b, b"bbb").unwrap();
        let (ma, mb) = (crate::hash::md5_hex(b"aaa"), crate::hash::md5_hex(b"bbb"));

        let first = f.put_new_file(&a, "P/Documents", "x.pdf", &ma).unwrap();
        assert_eq!(first, "P/Documents/x.pdf");
        // A run that stopped before its index was saved: the rerun finds
        // the same bytes and files nothing new.
        assert_eq!(
            f.put_new_file(&a, "P/Documents", "x.pdf", &ma).unwrap(),
            first
        );
        assert_eq!(
            f.put_new_file(&b, "P/Documents", "x.pdf", &mb).unwrap(),
            "P/Documents/x_2.pdf"
        );
        assert_eq!(
            st.read_text("P/Documents/x.pdf").unwrap().as_deref(),
            Some("aaa")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn filed_names_follow_the_convention_and_keep_the_original_name() {
        assert_eq!(
            filed_name(
                "2026-03-04",
                "lab_report",
                Some("Example Pediatrics"),
                None,
                "scan_0001.PDF"
            ),
            "2026-03-04_lab_report_example_pediatrics_scan_0001.pdf"
        );
        assert_eq!(
            filed_name(
                "2026-03-04",
                "eob",
                Some("Example Health Plan"),
                Some("claim 123"),
                "../x/:bad?.pdf"
            ),
            "2026-03-04_eob_example_health_plan_claim_123_x__bad.pdf"
        );
        assert_eq!(
            filed_name("2026-03-04", "other", None, None, ".pdf"),
            "2026-03-04_other_pdf"
        );
    }

    #[test]
    fn eobs_and_bills_go_to_claims() {
        let p = Person {
            id: "p_x".into(),
            name: "Robin".into(),
            folder: "Robin".into(),
            birth_date: None,
            adult: false,
            guardians: vec![],
            aliases: vec![],
            identifiers: vec![],
            links: Default::default(),
            created_at: String::new(),
        };
        assert_eq!(document_dir(&p, "eob", "2026-01-02"), "Robin/Claims/2026");
        assert_eq!(
            document_dir(&p, "lab_report", "2026-01-02"),
            "Robin/Documents/2026"
        );
    }

    #[test]
    fn person_folders_are_safe_and_unique() {
        assert_eq!(person_folder("Alex Testperson", &[]), "Alex Testperson");
        assert_eq!(
            person_folder("Alex Testperson", &["alex testperson".into()]),
            "Alex Testperson (2)"
        );
        assert_eq!(person_folder("inbox", &[]), "inbox (person)");
        assert_eq!(person_folder("_x/y", &[]), "x_y");
        assert_eq!(person_folder("..", &[]), "file");
    }

    #[test]
    fn create_then_open_and_write_with_rev_checks() {
        let root = crate::testutil::temp_dir("folder");
        let loc = Location::Dir(root.clone());
        let store = Store::open(&loc, Reads::Fresh, crate::store::DEFAULT_BUDGET);
        let h = create(&store).unwrap();
        assert!(store
            .read_text(README)
            .unwrap()
            .unwrap()
            .starts_with(README_MARK));
        let f = Folder::open_at(&loc, Access::Write, crate::store::DEFAULT_BUDGET).unwrap();
        assert_eq!(f.folder_id, h.folder_id);
        let mut a = f.household().unwrap();
        let mut b = f.household().unwrap();
        f.save_household(&mut a).unwrap();
        assert!(f.save_household(&mut b).is_err(), "a stale rev is refused");
        std::fs::remove_dir_all(root).unwrap();
    }
}
