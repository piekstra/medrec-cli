//! Text from a document, on this machine only: `pdftotext` for PDFs with a
//! text layer, `pdftoppm` + `tesseract` for pages without one and for
//! images, `sips` to turn a HEIC photo into something tesseract reads, and
//! a tag stripper for XML. Every call is bounded; scratch files live in an
//! owner-only temp directory removed when the extraction ends.
//!
//! A missing tool narrows what can be read and says so in `note`; it never
//! fails the intake.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use crate::intake::filetype::Kind;
use crate::proc::{which, Bounded};

/// Text a page needs before it counts as having a text layer.
const READABLE_CHARS: usize = 16;
/// Total text kept per document.
pub const MAX_TEXT: usize = 2 * 1024 * 1024;
/// Pages `pdftotext` reads.
const MAX_TEXT_PAGES: u32 = 200;
const PDFTOTEXT_TIMEOUT: Duration = Duration::from_secs(60);
const RASTER_TIMEOUT: Duration = Duration::from_secs(90);
const OCR_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub struct Tools {
    pub pdftotext: Option<PathBuf>,
    pub pdftoppm: Option<PathBuf>,
    pub tesseract: Option<PathBuf>,
    pub sips: Option<PathBuf>,
    versions: OnceLock<(Option<String>, Option<String>)>,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub languages: String,
    pub max_ocr_pages: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Extracted {
    pub text: String,
    pub engines: Vec<String>,
    pub pages: Option<u32>,
    pub ocr_pages: u32,
    pub note: Option<String>,
}

impl Tools {
    pub fn from_path() -> Self {
        Tools {
            pdftotext: which("pdftotext"),
            pdftoppm: which("pdftoppm"),
            tesseract: which("tesseract"),
            sips: which("sips"),
            versions: OnceLock::new(),
        }
    }

    /// `(pdftotext version, tesseract version)`, each probed once.
    pub fn versions(&self) -> &(Option<String>, Option<String>) {
        self.versions.get_or_init(|| {
            let poppler = self.pdftotext.as_ref().and_then(|p| {
                let r = Bounded::new(p)
                    .arg("-v")
                    .timeout(Duration::from_secs(10))
                    .run()
                    .ok()?;
                let all = format!("{}{}", String::from_utf8_lossy(&r.stdout), r.stderr_tail);
                all.lines()
                    .find(|l| l.contains("version"))
                    .and_then(|l| l.split_whitespace().last())
                    .map(|v| format!("pdftotext {v}"))
            });
            let tess = self.tesseract.as_ref().and_then(|p| {
                let r = Bounded::new(p)
                    .arg("--version")
                    .timeout(Duration::from_secs(10))
                    .run()
                    .ok()?;
                let all = format!("{}{}", String::from_utf8_lossy(&r.stdout), r.stderr_tail);
                all.lines()
                    .find(|l| l.trim_start().starts_with("tesseract"))
                    .map(|l| l.trim().to_string())
            });
            (poppler, tess)
        })
    }

    fn poppler_engine(&self) -> String {
        self.versions()
            .0
            .clone()
            .unwrap_or_else(|| "pdftotext".into())
    }

    fn ocr_engine(&self) -> String {
        self.versions()
            .1
            .clone()
            .unwrap_or_else(|| "tesseract".into())
    }
}

/// Extract text from the file at `path`, identified as `kind`.
pub fn extract(tools: &Tools, kind: Kind, path: &Path, opts: &Options) -> Extracted {
    let mut out = match kind {
        Kind::Pdf => pdf(tools, path, opts),
        Kind::Png | Kind::Jpeg | Kind::Tiff => image(tools, path, opts),
        Kind::Heic => heic(tools, path, opts),
        Kind::Xml => xml(path),
    };
    truncate(&mut out.text, MAX_TEXT);
    out
}

fn pdf(tools: &Tools, path: &Path, opts: &Options) -> Extracted {
    let mut ex = Extracted::default();
    let mut notes = Vec::new();
    let mut pages: Vec<String> = Vec::new();
    match &tools.pdftotext {
        Some(bin) => {
            let r = Bounded::new(bin)
                .args(["-layout", "-enc", "UTF-8", "-f", "1", "-l"])
                .arg(MAX_TEXT_PAGES.to_string())
                .arg(path)
                .arg("-")
                .timeout(PDFTOTEXT_TIMEOUT)
                .max_stdout(MAX_TEXT * 4)
                .run();
            match r {
                Ok(r) if r.code == Some(0) => {
                    let text = String::from_utf8_lossy(&r.stdout).into_owned();
                    // pdftotext ends every page with a form feed.
                    pages = text.split('\u{c}').map(str::to_string).collect();
                    if pages.last().is_some_and(|p| p.trim().is_empty()) {
                        pages.pop();
                    }
                    ex.engines.push(tools.poppler_engine());
                }
                Ok(r) => notes.push(format!(
                    "pdftotext failed (exit {}): {}",
                    r.code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "signal".into()),
                    r.stderr_tail.lines().next().unwrap_or("")
                )),
                Err(e) => notes.push(format!("pdftotext: {e}")),
            }
        }
        None => notes.push("pdftotext not installed: no text layer read".into()),
    }
    ex.pages = (!pages.is_empty()).then_some(pages.len() as u32);
    // Pages with no usable text layer get OCR. With no page list (no
    // pdftotext), OCR runs page by page until a page fails to render.
    let unreadable: Vec<u32> = if pages.is_empty() {
        (1..=opts.max_ocr_pages).collect()
    } else {
        pages
            .iter()
            .enumerate()
            .filter(|(_, t)| !readable(t))
            .map(|(i, _)| i as u32 + 1)
            .collect()
    };
    if !unreadable.is_empty() {
        match (&tools.pdftoppm, &tools.tesseract) {
            (Some(ppm), Some(tess)) => match pk_cli_drive::private_temp_dir("medrec-ocr") {
                Ok(dir) => {
                    let mut done = 0u32;
                    for &page in unreadable.iter().take(opts.max_ocr_pages as usize) {
                        let base = dir.path().join(format!("p{page}"));
                        let png = dir.path().join(format!("p{page}.png"));
                        let rendered = Bounded::new(ppm)
                            .args(["-r", "300", "-f"])
                            .arg(page.to_string())
                            .arg("-l")
                            .arg(page.to_string())
                            .args(["-png", "-singlefile"])
                            .arg(path)
                            .arg(&base)
                            .timeout(RASTER_TIMEOUT)
                            .run();
                        match rendered {
                            Ok(r) if r.code == Some(0) && png.exists() => {}
                            _ if pages.is_empty() => break,
                            _ => {
                                notes.push(format!("page {page} did not render for OCR"));
                                continue;
                            }
                        }
                        match ocr(tess, &png, opts) {
                            Ok(t) => {
                                let idx = page as usize - 1;
                                if idx < pages.len() {
                                    pages[idx] = t;
                                } else {
                                    pages.push(t);
                                }
                                done += 1;
                            }
                            Err(e) => notes.push(format!("OCR of page {page}: {e}")),
                        }
                        let _ = std::fs::remove_file(&png);
                    }
                    if unreadable.len() > opts.max_ocr_pages as usize {
                        notes.push(format!(
                            "OCR stopped at {} pages (config ocr_max_pages)",
                            opts.max_ocr_pages
                        ));
                    }
                    if done > 0 {
                        ex.engines.push(tools.ocr_engine());
                        ex.ocr_pages = done;
                        if ex.pages.is_none() {
                            ex.pages = Some(pages.len() as u32);
                        }
                    }
                }
                Err(e) => notes.push(format!("no scratch directory for OCR: {e}")),
            },
            _ => notes.push(format!(
                "{} page(s) have no text layer and OCR is unavailable (needs pdftoppm and tesseract)",
                unreadable.len()
            )),
        }
    }
    ex.text = pages.join("\n\u{c}\n");
    ex.note = (!notes.is_empty()).then(|| notes.join("; "));
    ex
}

