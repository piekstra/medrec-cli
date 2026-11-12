//! medrec: a household's medical records in a plain Drive folder. The
//! binary's commands live in `main.rs`; the mechanisms they use live here so
//! the tests can reach them.

pub mod config;
pub mod dicom;
pub mod folder;
pub mod hash;
pub mod ids;
pub mod imaging;
pub mod import;
pub mod intake;
pub mod maint;
pub mod model;
pub mod normalize;
pub mod notes;
pub mod pdfgen;
pub mod people;
pub mod proc;
pub mod prompt;
pub mod review;
pub mod store;
pub mod stream;
#[doc(hidden)]
pub mod testdata;
pub mod view;
pub mod views;

/// Create `path` owner-only (0600) and write `bytes`; never replaces a file.
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), pk_cli_core::CliError> {
    pk_cli_drive::private_file::write(path, bytes, pk_cli_drive::private_file::Replace::Never)
        .map_err(|e| pk_cli_core::CliError::Other(format!("writing {}: {e}", path.display())))
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fresh directory under the system temp dir, unique per call.
    pub fn temp_dir(label: &str) -> PathBuf {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "medrec-test-{label}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
