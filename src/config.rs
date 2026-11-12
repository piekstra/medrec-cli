//! Non-secret, per-machine settings (`~/.config/medrec/config.json`).

use pk_cli_config::ConfigStore;
use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};

use crate::store::Location;

pub const KEYS: &str = "folder, mount, inbox, me, viewer, ocr_languages, ocr_max_pages, \
                        max_file_mb, providers, import_timeout_secs, import_max_mb, rclone_budget_secs";

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The records folder: `<rclone remote>:<path>` (canonical) or a local
    /// directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Where the records folder is mounted locally (Google Drive for
    /// desktop), if it is. Optional: medrec reads and writes through the
    /// remote, and `doctor` only checks that the mount shows the same folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
    /// The inbox `--inbox` drains: `<remote>:<path>` or a local directory.
    /// Default: `inbox/` in the records folder. Files there are moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox: Option<String>,
    /// The person who uses medrec on this machine (a person id), named as
    /// the author of notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<String>,
    /// The DICOM viewer `images open` launches: an `.app` bundle or an
    /// executable. Default: Weasis in /Applications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<String>,
    /// tesseract languages, `eng` by default (`eng+spa` for two).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_languages: Option<String>,
    /// Pages OCR'd per document (default 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_max_pages: Option<u32>,
    /// Largest file intake accepts, MiB (default 200).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_mb: Option<u64>,
    /// Provider CLIs to consider besides the known ones (comma-separated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<String>,
    /// How long one provider export may run (default 1800 s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_timeout_secs: Option<u64>,
    /// Largest records stream one import reads, MiB (default 4096). An
    /// import holds the stream until its trailer, so this bounds memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_max_mb: Option<u64>,
    /// How long all of one command's rclone calls may take (default 1800 s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rclone_budget_secs: Option<u64>,
}

impl Config {
    pub fn load(store: &ConfigStore) -> Result<Self, CliError> {
        store.load()
    }

    pub fn folder_location(&self) -> Result<Location, CliError> {
        self.folder.as_deref().map(Location::parse).ok_or_else(|| {
            CliError::Usage(
                "no records folder configured; run `medrec init <remote:path|dir>` or \
                 `medrec config set folder <remote:path|dir>`"
                    .into(),
            )
        })
    }

    /// The inbox: configured, else `inbox/` in the records folder.
    pub fn inbox_location(&self) -> Result<Location, CliError> {
        if let Some(i) = &self.inbox {
            return Ok(Location::parse(i));
        }
        Ok(match self.folder_location()? {
            Location::Remote(s) if s.ends_with(':') => {
                Location::Remote(format!("{s}{}", crate::folder::INBOX))
            }
            Location::Remote(s) => Location::Remote(format!("{s}/{}", crate::folder::INBOX)),
            Location::Dir(d) => Location::Dir(d.join(crate::folder::INBOX)),
        })
    }

    pub fn ocr_languages(&self) -> String {
        self.ocr_languages.clone().unwrap_or_else(|| "eng".into())
    }

    pub fn ocr_max_pages(&self) -> u32 {
        self.ocr_max_pages.unwrap_or(20).clamp(1, 500)
    }

    pub fn max_file_bytes(&self) -> u64 {
        self.max_file_mb.unwrap_or(200).clamp(1, 4096) * 1024 * 1024
    }

    pub fn import_max_bytes(&self) -> u64 {
        self.import_max_mb.unwrap_or(4096).clamp(16, 65_536) * 1024 * 1024
    }

    pub fn import_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.import_timeout_secs.unwrap_or(1800).clamp(10, 86_400))
    }

    pub fn rclone_budget(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.rclone_budget_secs.unwrap_or(1800).clamp(10, 86_400))
    }

    pub fn extra_providers(&self) -> Vec<String> {
        self.providers
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub fn set(&mut self, key: &str, value: &str) -> Result<(), CliError> {
        let num = |v: &str| {
            v.parse::<u64>()
                .map_err(|_| CliError::Usage(format!("`{key}` takes a whole number, not `{v}`")))
        };
        match key {
            "folder" => self.folder = Some(value.into()),
            "mount" => self.mount = Some(value.into()),
            "inbox" => self.inbox = Some(value.into()),
            "me" => {
                if !crate::ids::is_valid("p", value) {
                    return Err(CliError::Usage(format!(
                        "`{value}` is not a person id (see `medrec people list`)"
                    )));
                }
                self.me = Some(value.into())
            }
            "viewer" => self.viewer = Some(value.into()),
            "ocr_languages" => {
                if value.is_empty()
                    || !value
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '_')
                {
                    return Err(CliError::Usage(
                        "ocr_languages takes tesseract codes like `eng` or `eng+spa`".into(),
                    ));
                }
                self.ocr_languages = Some(value.into())
            }
            "ocr_max_pages" => self.ocr_max_pages = Some(num(value)? as u32),
            "max_file_mb" => self.max_file_mb = Some(num(value)?),
            "providers" => self.providers = Some(value.into()),
            "import_timeout_secs" => self.import_timeout_secs = Some(num(value)?),
            "import_max_mb" => self.import_max_mb = Some(num(value)?),
            "rclone_budget_secs" => self.rclone_budget_secs = Some(num(value)?),
            other => {
                return Err(CliError::Usage(format!(
                    "unknown config key `{other}` (known: {KEYS})"
                )))
            }
        }
        Ok(())
    }

    pub fn unset(&mut self, key: &str) -> Result<(), CliError> {
        match key {
            "folder" => self.folder = None,
            "mount" => self.mount = None,
            "inbox" => self.inbox = None,
            "me" => self.me = None,
            "viewer" => self.viewer = None,
            "ocr_languages" => self.ocr_languages = None,
            "ocr_max_pages" => self.ocr_max_pages = None,
            "max_file_mb" => self.max_file_mb = None,
            "providers" => self.providers = None,
            "import_timeout_secs" => self.import_timeout_secs = None,
            "import_max_mb" => self.import_max_mb = None,
            "rclone_budget_secs" => self.rclone_budget_secs = None,
            other => {
                return Err(CliError::Usage(format!(
                    "unknown config key `{other}` (known: {KEYS})"
                )))
            }
        }
        Ok(())
    }
}
