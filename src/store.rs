//! The one seam between the records folder and where its bytes live.
//!
//! Every operation goes through [`Store`], which moves files under
//! folder-relative paths. It is backed by `pk-cli-drive`: an rclone remote
//! (canonical, behind the read cache) or a local directory (a test folder,
//! a disk). What `pk-cli-drive` does not offer yet (hash listings, folder
//! copies, deletes, free space) is one bounded `rclone` call here.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use md5::{Digest, Md5};
use serde::Serialize;
use serde_json::Value;

use pk_cli_core::CliError;
use pk_cli_drive::{cache, Backend, CachePolicy, CachedRemote, FileEntry, LocalCopy, Remote};

pub const BIN: &str = "medrec";

/// How a command reaches the store.
#[derive(Debug, Clone)]
pub enum Location {
    /// `<remote>:<path>` through rclone.
    Remote(String),
    /// A local directory.
    Dir(PathBuf),
}

impl Location {
    /// `remote:path` is a remote; anything else (absolute, `./x`, `~/x`)
    /// is a directory. A single-letter "remote" before a colon is never
    /// taken as a remote, so a Windows-style `C:\x` is not misread.
    pub fn parse(s: &str) -> Location {
        let s = s.trim();
        if let Some((name, _)) = s.split_once(':') {
            if name.len() > 1 && !name.contains('/') && !s.starts_with('.') && !s.starts_with('~') {
                return Location::Remote(s.trim_end_matches('/').to_string());
            }
        }
        Location::Dir(expand_home(s))
    }

    pub fn display(&self) -> String {
        match self {
            Location::Remote(s) => s.clone(),
            Location::Dir(d) => d.display().to_string(),
        }
    }
}

pub fn expand_home(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(s)
}

/// Whether reads may come from the per-machine cache. A command that will
/// rewrite what it reads asks for `Fresh`, so it never derives a write from
/// a copy another machine has since replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reads {
    Cached,
    Fresh,
}

pub struct Store {
    backend: Backend,
    location: Location,
    budget: Duration,
}

/// The rclone budget for one command: every rclone call it makes is killed
/// once this much time has passed since the command started.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(1800);

fn remote(spec: &str, budget: Duration) -> Remote {
    Remote::new(spec).temp_prefix(BIN).with_budget(budget)
}

impl Store {
    pub fn open(location: &Location, reads: Reads, budget: Duration) -> Store {
        let backend = match location {
            Location::Dir(d) => Backend::Mount(d.clone()),
            Location::Remote(spec) => match cache::cache_dir_for(BIN, spec) {
                None => Backend::Remote(remote(spec, budget)),
                Some(dir) => {
                    let mut policy = CachePolicy::from_env(BIN);
                    policy.bypass_reads |= reads == Reads::Fresh;
                    let mut cached = CachedRemote::new(remote(spec, budget), dir, policy);
                    if let Some(r) =
                        cache::background_revalidator(&policy, &["cache", "sync"], spec)
                    {
                        cached = cached.with_revalidator(r);
                    }
                    Backend::Cached(cached)
                }
            },
        };
        Store {
            backend,
            location: location.clone(),
            budget,
        }
    }

    pub fn location(&self) -> &Location {
        &self.location
    }

    pub fn display_path(&self, rel: &str) -> String {
        self.backend.display_path(rel)
    }

    pub fn read_text(&self, rel: &str) -> Result<Option<String>, CliError> {
        self.backend.read_file(rel)
    }

    pub fn write_text(&self, rel: &str, content: &str) -> Result<(), CliError> {
        if let Location::Dir(root) = &self.location {
            // Atomic and owner-only on a local directory; pk-cli-drive's
            // mount write is a plain overwrite.
            let path = root.join(rel);
            create_parent(&path)?;
            return pk_cli_drive::private_file::write(
                &path,
                content.as_bytes(),
                pk_cli_drive::private_file::Replace::Always,
            )
            .map_err(|e| CliError::Other(format!("writing {}: {e}", path.display())));
        }
        self.backend.write_file(rel, content)
    }

    /// Upload a local file (binary-safe), replacing any file at `rel`.
    pub fn put_file(&self, local: &Path, rel: &str) -> Result<(), CliError> {
        self.backend.put_file(local, rel)
    }

    /// The bytes of `rel` on local disk (the file itself on a directory; a
    /// download into an owner-only temp dir otherwise).
    pub fn fetch(&self, rel: &str) -> Result<LocalCopy, CliError> {
        self.backend.local_copy(rel)
    }