fn image(tools: &Tools, path: &Path, opts: &Options) -> Extracted {
    let mut ex = Extracted {
        pages: Some(1),
        ..Default::default()
    };
    match &tools.tesseract {
        Some(tess) => match ocr(tess, path, opts) {
            Ok(t) => {
                ex.text = t;
                ex.engines.push(tools.ocr_engine());
                ex.ocr_pages = 1;
            }
            Err(e) => ex.note = Some(format!("OCR: {e}")),
        },
        None => ex.note = Some("tesseract not installed: image not read".into()),
    }
    ex
}

fn heic(tools: &Tools, path: &Path, opts: &Options) -> Extracted {
    let Some(sips) = &tools.sips else {
        return Extracted {
            pages: Some(1),
            note: Some("HEIC needs macOS `sips` to convert before OCR; not read".into()),
            ..Default::default()
        };
    };
    let dir = match pk_cli_drive::private_temp_dir("medrec-heic") {
        Ok(d) => d,
        Err(e) => {
            return Extracted {
                note: Some(format!("no scratch directory: {e}")),
                ..Default::default()
            }
        }
    };
    let png = dir.path().join("photo.png");
    let r = Bounded::new(sips)
        .args(["-s", "format", "png"])
        .arg(path)
        .arg("--out")
        .arg(&png)
        .timeout(RASTER_TIMEOUT)
        .run();
    match r {
        Ok(r) if r.code == Some(0) && png.exists() => image(tools, &png, opts),
        _ => Extracted {
            pages: Some(1),
            note: Some("sips could not convert the HEIC photo; not read".into()),
            ..Default::default()
        },
    }
}

