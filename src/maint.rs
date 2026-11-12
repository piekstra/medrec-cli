//! Setting up and looking after the records folder: `init`, `doctor`,
//! `verify` and `backup`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use pk_cli_core::CliError;
use serde::Serialize;

use crate::config::Config;
use crate::folder::{self, Access, Folder};
use crate::hash;
use crate::model::Household;
use crate::proc::{which, Bounded};
use crate::store::{self, Location, Reads, Store};
use crate::view;

const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Serialize)]
pub struct Init {
    pub folder: String,
    pub folder_id: String,
    /// `created`, or `existing` for a folder medrec already set up.
    pub status: String,
    pub people: usize,
}

/// Set up (or adopt) a records folder and make it this machine's.
pub fn init(
    cfg: &mut Config,
    location: &Location,
    force: bool,
    budget: Duration,
) -> Result<Init, CliError> {
    let st = Store::open(location, Reads::Fresh, budget);
    let _lock = folder::FolderLock::acquire(location)?;
    let (h, status) = match folder::read_household(&st)? {
        Some(h) => (h, "existing"),
        None => {
            let missing_dir = matches!(location, Location::Dir(d) if !d.exists());
            let entries = match if missing_dir {
                Ok(Vec::new())
            } else {
                st.list("")
            } {
                Ok(e) => e,
                Err(CliError::NotFound(_)) => Vec::new(),
                Err(e) => return Err(e),
            };
            let foreign: Vec<_> = entries.iter().filter(|e| !e.rel.starts_with('.')).collect();
            if !foreign.is_empty() && !force {
                return Err(CliError::Usage(format!(
                    "{} already holds {} files that are not a medrec records folder; name an empty \
                     folder, or pass --force to set up medrec beside them",
                    location.display(),
                    foreign.len()
                )));
            }
            (folder::create(&st)?, "created")
        }
    };
    cfg.folder = Some(location.display());
    Ok(Init {
        folder: location.display(),
        folder_id: h.folder_id,
        status: status.into(),
        people: h.people.len(),
    })
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub check: String,
    /// `ok`, `warn`, `fail` or `skip`.
    pub status: String,
    pub detail: String,
}

fn check(name: &str, status: &str, detail: impl Into<String>) -> Check {
    Check {
        check: name.into(),
        status: status.into(),
        detail: detail.into(),
    }
}