    pub fn read_bytes(&self, rel: &str) -> Result<Vec<u8>, CliError> {
        let copy = self.fetch(rel)?;
        std::fs::read(&copy.path)
            .map_err(|e| CliError::Other(format!("reading {}: {e}", self.display_path(rel))))
    }

    pub fn exists(&self, rel: &str) -> Result<bool, CliError> {
        self.backend.exists(rel)
    }

    /// Every file under `rel`, recursively, with sizes.
    pub fn list(&self, rel: &str) -> Result<Vec<FileEntry>, CliError> {
        self.backend.list_entries(rel)
    }

    /// Delete one file. `Ok(false)` when it was not there. pk-cli-drive
    /// refuses an empty path, a folder, an absolute path and `..`.
    pub fn delete(&self, rel: &str) -> Result<bool, CliError> {
        self.backend.delete_file(rel)
    }
}

/// A file in the store with Drive's MD5 (computed locally for a directory).
#[derive(Debug, Clone, Serialize)]
pub struct Hashed {
    pub rel: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub md5: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Space {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free: Option<u64>,
}

impl Store {
    fn spec_of(spec: &str, rel: &str) -> String {
        let rel = rel.trim_matches('/');
        if rel.is_empty() {
            spec.to_string()
        } else if spec.ends_with(':') {
            format!("{spec}{rel}")
        } else {
            format!("{spec}/{rel}")
        }
    }

    fn rclone(&self, args: &[&str], max_stdout: usize) -> Result<crate::proc::Ran, CliError> {
        let mut all = vec!["--log-level=ERROR"];
        all.extend_from_slice(args);
        crate::proc::Bounded::new("rclone")
            .args(all)
            .timeout(self.budget)
            .max_stdout(max_stdout)
            .run()
    }

    fn rclone_ok(&self, what: &str, args: &[&str]) -> Result<crate::proc::Ran, CliError> {
        let out = self.rclone(args, 256 * 1024 * 1024)?;
        if out.code != Some(0) {
            return Err(CliError::Upstream(format!(
                "rclone {what}: {}",
                out.stderr_tail
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("failed")
            )));
        }
        Ok(out)
    }

    /// Every file under the folder `rel` (recursively) with its MD5, in one
    /// listing. Use [`Store::md5`] for a single file.
    pub fn hashes(&self, rel: &str) -> Result<Vec<Hashed>, CliError> {
        let rel = rel.trim_matches('/');
        match &self.location {
            Location::Dir(root) => {
                let base = if rel.is_empty() {
                    root.clone()
                } else {
                    root.join(rel)
                };
                let mut out = Vec::new();
                if base.is_dir() {
                    walk(&base, &mut |p| {
                        let sub = p
                            .strip_prefix(root)
                            .unwrap_or(p)
                            .to_string_lossy()
                            .replace('\\', "/");
                        let size = p.metadata().map(|m| m.len()).unwrap_or(0);
                        out.push(Hashed {
                            rel: sub,
                            size,
                            md5: md5_file(p).ok(),
                        });
                    })?;
                } else if base.is_file() {
                    out.push(Hashed {
                        rel: rel.to_string(),
                        size: base.metadata().map(|m| m.len()).unwrap_or(0),
                        md5: md5_file(&base).ok(),
                    });
                }
                out.sort_by(|a, b| a.rel.cmp(&b.rel));
                Ok(out)
            }
            Location::Remote(spec) => {
                let target = Self::spec_of(spec, rel);
                let out = self.rclone(
                    &[
                        "lsjson",
                        "-R",
                        "--files-only",
                        "--no-mimetype",
                        "--hash",
                        "--hash-type",
                        "md5",
                        target.as_str(),
                    ],
                    256 * 1024 * 1024,
                )?;
                match out.code {
                    Some(0) => {}
                    Some(3) => return Ok(Vec::new()),
                    _ => {
                        return Err(CliError::Upstream(format!(
                            "rclone lsjson {target}: {}",
                            out.stderr_tail.lines().next().unwrap_or("failed")
                        )))
                    }
                }
                let v: Vec<Value> = serde_json::from_slice(&out.stdout).map_err(|e| {
                    CliError::Upstream(format!("rclone lsjson printed no JSON list: {e}"))
                })?;
                let mut files: Vec<Hashed> = v
                    .iter()
                    .filter_map(|e| {
                        let path = e.get("Path")?.as_str()?;
                        let rel_path = if rel.is_empty() {
                            path.to_string()
                        } else {
                            format!("{rel}/{path}")
                        };
                        Some(Hashed {
                            rel: rel_path,
                            size: e.get("Size").and_then(Value::as_u64).unwrap_or(0),
                            md5: e
                                .get("Hashes")
                                .and_then(|h| h.get("md5"))
                                .and_then(Value::as_str)
                                .map(str::to_ascii_lowercase),
                        })
                    })
                    .collect();
                files.sort_by(|a, b| a.rel.cmp(&b.rel));
                Ok(files)
            }
        }
    }

