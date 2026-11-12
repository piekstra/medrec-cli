//! Opening a document or a study: a private local copy for the viewer.
//!
//! The viewer runs after medrec exits, so the copy cannot live in a temp
//! directory removed on drop. Copies go under one owner-only directory
//! (`<cache>/medrec/open/`), each in its own `0700` subdirectory, and every
//! `open` removes the copies older than [`OPEN_TTL`]. `doctor` reports any
//! left.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use pk_cli_core::CliError;

pub const OPEN_TTL: Duration = Duration::from_secs(12 * 3600);

/// `<cache>/medrec/open`, created `0700` and checked to be ours.
pub fn open_root() -> Result<PathBuf, CliError> {
    let base = pk_cli_drive::cache::cache_base(crate::store::BIN).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("medrec-{}", unsafe { libc::getuid() }))
    });
    let root = base.join("open");
    private_dir_all(&root)?;
    Ok(root)
}

/// Create `dir` and any missing parents `0700`, then refuse it unless it
/// is a real directory owned by this user with no group or other access.
fn private_dir_all(dir: &Path) -> Result<(), CliError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))?;
    let md = std::fs::symlink_metadata(dir)
        .map_err(|e| CliError::Other(format!("{}: {e}", dir.display())))?;
    let uid = unsafe { libc::getuid() };
    if !md.is_dir() || md.uid() != uid {
        return Err(CliError::Other(format!(
            "{} is not a directory owned by you; remove it and try again",
            dir.display()
        )));
    }
    if md.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| CliError::Other(format!("restricting {}: {e}", dir.display())))?;
    }
    Ok(())
}

/// A fresh `0700` directory for one opened item, after pruning old ones.
pub fn new_open_dir(label: &str) -> Result<PathBuf, CliError> {
    use std::os::unix::fs::DirBuilderExt;
    let root = open_root()?;
    prune(&root, OPEN_TTL);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = root.join(format!("{}-{nanos}", crate::folder::slug(label, 40)));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))?;
    Ok(dir)
}

/// Remove copies older than `ttl`; returns how many were removed.
pub fn prune(root: &Path, ttl: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > ttl);
        if old && std::fs::remove_dir_all(e.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The open copies that remain, for `doctor`.
pub fn leftovers() -> usize {
    open_root()
        .ok()
        .and_then(|r| std::fs::read_dir(r).ok())
        .map(|d| d.flatten().count())
        .unwrap_or(0)
}

/// How a path is handed to a viewer.
#[derive(Debug, Clone, PartialEq)]
pub enum Viewer {
    /// Weasis, through its `weasis://` URL scheme (`$dicom:get -l <dir>`
    /// loads a local folder; nothing leaves the machine).
    Weasis,
    /// A macOS application bundle: `open -a <app> <path>`.
    App(String),
    /// An executable, run as `<exe> <path>`.
    Exe(String),
    /// The system default for the file (`open` / `xdg-open`).
    System,
    /// Launch nothing; print the path.
    None,
}

impl Viewer {
    /// From `config viewer`: unset means Weasis; `none`, `system`, a
    /// `.app`, or an executable.
    pub fn for_images(configured: Option<&str>) -> Viewer {
        match configured.map(str::trim) {
            None | Some("") => Viewer::Weasis,
            Some(v) => Viewer::parse(v),
        }
    }

    pub fn for_documents(configured: Option<&str>) -> Viewer {
        match configured.map(str::trim) {
            None | Some("") => Viewer::System,
            Some(v) => Viewer::parse(v),
        }
    }

    fn parse(v: &str) -> Viewer {
        match v {
            "none" => Viewer::None,
            "system" => Viewer::System,
            "weasis" => Viewer::Weasis,
            v if v.trim_end_matches('/').ends_with(".app") => Viewer::App(v.into()),
            v => Viewer::Exe(v.into()),
        }
    }

    /// The command that opens `path`, or `None` for [`Viewer::None`].
    pub fn command(&self, path: &Path) -> Option<Vec<OsString>> {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let p = path.as_os_str().to_owned();
        Some(match self {
            Viewer::None => return None,
            Viewer::System => vec![opener.into(), p],
            Viewer::Weasis => vec![opener.into(), weasis_url(path).into()],
            Viewer::App(app) => vec!["open".into(), "-a".into(), app.into(), p],
            Viewer::Exe(exe) => vec![exe.into(), p],
        })
    }
}

/// `weasis://$dicom:get -l "<dir>"`, percent-encoded.
pub fn weasis_url(dir: &Path) -> String {
    let arg = format!("$dicom:get -l \"{}\"", dir.display());
    let mut out = String::from("weasis://");
    for b in arg.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Start the viewer detached: no stdin, output discarded, not waited for.
pub fn launch(cmd: &[OsString]) -> Result<(), CliError> {
    let (prog, args) = cmd
        .split_first()
        .ok_or_else(|| CliError::Other("no viewer command".into()))?;
    Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            CliError::Other(format!(
                "could not start the viewer `{}`: {e}; set another with `medrec config set viewer <app or command>`",
                prog.to_string_lossy()
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewers_parse_and_weasis_gets_a_local_folder_url() {
        assert_eq!(Viewer::for_images(None), Viewer::Weasis);
        assert_eq!(Viewer::for_images(Some("none")), Viewer::None);
        assert_eq!(
            Viewer::for_images(Some("/Applications/Horos.app")),
            Viewer::App("/Applications/Horos.app".into())
        );
        assert_eq!(Viewer::for_documents(None), Viewer::System);
        assert!(Viewer::None.command(Path::new("/x")).is_none());
        assert_eq!(
            weasis_url(Path::new("/tmp/a b")),
            "weasis://%24dicom%3Aget%20-l%20%22/tmp/a%20b%22"
        );
    }

    #[test]
    fn old_copies_are_pruned() {
        let root = crate::testutil::temp_dir("open-prune");
        std::fs::create_dir(root.join("fresh")).unwrap();
        assert_eq!(prune(&root, Duration::from_secs(3600)), 0);
        assert_eq!(prune(&root, Duration::ZERO), 1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
