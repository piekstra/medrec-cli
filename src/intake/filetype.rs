//! What a file is, from its bytes. A file's name and extension are the
//! sender's claim; intake decides from the content.

use pk_cli_core::CliError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    Png,
    Jpeg,
    Tiff,
    Heic,
    /// XML: a C-CDA document, or other XML a portal exports.
    Xml,
}

impl Kind {
    pub fn media_type(self) -> &'static str {
        match self {
            Kind::Pdf => "application/pdf",
            Kind::Png => "image/png",
            Kind::Jpeg => "image/jpeg",
            Kind::Tiff => "image/tiff",
            Kind::Heic => "image/heic",
            Kind::Xml => "application/xml",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Kind::Pdf => "pdf",
            Kind::Png => "png",
            Kind::Jpeg => "jpg",
            Kind::Tiff => "tiff",
            Kind::Heic => "heic",
            Kind::Xml => "xml",
        }
    }

    pub fn from_media_type(m: &str) -> Option<Kind> {
        Some(
            match m.split(';').next()?.trim().to_ascii_lowercase().as_str() {
                "application/pdf" => Kind::Pdf,
                "image/png" => Kind::Png,
                "image/jpeg" | "image/jpg" => Kind::Jpeg,
                "image/tiff" => Kind::Tiff,
                "image/heic" | "image/heif" => Kind::Heic,
                "application/xml" | "text/xml" | "application/hl7-cda+xml" => Kind::Xml,
                _ => return None,
            },
        )
    }

    pub fn is_image(self) -> bool {
        matches!(self, Kind::Png | Kind::Jpeg | Kind::Tiff | Kind::Heic)
    }
}

/// Identify `bytes`, or say why it is not a document intake accepts.
pub fn identify(bytes: &[u8]) -> Result<Kind, CliError> {
    if bytes.is_empty() {
        return Err(CliError::Usage("the file is empty".into()));
    }
    let kind = sniff(bytes).ok_or_else(|| {
        CliError::Usage(
            "not a PDF, PNG, JPEG, TIFF, HEIC or XML file (judged by its content, not its name)"
                .into(),
        )
    })?;
    // The family's download check for the shapes it covers: a PDF's magic,
    // and an XML body that is not an error page.
    match kind {
        Kind::Pdf => pk_cli_documents::verify_download(bytes, Some("pdf"))?,
        Kind::Xml => pk_cli_documents::verify_download(bytes, Some("xml"))?,
        _ => {}
    }
    Ok(kind)
}

pub fn sniff(b: &[u8]) -> Option<Kind> {
    if b.starts_with(b"%PDF-") {
        return Some(Kind::Pdf);
    }
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(Kind::Png);
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(Kind::Jpeg);
    }
    if b.starts_with(b"II*\0") || b.starts_with(b"MM\0*") {
        return Some(Kind::Tiff);
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        let brand = &b[8..12];
        let heif: [&[u8]; 8] = [
            b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"mif1", b"msf1",
        ];
        if heif.contains(&brand) {
            return Some(Kind::Heic);
        }
    }
    let text = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b);
    let start = text.iter().position(|c| !c.is_ascii_whitespace())?;
    let head = &text[start..text.len().min(start + 512)];
    if head.starts_with(b"<?xml") || head.starts_with(b"<ClinicalDocument") {
        return Some(Kind::Xml);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_decides_the_type() {
        assert_eq!(identify(b"%PDF-1.7\n...").unwrap(), Kind::Pdf);
        assert_eq!(
            identify(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]).unwrap(),
            Kind::Png
        );
        assert_eq!(identify(&[0xFF, 0xD8, 0xFF, 0xE0]).unwrap(), Kind::Jpeg);
        assert_eq!(identify(b"II*\0rest").unwrap(), Kind::Tiff);
        assert_eq!(identify(b"\0\0\0\x18ftypheic\0\0\0\0").unwrap(), Kind::Heic);
        assert_eq!(
            identify(b"\xEF\xBB\xBF  <?xml version=\"1.0\"?><ClinicalDocument/>").unwrap(),
            Kind::Xml
        );
    }

    #[test]
    fn other_files_and_error_pages_are_refused() {
        assert!(identify(b"").is_err());
        assert!(identify(b"<html><body>Sign in</body></html>").is_err());
        assert!(identify(b"{\"error\":\"expired\"}").is_err());
        assert!(identify(b"\0\0\0\x18ftypmp42").is_err());
        assert!(identify(b"PK\x03\x04").is_err());
    }
}