    /// The MD5 the store holds for one file; `None` when it is not there.
    pub fn md5(&self, rel: &str) -> Result<Option<String>, CliError> {
        match &self.location {
            Location::Dir(root) => {
                let p = root.join(rel);
                if !p.is_file() {
                    return Ok(None);
                }
                md5_file(&p).map(Some)
            }
            Location::Remote(spec) => {
                let (parent, name) = rel.rsplit_once('/').unwrap_or(("", rel));
                let target = Self::spec_of(spec, parent);
                let filter = format!("/{}", escape_filter(name));
                let out = self.rclone(
                    &[
                        "lsjson",
                        "--files-only",
                        "--no-mimetype",
                        "--hash",
                        "--hash-type",
                        "md5",
                        "--include",
                        filter.as_str(),
                        target.as_str(),
                    ],
                    1024 * 1024,
                )?;
                if out.code == Some(3) {
                    return Ok(None);
                }
                if out.code != Some(0) {
                    return Err(CliError::Upstream(format!(
                        "rclone lsjson {target}: {}",
                        out.stderr_tail.lines().next().unwrap_or("failed")
                    )));
                }
                let v: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
                Ok(v.iter()
                    .find(|e| e.get("Name").and_then(Value::as_str) == Some(name))
                    .and_then(|e| {
                        e.get("Hashes")?
                            .get("md5")?
                            .as_str()
                            .map(str::to_ascii_lowercase)
                    }))
            }
        }
    }

    /// Copy a local directory's contents under `rel` (one rclone call).
    pub fn put_tree(&self, local: &Path, rel: &str) -> Result<(), CliError> {
        match &self.location {
            Location::Dir(root) => copy_tree(local, &root.join(rel)),
            Location::Remote(spec) => {
                let src = local.to_string_lossy().into_owned();
                let dst = Self::spec_of(spec, rel);
                self.rclone_ok("copy", &["copy", src.as_str(), dst.as_str()])
                    .map(|_| ())
            }
        }
    }

    /// Copy everything under `rel` into a local directory.
    pub fn fetch_tree(&self, rel: &str, local: &Path) -> Result<(), CliError> {
        match &self.location {
            Location::Dir(root) => copy_tree(&root.join(rel), local),
            Location::Remote(spec) => {
                let src = Self::spec_of(spec, rel);
                let dst = local.to_string_lossy().into_owned();
                self.rclone_ok("copy", &["copy", src.as_str(), dst.as_str()])
                    .map(|_| ())
            }
        }
    }

    /// Move a file within the store; refuses to replace one.
    pub fn move_no_clobber(&self, from: &str, to: &str) -> Result<(), CliError> {
        self.backend.move_no_clobber(from, to)
    }

    /// What rclone (or the file system) reports as total and free space.
    pub fn space(&self) -> Result<Space, CliError> {
        match &self.location {
            Location::Dir(root) => Ok(statvfs_space(root)),
            Location::Remote(spec) => {
                let remote = format!("{}:", spec.split(':').next().unwrap_or(spec));
                let out = self.rclone_ok("about", &["about", "--json", remote.as_str()])?;
                let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
                Ok(Space {
                    total: v.get("total").and_then(Value::as_u64),
                    free: v.get("free").and_then(Value::as_u64),
                })
            }
        }
    }

    /// The location as rclone names it (a remote spec, or a local path).
    pub fn rclone_spec(&self) -> String {
        match &self.location {
            Location::Remote(s) => s.clone(),
            Location::Dir(d) => d.to_string_lossy().into_owned(),
        }
    }