fn first_line(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn version_of(prog: &str, args: &[&str]) -> Option<String> {
    let path = which(prog)?;
    let r = Bounded::new(&path)
        .args(args)
        .timeout(PROBE_TIMEOUT)
        .max_stdout(64 * 1024)
        .run()
        .ok()?;
    let out = if r.stdout.is_empty() {
        r.stderr_tail.as_bytes().to_vec()
    } else {
        r.stdout
    };
    Some(first_line(&out))
}

/// Every check `doctor` runs. None reads a credential or the Keychain;
/// rclone reads its own config file, whose mode is checked but whose
/// contents are never read here.
pub fn doctor(cfg: &Config) -> Vec<Check> {
    let mut out = Vec::new();
    out.push(filevault());
    let rclone = which("rclone");
    out.push(match &rclone {
        Some(_) => check(
            "rclone",
            "ok",
            version_of("rclone", &["version"]).unwrap_or_default(),
        ),
        None => check(
            "rclone",
            "fail",
            "rclone is not installed (`brew install rclone`)",
        ),
    });
    if rclone.is_some() {
        out.push(rclone_conf_mode());
    }
    match cfg.folder_location() {
        Err(e) => out.push(check("folder", "fail", e.to_string())),
        Ok(loc) => folder_checks(cfg, &loc, rclone.is_some(), &mut out),
    }
    if let Some(m) = &cfg.mount {
        out.push(mount_check(cfg, m));
    }
    for (prog, args, why) in [
        ("pdftotext", &["-v"][..], "reading text from PDFs (poppler)"),
        ("pdftoppm", &["-v"][..], "OCR of scanned PDFs (poppler)"),
        ("tesseract", &["--version"][..], "OCR"),
    ] {
        out.push(match version_of(prog, args) {
            Some(v) => check(prog, "ok", v),
            None => check(prog, "warn", format!("not installed; needed for {why}")),
        });
    }
    if which("tesseract").is_some() {
        out.push(ocr_languages(cfg));
    }
    out.push(
        match which("bsdtar")
            .or_else(|| cfg!(target_os = "macos").then(|| PathBuf::from("/usr/bin/tar")))
        {
            Some(p) => check("bsdtar", "ok", p.display().to_string()),
            None => check(
                "bsdtar",
                "warn",
                "not installed; needed to read a zipped or ISO imaging disc",
            ),
        },
    );
    out.push(viewer_check(cfg));
    out.push(cache_check());
    let left = view::leftovers();
    out.push(if left == 0 {
        check("open copies", "ok", "none left")
    } else {
        check(
            "open copies",
            "warn",
            format!(
                "{left} opened copies remain under the private open folder; they go after 12 hours"
            ),
        )
    });
    out
}

fn filevault() -> Check {
    if !cfg!(target_os = "macos") {
        return check("filevault", "skip", "not macOS; use full-disk encryption");
    }
    match Bounded::new("/usr/bin/fdesetup")
        .arg("status")
        .timeout(PROBE_TIMEOUT)
        .max_stdout(4096)
        .run()
    {
        Ok(r) => {
            let line = first_line(&r.stdout);
            if line.contains("FileVault is On") {
                check("filevault", "ok", line)
            } else {
                check(
                    "filevault",
                    "fail",
                    format!(
                        "{line}: turn FileVault on (opened copies and the cache live on this disk)"
                    ),
                )
            }
        }
        Err(e) => check("filevault", "warn", format!("could not ask fdesetup: {e}")),
    }
}

fn rclone_conf_mode() -> Check {
    use std::os::unix::fs::PermissionsExt;
    let Some(r) = which("rclone").and_then(|p| {
        Bounded::new(p)
            .args(["config", "file"])
            .timeout(PROBE_TIMEOUT)
            .max_stdout(4096)
            .run()
            .ok()
    }) else {
        return check(
            "rclone.conf",
            "warn",
            "could not ask rclone where its config is",
        );
    };
    let text = String::from_utf8_lossy(&r.stdout);
    let Some(path) = text.lines().map(str::trim).find(|l| l.starts_with('/')) else {
        return check("rclone.conf", "warn", "rclone named no config file");
    };
    match std::fs::metadata(path) {
        Ok(m) if m.permissions().mode() & 0o077 == 0 => {
            check("rclone.conf", "ok", format!("{path} is owner-only"))
        }
        Ok(m) => check(
            "rclone.conf",
            "fail",
            format!(
                "{path} is mode {:o}; it holds the Drive token: run `chmod 600 {path}`",
                m.permissions().mode() & 0o777
            ),
        ),
        Err(_) => check("rclone.conf", "warn", format!("{path} does not exist yet")),
    }
}

fn folder_checks(cfg: &Config, loc: &Location, have_rclone: bool, out: &mut Vec<Check>) {
    if let Location::Remote(spec) = loc {
        if !have_rclone {
            out.push(check("remote", "fail", format!("{spec} needs rclone")));
            return;
        }
        let name = spec.split(':').next().unwrap_or(spec);
        let listed = which("rclone").and_then(|p| {
            Bounded::new(p)
                .arg("listremotes")
                .timeout(PROBE_TIMEOUT)
                .max_stdout(64 * 1024)
                .run()
                .ok()
        });
        let known = listed
            .map(|r| {
                String::from_utf8_lossy(&r.stdout)
                    .lines()
                    .any(|l| l.trim() == format!("{name}:"))
            })
            .unwrap_or(false);
        if !known {
            out.push(check(
                "remote",
                "fail",
                format!("rclone has no remote `{name}`; set it up with `rclone config` (Google Drive, scope drive)"),
            ));
            return;
        }
        out.push(check(
            "remote",
            "ok",
            format!("rclone remote `{name}` is configured"),
        ));
    }
    let folder = match Folder::open_at(loc, Access::Write, cfg.rclone_budget()) {
        Ok(f) => f,
        Err(e) => {
            out.push(check("folder", "fail", e.to_string()));
            return;
        }
    };
    match folder.household() {
        Ok(h) => out.push(check(
            "folder",
            "ok",
            format!("{} ({} people)", loc.display(), h.people.len()),
        )),
        Err(e) => out.push(check("folder", "fail", e.to_string())),
    }
    match folder.store.read_text(folder::README) {
        Ok(Some(t)) if t.starts_with(folder::README_MARK) => {}
        _ => out.push(check(
            "readme",
            "warn",
            "README.md is missing or changed; `medrec init` writes it again",
        )),
    }
    out.push(match folder.store.space() {
        Ok(store::Space {
            free: Some(f),
            total,
        }) => {
            let gib = |b: u64| format!("{:.1} GiB", b as f64 / (1u64 << 30) as f64);
            let detail = match total {
                Some(t) => format!("{} free of {}", gib(f), gib(t)),
                None => format!("{} free", gib(f)),
            };
            check(
                "free space",
                if f < (1 << 30) { "warn" } else { "ok" },
                detail,
            )
        }
        Ok(_) => check("free space", "skip", "the store does not report free space"),
        Err(e) => check("free space", "warn", e.to_string()),
    });
    out.push(probe(&folder));
}

/// Write a random file, read it back, remove it.
fn probe(folder: &Folder) -> Check {
    let token = crate::ids::new("probe");
    let run = || -> Result<(), CliError> {
        folder.store.write_text(folder::PROBE, &token)?;
        let back = folder.store.read_text(folder::PROBE)?;
        folder.store.delete(folder::PROBE)?;
        if back.as_deref() != Some(token.as_str()) {
            return Err(CliError::Upstream("the probe read back different".into()));
        }
        Ok(())
    };
    match run() {
        Ok(()) => check(
            "write probe",
            "ok",
            "wrote, read back and removed a probe file",
        ),
        Err(e) => check("write probe", "fail", e.to_string()),
    }
}

fn mount_check(cfg: &Config, mount: &str) -> Check {
    let dir = store::expand_home(mount);
    let Ok(text) = std::fs::read_to_string(dir.join(folder::HOUSEHOLD)) else {
        return check(
            "mount",
            "warn",
            format!(
                "{} has no household.json (Drive for desktop not running?)",
                dir.display()
            ),
        );
    };
    let mounted: Option<Household> = serde_json::from_str(&text).ok();
    let remote = cfg
        .folder_location()
        .ok()
        .map(|l| Store::open(&l, Reads::Cached, cfg.rclone_budget()))
        .and_then(|s| folder::read_household(&s).ok().flatten());
    match (mounted, remote) {
        (Some(m), Some(r)) if m.folder_id == r.folder_id => check(
            "mount",
            "ok",
            format!("{} is the same records folder", dir.display()),
        ),
        (Some(_), Some(_)) => check(
            "mount",
            "fail",
            format!(
                "{} is a different records folder than `folder`",
                dir.display()
            ),
        ),
        _ => check(
            "mount",
            "warn",
            format!(
                "could not compare {} with the records folder",
                dir.display()
            ),
        ),
    }
}

fn ocr_languages(cfg: &Config) -> Check {
    let r = which("tesseract").and_then(|p| {
        Bounded::new(p)
            .arg("--list-langs")
            .timeout(PROBE_TIMEOUT)
            .max_stdout(64 * 1024)
            .run()
            .ok()
    });
    let text = r
        .map(|r| format!("{}\n{}", String::from_utf8_lossy(&r.stdout), r.stderr_tail))
        .unwrap_or_default();
    let have: HashSet<&str> = text.lines().map(str::trim).collect();
    let want = cfg.ocr_languages();
    let missing: Vec<&str> = want.split('+').filter(|l| !have.contains(l)).collect();
    if missing.is_empty() {
        check("ocr languages", "ok", want)
    } else {
        check(
            "ocr languages",
            "warn",
            format!("tesseract lacks {}", missing.join(", ")),
        )
    }
}

fn viewer_check(cfg: &Config) -> Check {
    match view::Viewer::for_images(cfg.viewer.as_deref()) {
        view::Viewer::None => check(
            "viewer",
            "skip",
            "viewer is `none`: `images open` prints the copy's path",
        ),
        view::Viewer::System => check("viewer", "ok", "the system's default application"),
        view::Viewer::Weasis => {
            let found = Path::new("/Applications/Weasis.app").exists() || which("weasis").is_some();
            if found {
                check("viewer", "ok", "Weasis")
            } else {
                check("viewer", "warn", "Weasis is not installed (https://weasis.org); `images open --no-launch` still works")
            }
        }
        view::Viewer::App(a) => check(
            "viewer",
            if Path::new(&a).exists() { "ok" } else { "warn" },
            a,
        ),
        view::Viewer::Exe(e) => check(
            "viewer",
            if Path::new(&e).exists() || which(&e).is_some() {
                "ok"
            } else {
                "warn"
            },
            e,
        ),
    }
}

fn cache_check() -> Check {
    use std::os::unix::fs::PermissionsExt;
    let Some(base) = pk_cli_drive::cache::cache_base(store::BIN) else {
        return check("cache", "skip", "no cache directory");
    };
    match std::fs::metadata(&base) {
        Ok(m) if m.permissions().mode() & 0o077 == 0 => {
            check("cache", "ok", format!("{} is owner-only", base.display()))
        }
        Ok(m) => check(
            "cache",
            "fail",
            format!(
                "{} is mode {:o}; run `chmod 700 {}`",
                base.display(),
                m.permissions().mode() & 0o777,
                base.display()
            ),
        ),
        Err(_) => check("cache", "ok", "no cache yet"),
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Verify {
    pub deep: bool,
    pub documents: usize,
    pub study_files: usize,
    pub raw_records: usize,
    pub review_items: usize,
    pub ok: usize,
    /// Indexed files that are not in the folder.
    pub missing: Vec<String>,
    /// Indexed files whose hash differs from the index.
    pub changed: Vec<String>,
    /// Files under Documents, Claims or Imaging that no index entry names.
    pub unindexed: Vec<String>,
}

impl Verify {
    pub fn clean(&self) -> bool {
        self.missing.is_empty() && self.changed.is_empty()
    }
}

/// Check every indexed file against the folder: present, and with the MD5
/// the index recorded (`deep`: download each document and compare its
/// SHA-256 too).
pub fn verify(folder: &Folder, deep: bool) -> Result<Verify, CliError> {
    let h = folder.household()?;
    let mut v = Verify {
        deep,
        ..Default::default()
    };
    for p in &h.people {
        let idx = folder.index(p)?;
        let listing: HashMap<String, Option<String>> = folder
            .store
            .hashes(&p.folder)?
            .into_iter()
            .map(|f| (f.rel, f.md5))
            .collect();
        let mut named: HashSet<String> = HashSet::new();
        for d in &idx.documents {
            v.documents += 1;
            named.insert(d.path.clone());
            match listing.get(&d.path) {
                None => v.missing.push(d.path.clone()),
                Some(Some(m)) if *m != d.md5 => v.changed.push(d.path.clone()),
                _ if deep => {
                    let copy = folder.store.fetch(&d.path)?;
                    let f = std::fs::File::open(&copy.path)
                        .map_err(|e| CliError::Other(e.to_string()))?;
                    let (sha, _, _) =
                        hash::both_reader(f).map_err(|e| CliError::Other(e.to_string()))?;
                    if sha == d.sha256 {
                        v.ok += 1;
                    } else {
                        v.changed.push(d.path.clone());
                    }
                }
                _ => v.ok += 1,
            }
        }
        for s in &idx.studies {
            named.insert(format!("{}/{}", s.path, crate::imaging::STUDY_FILE));
            for series in &s.series {
                for f in series.instances.values() {
                    v.study_files += 1;
                    let rel = format!("{}/{f}", s.path);
                    if listing.contains_key(&rel) {
                        v.ok += 1;
                    } else {
                        v.missing.push(rel.clone());
                    }
                    named.insert(rel);
                }
            }
        }
        let raw: HashSet<&str> = idx
            .records
            .iter()
            .flat_map(|r| r.provenance.iter())
            .filter_map(|pv| pv.raw_path.as_deref())
            .collect();
        for r in raw {
            v.raw_records += 1;
            if listing.contains_key(r) {
                v.ok += 1;
            } else {
                v.missing.push(r.to_string());
            }
        }
        for rel in listing.keys() {
            let under = |top: &str| rel.starts_with(&format!("{}/{top}/", p.folder));
            if (under("Documents") || under("Claims") || under("Imaging")) && !named.contains(rel) {
                v.unindexed.push(rel.clone());
            }
        }
    }
    let queue = folder.review()?;
    let listing: HashMap<String, Option<String>> = folder
        .store
        .hashes(folder::REVIEW_DIR)?
        .into_iter()
        .map(|f| (f.rel, f.md5))
        .collect();
    for q in &queue.items {
        v.review_items += 1;
        match listing.get(&q.path) {
            None => v.missing.push(q.path.clone()),
            Some(Some(m)) if *m != q.md5 => v.changed.push(q.path.clone()),
            _ => v.ok += 1,
        }
    }
    v.missing.sort();
    v.changed.sort();
    v.unindexed.sort();
    Ok(v)
}

#[derive(Debug, Serialize)]
pub struct Backup {
    pub from: String,
    pub to: String,
    pub files: usize,
    pub bytes: u64,
    pub verified: usize,
    /// Files whose copy is missing or differs.
    pub mismatched: Vec<String>,
}

/// Copy the whole records folder to another rclone remote or directory,
/// then compare every file's MD5 on both sides.
pub fn backup(folder: &Folder, to: &Location, budget: Duration) -> Result<Backup, CliError> {
    let from = folder.store.location().clone();
    if from.display() == to.display() {
        return Err(CliError::Usage(
            "the backup must go somewhere other than the records folder".into(),
        ));
    }
    match (&from, to) {
        (Location::Dir(a), Location::Dir(b)) => copy_dir(a, b)?,
        _ => {
            let rclone = which("rclone")
                .ok_or_else(|| CliError::Other("backup to a remote needs rclone".into()))?;
            let (src, dst) = (
                folder.store.rclone_spec(),
                Store::open(to, Reads::Fresh, budget).rclone_spec(),
            );
            let r = Bounded::new(rclone)
                .args(["--log-level=ERROR", "copy", src.as_str(), dst.as_str()])
                .timeout(budget)
                .max_stdout(1024 * 1024)
                .run()?;
            if r.code != Some(0) {
                return Err(CliError::Upstream(format!(
                    "rclone copy: {}",
                    r.stderr_tail
                        .lines()
                        .rev()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("failed")
                )));
            }
        }
    }
    let target = Store::open(to, Reads::Fresh, budget);
    let theirs: HashMap<String, Option<String>> = target
        .hashes("")?
        .into_iter()
        .map(|f| (f.rel, f.md5))
        .collect();
    let ours = folder.store.hashes("")?;
    let mut b = Backup {
        from: from.display(),
        to: to.display(),
        files: ours.len(),
        bytes: ours.iter().map(|f| f.size).sum(),
        verified: 0,
        mismatched: Vec::new(),
    };
    for f in &ours {
        match (theirs.get(&f.rel), &f.md5) {
            (Some(Some(t)), Some(o)) if t == o => b.verified += 1,
            (Some(_), None) => b.verified += 1,
            _ => b.mismatched.push(f.rel.clone()),
        }
    }
    Ok(b)
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), CliError> {
    std::fs::create_dir_all(to)
        .map_err(|e| CliError::Other(format!("creating {}: {e}", to.display())))?;
    for e in std::fs::read_dir(from)
        .map_err(|e| CliError::Other(format!("reading {}: {e}", from.display())))?
    {
        let e = e.map_err(|e| CliError::Other(e.to_string()))?;
        let ft = e.file_type().map_err(|e| CliError::Other(e.to_string()))?;
        let dst = to.join(e.file_name());
        if ft.is_dir() {
            copy_dir(&e.path(), &dst)?;
        } else if ft.is_file() {
            std::fs::copy(e.path(), &dst)
                .map_err(|err| CliError::Other(format!("copying {}: {err}", e.path().display())))?;
        }
    }
    Ok(())
}

/// What `cache clear` removed.
#[derive(Debug, Serialize)]
pub struct Cleared {
    pub path: String,
    pub files: u64,
    pub bytes: u64,
}

/// Remove medrec's per-machine cache: every remote's read cache and every
/// opened copy. The records folder is not touched.
pub fn clear_local_copies() -> Result<Cleared, CliError> {
    let base = pk_cli_drive::cache::cache_base(store::BIN).ok_or_else(|| {
        CliError::Other("cannot resolve a cache directory (is $HOME set?)".into())
    })?;
    let (mut files, mut bytes) = (0u64, 0u64);
    fn tally(dir: &Path, files: &mut u64, bytes: &mut u64) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let Ok(m) = e.path().symlink_metadata() else {
                continue;
            };
            if m.is_dir() {
                tally(&e.path(), files, bytes);
            } else {
                *files += 1;
                *bytes += m.len();
            }
        }
    }
    // `locks/` stays: a writer running now holds its lock file open, and a
    // new file would let a second writer in beside it.
    let entries = match std::fs::read_dir(&base) {
        Ok(rd) => rd.flatten().collect::<Vec<_>>(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            return Err(CliError::Other(format!(
                "cannot read {}: {e}",
                base.display()
            )))
        }
    };
    for e in entries {
        if e.file_name() == "locks" {
            continue;
        }
        let path = e.path();
        let is_dir = path.symlink_metadata().map(|m| m.is_dir()).unwrap_or(false);
        if is_dir {
            tally(&path, &mut files, &mut bytes);
        } else {
            files += 1;
            bytes += path.symlink_metadata().map(|m| m.len()).unwrap_or(0);
        }
        let removed = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(err) = removed {
            return Err(CliError::Other(format!(
                "cannot remove {}: {err}",
                path.display()
            )));
        }
    }
    Ok(Cleared {
        path: base.display().to_string(),
        files,
        bytes,
    })
}