fn ocr(tesseract: &Path, image: &Path, opts: &Options) -> Result<String, String> {
    let r = Bounded::new(tesseract)
        .arg(image)
        .arg("stdout")
        .arg("-l")
        .arg(&opts.languages)
        .timeout(OCR_TIMEOUT)
        .max_stdout(MAX_TEXT)
        .run()
        .map_err(|e| e.to_string())?;
    if r.code != Some(0) {
        return Err(format!(
            "tesseract exit {}: {}",
            r.code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into()),
            r.stderr_tail.lines().last().unwrap_or("")
        ));
    }
    Ok(String::from_utf8_lossy(&r.stdout).into_owned())
}

fn xml(path: &Path) -> Extracted {
    let mut ex = Extracted::default();
    match std::fs::read(path) {
        Ok(bytes) => {
            ex.text = strip_tags(&String::from_utf8_lossy(&bytes));
            ex.engines.push("xml".into());
        }
        Err(e) => ex.note = Some(format!("reading XML: {e}")),
    }
    ex
}

/// Element text of an XML document, one run per line. Not a parser: it
/// drops markup, comments and processing instructions and decodes the five
/// predefined entities, which is all search and classification need.
pub fn strip_tags(xml: &str) -> String {
    let mut out = String::with_capacity(xml.len() / 2);
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        push_text(&mut out, &rest[..lt]);
        let after = &rest[lt..];
        let end = if after.starts_with("<!--") {
            after.find("-->").map(|i| i + 3)
        } else if after.starts_with("<![CDATA[") {
            if let Some(i) = after.find("]]>") {
                push_text(&mut out, &after[9..i]);
                Some(i + 3)
            } else {
                None
            }
        } else {
            after.find('>').map(|i| i + 1)
        };
        match end {
            Some(e) => rest = &after[e..],
            None => {
                rest = "";
            }
        }
    }
    push_text(&mut out, rest);
    out
}

fn push_text(out: &mut String, raw: &str) {
    let t = raw.trim();
    if t.is_empty() {
        return;
    }
    let decoded = t
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    out.push_str(&decoded.split_whitespace().collect::<Vec<_>>().join(" "));
    out.push('\n');
}

fn readable(t: &str) -> bool {
    t.chars().filter(|c| c.is_alphanumeric()).count() >= READABLE_CHARS
}

fn truncate(s: &mut String, max: usize) {
    if s.len() > max {
        let mut cut = max;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_text_is_element_text() {
        let t = strip_tags(
            "<?xml version=\"1.0\"?><!-- c --><ClinicalDocument><title>Visit &amp; Summary</title>\
             <text><![CDATA[a <b> c]]></text><p>  spaced   out </p></ClinicalDocument>",
        );
        assert_eq!(t, "Visit & Summary\na <b> c\nspaced out\n");
    }

    #[test]
    fn readability_counts_letters_and_digits() {
        assert!(!readable("  \n -- \n"));
        assert!(readable("Laboratory report 2026"));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let mut s = "ééé".to_string();
        truncate(&mut s, 3);
        assert_eq!(s, "é");
    }

    #[test]
    fn a_text_pdf_reads_without_ocr_when_poppler_is_installed() {
        let tools = Tools::from_path();
        if tools.pdftotext.is_none() {
            eprintln!("skipped: pdftotext not installed");
            return;
        }
        let dir = crate::testutil::temp_dir("pdf");
        let path = dir.join("doc.pdf");
        std::fs::write(
            &path,
            crate::pdfgen::text_pdf(&["Synthetic Lab Report", "Reference range 1-2"]),
        )
        .unwrap();
        let ex = extract(
            &tools,
            Kind::Pdf,
            &path,
            &Options {
                languages: "eng".into(),
                max_ocr_pages: 2,
            },
        );
        assert!(ex.text.contains("Synthetic Lab Report"), "{:?}", ex);
        assert_eq!(ex.pages, Some(1));
        assert_eq!(ex.ocr_pages, 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