    pub fn budget(&self) -> Duration {
        self.budget
    }
}

/// The background refresh a stale cached read spawns (`medrec cache
/// sync --revalidate-*`), built as [`Store::open`] built the cache.
pub fn revalidate(
    spec: &str,
    listing: bool,
    rel: &str,
    budget: Duration,
) -> Result<String, CliError> {
    let dir = cache::cache_dir_for(BIN, spec).ok_or_else(|| {
        CliError::Other("cannot resolve a cache directory (is $HOME set?)".into())
    })?;
    let kind = if listing {
        cache::Kind::Listing
    } else {
        cache::Kind::File
    };
    let outcome = cache::run_revalidation(
        remote(spec, budget),
        dir,
        CachePolicy::from_env(BIN),
        kind,
        rel,
    )?;
    Ok(outcome.as_str().to_string())
}

/// rclone filter syntax treats these as patterns; a file name is literal.
fn escape_filter(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub fn md5_file(path: &Path) -> Result<String, CliError> {
    let mut f = std::fs::File::open(path)
        .map_err(|e| CliError::Other(format!("reading {}: {e}", path.display())))?;
    let mut h = Md5::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| CliError::Other(format!("reading {}: {e}", path.display())))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(crate::hash::hex(&h.finalize()))
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) -> Result<(), CliError> {
    let rd = std::fs::read_dir(dir)
        .map_err(|e| CliError::Other(format!("listing {}: {e}", dir.display())))?;
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let ft = e.file_type().map_err(|e| CliError::Other(e.to_string()))?;
        if ft.is_dir() {
            walk(&p, f)?;
        } else if ft.is_file() {
            f(&p);
        }
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), CliError> {
    if from.is_file() {
        create_parent(to)?;
        std::fs::copy(from, to)
            .map_err(|e| CliError::Other(format!("copying {}: {e}", from.display())))?;
        return Ok(());
    }
    let mut files = Vec::new();
    walk(from, &mut |p| files.push(p.to_path_buf()))?;
    for p in files {
        let rel = p.strip_prefix(from).unwrap_or(&p);
        let dst = to.join(rel);
        create_parent(&dst)?;
        std::fs::copy(&p, &dst)
            .map_err(|e| CliError::Other(format!("copying {}: {e}", p.display())))?;
    }
    Ok(())
}

pub fn statvfs_space(path: &Path) -> Space {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return Space {
            total: None,
            free: None,
        };
    };
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: c is a valid NUL-terminated path; s is a zeroed statvfs.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return Space {
            total: None,
            free: None,
        };
    }
    let frsize = s.f_frsize as u64;
    Space {
        total: Some(s.f_blocks as u64 * frsize),
        free: Some(s.f_bavail as u64 * frsize),
    }
}

fn create_parent(path: &Path) -> Result<(), CliError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CliError::Other(format!("creating {}: {e}", parent.display())))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_parse_remotes_and_directories() {
        assert!(
            matches!(Location::parse("gdrive:Medical Records"), Location::Remote(s) if s == "gdrive:Medical Records")
        );
        assert!(matches!(Location::parse("gdrive:"), Location::Remote(_)));
        assert!(matches!(Location::parse("/tmp/records"), Location::Dir(_)));
        assert!(matches!(Location::parse("./a:b"), Location::Dir(_)));
        assert!(matches!(Location::parse("C:\\x"), Location::Dir(_)));
    }

    #[test]
    fn a_directory_store_writes_reads_lists_and_deletes() {
        let root = crate::testutil::temp_dir("store");
        let store = Store::open(&Location::Dir(root.clone()), Reads::Fresh, DEFAULT_BUDGET);
        store.write_text("a/b.txt", "hello").unwrap();
        assert_eq!(
            store.read_text("a/b.txt").unwrap().as_deref(),
            Some("hello")
        );
        assert_eq!(store.list("a").unwrap().len(), 1);
        assert!(store.delete("a/b.txt").unwrap());
        assert!(!store.delete("a/b.txt").unwrap());
        assert!(store.delete("a").is_err());
        assert!(store.delete("").is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_directory_store_hashes_copies_trees_and_moves_without_clobbering() {
        let root = crate::testutil::temp_dir("store-hash");
        let store = Store::open(&Location::Dir(root.clone()), Reads::Fresh, DEFAULT_BUDGET);
        store.write_text("p/a.txt", "abc").unwrap();
        assert_eq!(
            store.md5("p/a.txt").unwrap().as_deref(),
            Some("900150983cd24fb0d6963f7d28e17f72")
        );
        assert_eq!(store.md5("p/missing").unwrap(), None);
        let h = store.hashes("p").unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].rel, "p/a.txt");
        let local = crate::testutil::temp_dir("store-tree");
        std::fs::create_dir_all(local.join("s")).unwrap();
        std::fs::write(local.join("s/x.dcm"), b"x").unwrap();
        store.put_tree(&local, "q").unwrap();
        assert!(store.exists("q/s/x.dcm").unwrap());
        store.write_text("p/b.txt", "b").unwrap();
        assert!(store.move_no_clobber("p/a.txt", "p/b.txt").is_err());
        assert!(store.space().unwrap().free.is_some());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(local).unwrap();
    }
}
