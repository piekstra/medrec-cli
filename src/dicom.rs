//! DICOM Part 10 headers, as agreed with medical-common for `pk-med-dicom`
//! (v0.3.0): `read_header`, `Header`, `is_dicom`, `read_dicomdir`,
//! `StudySummary`, and a synthetic writer for fixtures. Kept in this one
//! module so pinning the crate replaces it without touching the importer.
//!
//! Reads the file meta group and the data set up to Pixel Data, never the
//! pixels: explicit and implicit VR little endian, and sequences of defined
//! or undefined length. Deflated and big-endian data sets are refused (the
//! meta group, which names the instance, is still read).

use std::collections::BTreeMap;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

const PREAMBLE: usize = 128;
const PIXEL_DATA: u32 = 0x7FE0_0010;
const ITEM: u32 = 0xFFFE_E000;
const ITEM_END: u32 = 0xFFFE_E00D;
const SEQ_END: u32 = 0xFFFE_E0DD;
const UNDEFINED: u32 = 0xFFFF_FFFF;
/// A string element longer than this is skipped, not read.
const MAX_VALUE: u32 = 64 * 1024;

pub const IMPLICIT_LE: &str = "1.2.840.10008.1.2";
pub const EXPLICIT_LE: &str = "1.2.840.10008.1.2.1";
pub const DEFLATED_LE: &str = "1.2.840.10008.1.2.1.99";
pub const EXPLICIT_BE: &str = "1.2.840.10008.1.2.2";

pub mod tags {
    pub const TRANSFER_SYNTAX: u32 = 0x0002_0010;
    pub const MEDIA_SOP_CLASS: u32 = 0x0002_0002;
    pub const MEDIA_SOP_INSTANCE: u32 = 0x0002_0003;
    pub const SOP_CLASS: u32 = 0x0008_0016;
    pub const SOP_INSTANCE: u32 = 0x0008_0018;
    pub const STUDY_DATE: u32 = 0x0008_0020;
    pub const STUDY_TIME: u32 = 0x0008_0030;
    pub const ACCESSION: u32 = 0x0008_0050;
    pub const MODALITY: u32 = 0x0008_0060;
    pub const INSTITUTION: u32 = 0x0008_0080;
    pub const REFERRING: u32 = 0x0008_0090;
    pub const STUDY_DESCRIPTION: u32 = 0x0008_1030;
    pub const SERIES_DESCRIPTION: u32 = 0x0008_103E;
    pub const PATIENT_NAME: u32 = 0x0010_0010;
    pub const PATIENT_ID: u32 = 0x0010_0020;
    pub const PATIENT_BIRTH_DATE: u32 = 0x0010_0030;
    pub const BODY_PART: u32 = 0x0018_0015;
    pub const STUDY_UID: u32 = 0x0020_000D;
    pub const SERIES_UID: u32 = 0x0020_000E;
    pub const SERIES_NUMBER: u32 = 0x0020_0011;
    pub const INSTANCE_NUMBER: u32 = 0x0020_0013;
    pub const NUMBER_OF_FRAMES: u32 = 0x0028_0008;
    pub const DIR_RECORDS: u32 = 0x0004_1220;
    pub const DIR_RECORD_TYPE: u32 = 0x0004_1430;
    pub const REFERENCED_FILE_ID: u32 = 0x0004_1500;
    pub const REFERENCED_SOP_INSTANCE: u32 = 0x0004_1511;
}

/// The string values a header reader keeps.
const KEPT: &[u32] = &[
    tags::TRANSFER_SYNTAX,
    tags::MEDIA_SOP_CLASS,
    tags::MEDIA_SOP_INSTANCE,
    tags::SOP_CLASS,
    tags::SOP_INSTANCE,
    tags::STUDY_DATE,
    tags::STUDY_TIME,
    tags::ACCESSION,
    tags::MODALITY,
    tags::INSTITUTION,
    tags::REFERRING,
    tags::STUDY_DESCRIPTION,
    tags::SERIES_DESCRIPTION,
    tags::PATIENT_NAME,
    tags::PATIENT_ID,
    tags::PATIENT_BIRTH_DATE,
    tags::BODY_PART,
    tags::STUDY_UID,
    tags::SERIES_UID,
    tags::SERIES_NUMBER,
    tags::INSTANCE_NUMBER,
    tags::NUMBER_OF_FRAMES,
];

#[derive(Debug)]
pub enum DicomError {
    Io(io::Error),
    NotDicom,
    Unsupported(String),
    Malformed(String),
}

impl std::fmt::Display for DicomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DicomError::Io(e) => write!(f, "reading DICOM: {e}"),
            DicomError::NotDicom => write!(f, "not a DICOM Part 10 file (no DICM marker)"),
            DicomError::Unsupported(s) => write!(f, "unsupported DICOM encoding: {s}"),
            DicomError::Malformed(s) => write!(f, "malformed DICOM: {s}"),
        }
    }
}

impl From<io::Error> for DicomError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            DicomError::Malformed("the file ends inside an element".into())
        } else {
            DicomError::Io(e)
        }
    }
}

/// One instance's header values. Patient fields have getters like the rest
/// but `Debug` leaves them out, so a log line never carries them.
#[derive(Clone, Default)]
pub struct Header {
    values: BTreeMap<u32, String>,
}

impl std::fmt::Debug for Header {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shown: BTreeMap<String, &String> = self
            .values
            .iter()
            .filter(|(t, _)| (**t >> 16) != 0x0010)
            .map(|(t, v)| (format!("{:04X},{:04X}", t >> 16, t & 0xFFFF), v))
            .collect();
        f.debug_struct("Header").field("values", &shown).finish()
    }
}

impl Header {
    pub fn get(&self, tag: u32) -> Option<&str> {
        self.values
            .get(&tag)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }
    pub fn transfer_syntax(&self) -> Option<&str> {
        self.get(tags::TRANSFER_SYNTAX)
    }
    pub fn sop_class_uid(&self) -> Option<&str> {
        self.get(tags::SOP_CLASS)
            .or_else(|| self.get(tags::MEDIA_SOP_CLASS))
    }
    pub fn sop_instance_uid(&self) -> Option<&str> {
        self.get(tags::SOP_INSTANCE)
            .or_else(|| self.get(tags::MEDIA_SOP_INSTANCE))
    }
    pub fn study_uid(&self) -> Option<&str> {
        self.get(tags::STUDY_UID)
    }
    pub fn series_uid(&self) -> Option<&str> {
        self.get(tags::SERIES_UID)
    }
    pub fn modality(&self) -> Option<&str> {
        self.get(tags::MODALITY)
    }
    pub fn body_part(&self) -> Option<&str> {
        self.get(tags::BODY_PART)
    }
    /// `YYYY-MM-DD` from the DA value `YYYYMMDD`.
    pub fn study_date(&self) -> Option<String> {
        da_to_iso(self.get(tags::STUDY_DATE)?)
    }
    pub fn study_time(&self) -> Option<&str> {
        self.get(tags::STUDY_TIME)
    }
    pub fn study_description(&self) -> Option<&str> {
        self.get(tags::STUDY_DESCRIPTION)
    }
    pub fn series_description(&self) -> Option<&str> {
        self.get(tags::SERIES_DESCRIPTION)
    }
    pub fn series_number(&self) -> Option<i64> {
        self.get(tags::SERIES_NUMBER)?.trim().parse().ok()
    }
    pub fn instance_number(&self) -> Option<i64> {
        self.get(tags::INSTANCE_NUMBER)?.trim().parse().ok()
    }
    pub fn number_of_frames(&self) -> Option<u32> {
        self.get(tags::NUMBER_OF_FRAMES)?.trim().parse().ok()
    }
    pub fn institution(&self) -> Option<&str> {
        self.get(tags::INSTITUTION)
    }
    pub fn accession(&self) -> Option<&str> {
        self.get(tags::ACCESSION)
    }
    /// PN with `^` between components, as `Given Family`.
    pub fn referring_physician(&self) -> Option<String> {
        self.get(tags::REFERRING).map(person_name)
    }
    pub fn patient_name(&self) -> Option<String> {
        self.get(tags::PATIENT_NAME).map(person_name)
    }
    pub fn patient_id(&self) -> Option<&str> {
        self.get(tags::PATIENT_ID)
    }
    pub fn patient_birth_date(&self) -> Option<String> {
        da_to_iso(self.get(tags::PATIENT_BIRTH_DATE)?)
    }
}

fn da_to_iso(da: &str) -> Option<String> {
    let d: String = da.chars().filter(char::is_ascii_digit).collect();
    if d.len() != 8 {
        return None;
    }
    let (y, m, day) = (
        d[..4].parse().ok()?,
        d[4..6].parse().ok()?,
        d[6..8].parse().ok()?,
    );
    crate::intake::classify::valid_iso(y, m, day)
}

/// `Family^Given^Middle^Prefix^Suffix` as `Prefix Given Middle Family Suffix`.
fn person_name(pn: &str) -> String {
    let alpha = pn.split('=').next().unwrap_or(pn);
    let parts: Vec<&str> = alpha.split('^').map(str::trim).collect();
    let get = |i: usize| parts.get(i).copied().unwrap_or("");
    [get(3), get(1), get(2), get(0), get(4)]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A Part 10 file: a 128-byte preamble, then `DICM`.
pub fn is_dicom(bytes: &[u8]) -> bool {
    bytes.len() >= PREAMBLE + 4 && &bytes[PREAMBLE..PREAMBLE + 4] == b"DICM"
}

pub fn is_dicom_file(path: &Path) -> bool {
    let mut buf = [0u8; PREAMBLE + 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut buf))
        .map(|_| is_dicom(&buf))
        .unwrap_or(false)
}

/// Read one instance's header, stopping at Pixel Data.
pub fn read_header(r: impl Read) -> Result<Header, DicomError> {
    let mut r = BufReader::new(r);
    let mut pre = [0u8; PREAMBLE + 4];
    r.read_exact(&mut pre).map_err(|_| DicomError::NotDicom)?;
    if !is_dicom(&pre) {
        return Err(DicomError::NotDicom);
    }
    let mut h = Header::default();
    let mut p = Parser {
        r: &mut r,
        explicit: true,
    };
    // The meta group is always explicit VR little endian.
    let first_data = p.read_group(&mut h.values, Some(0x0002))?;
    let ts = h.transfer_syntax().unwrap_or(EXPLICIT_LE).to_string();
    match ts.as_str() {
        IMPLICIT_LE => p.explicit = false,
        DEFLATED_LE => return Err(DicomError::Unsupported("deflated data set".into())),
        EXPLICIT_BE => return Err(DicomError::Unsupported("big-endian data set".into())),
        _ => p.explicit = true,
    }
    if let Some(tag) = first_data {
        p.read_element_body(tag, &mut h.values, true)?;
        p.read_group(&mut h.values, None)?;
    }
    Ok(h)
}

struct Parser<'a, R: Read> {
    r: &'a mut R,
    explicit: bool,
}

impl<R: Read> Parser<'_, R> {
    fn u16(&mut self) -> io::Result<u16> {
        let mut b = [0u8; 2];
        self.r.read_exact(&mut b)?;
        Ok(u16::from_le_bytes(b))
    }

    fn u32(&mut self) -> io::Result<u32> {
        let mut b = [0u8; 4];
        self.r.read_exact(&mut b)?;
        Ok(u32::from_le_bytes(b))
    }

    /// The next tag, or `None` at the end of input.
    fn tag(&mut self) -> Result<Option<u32>, DicomError> {
        let mut b = [0u8; 4];
        let mut got = 0;
        while got < 4 {
            let n = self.r.read(&mut b[got..])?;
            if n == 0 {
                return if got == 0 {
                    Ok(None)
                } else {
                    Err(DicomError::Malformed("truncated tag".into()))
                };
            }
            got += n;
        }
        let group = u16::from_le_bytes([b[0], b[1]]) as u32;
        let elem = u16::from_le_bytes([b[2], b[3]]) as u32;
        Ok(Some((group << 16) | elem))
    }

    fn skip(&mut self, n: u64) -> io::Result<()> {
        let copied = io::copy(&mut self.r.by_ref().take(n), &mut io::sink())?;
        if copied < n {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(())
    }

    /// Read elements until the group changes away from `only` (returning
    /// the first tag outside it, already consumed), Pixel Data, or the end.
    fn read_group(
        &mut self,
        out: &mut BTreeMap<u32, String>,
        only: Option<u32>,
    ) -> Result<Option<u32>, DicomError> {
        loop {
            let Some(tag) = self.tag()? else {
                return Ok(None);
            };
            if let Some(g) = only {
                if tag >> 16 != g {
                    return Ok(Some(tag));
                }
            }
            if tag == PIXEL_DATA {
                return Ok(None);
            }
            // Explicit meta group even when the data set is implicit.
            let explicit = self.explicit || tag >> 16 == 0x0002;
            let saved = self.explicit;
            self.explicit = explicit;
            let r = self.read_element_body(tag, out, true);
            self.explicit = saved;
            r?;
        }
    }

    /// The VR and length after a tag, then the value: kept when it is one
    /// of ours and `keep`, skipped otherwise, recursed into for sequences.
    fn read_element_body(
        &mut self,
        tag: u32,
        out: &mut BTreeMap<u32, String>,
        keep: bool,
    ) -> Result<(), DicomError> {
        if tag == PIXEL_DATA {
            return Ok(());
        }
        let (vr, len) = if self.explicit {
            let mut vr = [0u8; 2];
            self.r.read_exact(&mut vr)?;
            let long = matches!(
                &vr,
                b"OB"
                    | b"OD"
                    | b"OF"
                    | b"OL"
                    | b"OV"
                    | b"OW"
                    | b"SQ"
                    | b"SV"
                    | b"UC"
                    | b"UN"
                    | b"UR"
                    | b"UT"
                    | b"UV"
            );
            let len = if long {
                self.u16()?;
                self.u32()?
            } else {
                self.u16()? as u32
            };
            (Some(vr), len)
        } else {
            (None, self.u32()?)
        };
        let is_seq = vr.as_ref().is_some_and(|v| v == b"SQ") || (vr.is_none() && len == UNDEFINED);
        if is_seq || len == UNDEFINED {
            return self.skip_sequence(len);
        }
        if keep && KEPT.contains(&tag) && len <= MAX_VALUE {
            let mut buf = vec![0u8; len as usize];
            self.r.read_exact(&mut buf)?;
            let s = String::from_utf8_lossy(&buf)
                .trim_end_matches(['\0', ' '])
                .trim_start()
                .to_string();
            out.insert(tag, s);
            return Ok(());
        }
        self.skip(len as u64)?;
        Ok(())
    }

    fn skip_sequence(&mut self, len: u32) -> Result<(), DicomError> {
        if len != UNDEFINED {
            return Ok(self.skip(len as u64)?);
        }
        loop {
            let Some(tag) = self.tag()? else {
                return Err(DicomError::Malformed("a sequence never ends".into()));
            };
            let item_len = self.u32()?;
            match tag {
                SEQ_END => return Ok(()),
                ITEM if item_len == UNDEFINED => self.skip_item_undefined()?,
                ITEM => self.skip(item_len as u64)?,
                other => {
                    return Err(DicomError::Malformed(format!(
                        "tag {other:08X} inside a sequence"
                    )))
                }
            }
        }
    }

    fn skip_item_undefined(&mut self) -> Result<(), DicomError> {
        let mut ignore = BTreeMap::new();
        loop {
            let Some(tag) = self.tag()? else {
                return Err(DicomError::Malformed("an item never ends".into()));
            };
            if tag == ITEM_END {
                self.u32()?;
                return Ok(());
            }
            self.read_element_body(tag, &mut ignore, false)?;
        }
    }
}

/// One record of a DICOMDIR that names a file.
#[derive(Debug, Clone, PartialEq)]
pub struct DicomDirEntry {
    /// Relative to the DICOMDIR's folder.
    pub path: PathBuf,
    pub study_uid: Option<String>,
    pub series_uid: Option<String>,
    pub sop_uid: Option<String>,
}

/// The files a DICOMDIR lists, with the study and series records above
/// each.
pub fn read_dicomdir(path: &Path) -> Result<Vec<DicomDirEntry>, DicomError> {
    let bytes = std::fs::read(path)?;
    if !is_dicom(&bytes) {
        return Err(DicomError::NotDicom);
    }
    let mut d = Tree {
        b: &bytes,
        pos: PREAMBLE + 4,
        explicit: true,
    };
    let top = d.elements(usize::MAX, true)?;
    let mut out = Vec::new();
    let (mut study, mut series) = (None, None);
    let Some(Node::Seq(records)) = top.get(&tags::DIR_RECORDS) else {
        return Ok(out);
    };
    for rec in records {
        let text = |t: u32| match rec.get(&t) {
            Some(Node::Text(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        };
        match text(tags::DIR_RECORD_TYPE).as_deref() {
            Some("STUDY") => {
                study = text(tags::STUDY_UID);
                series = None;
            }
            Some("SERIES") => series = text(tags::SERIES_UID),
            _ => {}
        }
        if let Some(file_id) = text(tags::REFERENCED_FILE_ID) {
            let rel: PathBuf = file_id
                .split('\\')
                .filter(|c| !c.is_empty() && *c != ".." && *c != ".")
                .collect();
            out.push(DicomDirEntry {
                path: rel,
                study_uid: study.clone(),
                series_uid: series.clone(),
                sop_uid: text(tags::REFERENCED_SOP_INSTANCE),
            });
        }
    }
    Ok(out)
}

enum Node {
    Text(String),
    Seq(Vec<BTreeMap<u32, Node>>),
}

/// An in-memory element tree, for a DICOMDIR (small, and all sequences).
struct Tree<'a> {
    b: &'a [u8],
    pos: usize,
    explicit: bool,
}

impl Tree<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], DicomError> {
        if self.pos + n > self.b.len() {
            return Err(DicomError::Malformed(
                "the DICOMDIR ends inside an element".into(),
            ));
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16, DicomError> {
        let s = self.take(2)?;
        Ok(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Result<u32, DicomError> {
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn tag(&mut self) -> Result<u32, DicomError> {
        let g = self.u16()? as u32;
        let e = self.u16()? as u32;
        Ok((g << 16) | e)
    }

    /// Elements until `end` bytes or an item delimiter.
    fn elements(&mut self, end: usize, top: bool) -> Result<BTreeMap<u32, Node>, DicomError> {
        let mut out = BTreeMap::new();
        while self.pos < end.min(self.b.len()) {
            let tag = self.tag()?;
            if tag == ITEM_END {
                self.u32()?;
                break;
            }
            if top && tag >> 16 != 0x0002 && out.contains_key(&tags::TRANSFER_SYNTAX) {
                if let Some(Node::Text(ts)) = out.get(&tags::TRANSFER_SYNTAX) {
                    self.explicit = ts != IMPLICIT_LE;
                }
            }
            let explicit = self.explicit || tag >> 16 == 0x0002;
            let (vr, len) = if explicit {
                let vr = [self.take(1)?[0], self.take(1)?[0]];
                let long = matches!(
                    &vr,
                    b"OB"
                        | b"OD"
                        | b"OF"
                        | b"OL"
                        | b"OV"
                        | b"OW"
                        | b"SQ"
                        | b"SV"
                        | b"UC"
                        | b"UN"
                        | b"UR"
                        | b"UT"
                        | b"UV"
                );
                let len = if long {
                    self.u16()?;
                    self.u32()?
                } else {
                    self.u16()? as u32
                };
                (Some(vr), len)
            } else {
                (None, self.u32()?)
            };
            let is_seq =
                vr.as_ref().is_some_and(|v| v == b"SQ") || (vr.is_none() && len == UNDEFINED);
            if is_seq {
                out.insert(tag, Node::Seq(self.items(len)?));
            } else if len == UNDEFINED {
                return Err(DicomError::Unsupported(
                    "an undefined-length value in a DICOMDIR".into(),
                ));
            } else {
                let v = self.take(len as usize)?;
                let s = String::from_utf8_lossy(v)
                    .trim_end_matches(['\0', ' '])
                    .trim_start()
                    .to_string();
                out.insert(tag, Node::Text(s));
            }
        }
        Ok(out)
    }

    fn items(&mut self, len: u32) -> Result<Vec<BTreeMap<u32, Node>>, DicomError> {
        let end = if len == UNDEFINED {
            usize::MAX
        } else {
            self.pos + len as usize
        };
        let mut items = Vec::new();
        while self.pos < end.min(self.b.len()) {
            let tag = self.tag()?;
            let item_len = self.u32()?;
            match tag {
                SEQ_END => break,
                ITEM => {
                    let item_end = if item_len == UNDEFINED {
                        usize::MAX
                    } else {
                        self.pos + item_len as usize
                    };
                    items.push(self.elements(item_end, false)?);
                }
                other => {
                    return Err(DicomError::Malformed(format!(
                        "tag {other:08X} inside a sequence"
                    )))
                }
            }
        }
        Ok(items)
    }
}

/// A study's series and instance counts, from its instances' headers.
#[derive(Debug, Clone, Default)]
pub struct StudySummary {
    pub study_uid: String,
    pub modalities: Vec<String>,
    pub series: Vec<SeriesSummary>,
    pub instances: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SeriesSummary {
    pub uid: String,
    pub number: Option<i64>,
    pub modality: Option<String>,
    pub description: Option<String>,
    pub instances: usize,
}

impl StudySummary {
    /// One summary per study UID among `headers`.
    pub fn from_headers(headers: &[Header]) -> Vec<StudySummary> {
        let mut by_study: BTreeMap<String, Vec<&Header>> = BTreeMap::new();
        for h in headers {
            if let Some(s) = h.study_uid() {
                by_study.entry(s.to_string()).or_default().push(h);
            }
        }
        by_study
            .into_iter()
            .map(|(study_uid, hs)| {
                let mut series: BTreeMap<String, SeriesSummary> = BTreeMap::new();
                for h in &hs {
                    let uid = h.series_uid().unwrap_or("unknown").to_string();
                    let s = series.entry(uid.clone()).or_insert_with(|| SeriesSummary {
                        uid,
                        number: h.series_number(),
                        modality: h.modality().map(str::to_string),
                        description: h.series_description().map(str::to_string),
                        instances: 0,
                    });
                    s.instances += 1;
                }
                let mut modalities: Vec<String> = hs
                    .iter()
                    .filter_map(|h| h.modality().map(str::to_string))
                    .collect();
                modalities.sort();
                modalities.dedup();
                StudySummary {
                    study_uid,
                    modalities,
                    instances: hs.len(),
                    series: series.into_values().collect(),
                }
            })
            .collect()
    }
}

/// Synthetic DICOM for fixtures and dogfooding: Part 10 files in explicit
/// VR little endian with a tiny pixel matrix, and a DICOMDIR. Every UID is
/// under `2.25.0`, a root no real equipment uses.
pub mod synthetic {
    use super::*;

    pub struct SyntheticStudy {
        modality: String,
        series: usize,
        instances: usize,
        seed: u32,
        study_date: String,
        body_part: String,
        institution: String,
        description: String,
        patient_name: String,
        patient_birth_date: String,
        accession: String,
    }

    impl SyntheticStudy {
        pub fn new(modality: &str) -> Self {
            SyntheticStudy {
                modality: modality.into(),
                series: 1,
                instances: 3,
                seed: 1,
                study_date: "20260304".into(),
                body_part: "ABDOMEN".into(),
                institution: "Example Imaging".into(),
                description: "Synthetic study".into(),
                patient_name: "Testperson^Robin".into(),
                patient_birth_date: "20200202".into(),
                accession: "ACC000001".into(),
            }
        }
        pub fn series(mut self, n: usize) -> Self {
            self.series = n;
            self
        }
        pub fn instances(mut self, n: usize) -> Self {
            self.instances = n;
            self
        }
        /// Distinguishes studies: the UIDs derive from it.
        pub fn seed(mut self, n: u32) -> Self {
            self.seed = n;
            self
        }
        /// `YYYYMMDD`.
        pub fn study_date(mut self, d: &str) -> Self {
            self.study_date = d.into();
            self
        }
        pub fn body_part(mut self, b: &str) -> Self {
            self.body_part = b.into();
            self
        }
        pub fn institution(mut self, i: &str) -> Self {
            self.institution = i.into();
            self
        }
        /// `Family^Given`.
        pub fn patient_name(mut self, n: &str) -> Self {
            self.patient_name = n.into();
            self
        }
        /// `YYYYMMDD`.
        pub fn patient_birth_date(mut self, d: &str) -> Self {
            self.patient_birth_date = d.into();
            self
        }

        pub fn study_uid(&self) -> String {
            format!("2.25.0.{}", self.seed)
        }

        /// Write `DICOMDIR` and `IMAGES/S<n>/I<m>` under `dir`; returns the
        /// instance files.
        pub fn write_to(&self, dir: &Path) -> io::Result<Vec<PathBuf>> {
            let mut files = Vec::new();
            let mut records = Vec::new();
            records.push(dir_record("PATIENT", &[]));
            records.push(dir_record(
                "STUDY",
                &[(tags::STUDY_UID, "UI", self.study_uid())],
            ));
            for s in 1..=self.series {
                let series_uid = format!("{}.{s}", self.study_uid());
                records.push(dir_record(
                    "SERIES",
                    &[(tags::SERIES_UID, "UI", series_uid.clone())],
                ));
                for i in 1..=self.instances {
                    let sop = format!("{series_uid}.{i}");
                    let rel = format!("IMAGES/S{s}/I{i}");
                    let path = dir.join(&rel);
                    std::fs::create_dir_all(path.parent().unwrap())?;
                    std::fs::write(&path, self.instance(&series_uid, s, &sop, i))?;
                    files.push(path);
                    records.push(dir_record(
                        "IMAGE",
                        &[
                            (tags::REFERENCED_FILE_ID, "CS", rel.replace('/', "\\")),
                            (tags::REFERENCED_SOP_INSTANCE, "UI", sop.clone()),
                        ],
                    ));
                }
            }
            std::fs::write(dir.join("DICOMDIR"), dicomdir(&records))?;
            Ok(files)
        }

        fn instance(&self, series_uid: &str, s: usize, sop: &str, i: usize) -> Vec<u8> {
            let sop_class = "1.2.840.10008.5.1.4.1.1.6.1";
            let mut data = Vec::new();
            for (tag, vr, v) in [
                (0x0008_0005u32, "CS", "ISO_IR 192".to_string()),
                (tags::SOP_CLASS, "UI", sop_class.to_string()),
                (tags::SOP_INSTANCE, "UI", sop.to_string()),
                (tags::STUDY_DATE, "DA", self.study_date.clone()),
                (tags::STUDY_TIME, "TM", "101500".into()),
                (tags::ACCESSION, "SH", self.accession.clone()),
                (tags::MODALITY, "CS", self.modality.clone()),
                (tags::INSTITUTION, "LO", self.institution.clone()),
                (tags::REFERRING, "PN", "Clinician^Example".into()),
                (tags::STUDY_DESCRIPTION, "LO", self.description.clone()),
                (tags::SERIES_DESCRIPTION, "LO", format!("Series {s}")),
                (tags::PATIENT_NAME, "PN", self.patient_name.clone()),
                (tags::PATIENT_ID, "LO", "PID000001".into()),
                (
                    tags::PATIENT_BIRTH_DATE,
                    "DA",
                    self.patient_birth_date.clone(),
                ),
                (tags::BODY_PART, "CS", self.body_part.clone()),
                (tags::STUDY_UID, "UI", self.study_uid()),
                (tags::SERIES_UID, "UI", series_uid.to_string()),
                (tags::SERIES_NUMBER, "IS", s.to_string()),
                (tags::INSTANCE_NUMBER, "IS", i.to_string()),
            ] {
                element(&mut data, tag, vr, v.as_bytes());
            }
            // An undefined-length sequence the reader must skip.
            data.extend_from_slice(&[0x08, 0x00, 0x15, 0x11]);
            data.extend_from_slice(b"SQ\0\0");
            data.extend_from_slice(&UNDEFINED.to_le_bytes());
            data.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
            data.extend_from_slice(&UNDEFINED.to_le_bytes());
            element(&mut data, 0x0008_1150, "UI", sop_class.as_bytes());
            data.extend_from_slice(&[0xFE, 0xFF, 0x0D, 0xE0, 0, 0, 0, 0]);
            data.extend_from_slice(&[0xFE, 0xFF, 0xDD, 0xE0, 0, 0, 0, 0]);
            for (tag, v) in [
                (0x0028_0010u32, 8u16),
                (0x0028_0011, 8),
                (0x0028_0100, 8),
                (0x0028_0101, 8),
                (0x0028_0102, 7),
            ] {
                element(&mut data, tag, "US", &v.to_le_bytes());
            }
            let pixels: Vec<u8> = (0..64)
                .map(|p| ((p * 4 + i * 7 + s * 13) % 256) as u8)
                .collect();
            element(&mut data, PIXEL_DATA, "OW", &pixels);
            part10(sop_class, sop, &data)
        }
    }

    fn dir_record(kind: &str, extra: &[(u32, &str, String)]) -> Vec<u8> {
        let mut item = Vec::new();
        element(&mut item, 0x0004_1400, "UL", &0u32.to_le_bytes());
        element(&mut item, 0x0004_1410, "US", &0xFFFFu16.to_le_bytes());
        element(&mut item, 0x0004_1420, "UL", &0u32.to_le_bytes());
        element(&mut item, tags::DIR_RECORD_TYPE, "CS", kind.as_bytes());
        for (tag, vr, v) in extra {
            element(&mut item, *tag, vr, v.as_bytes());
        }
        item
    }

    fn dicomdir(records: &[Vec<u8>]) -> Vec<u8> {
        let mut seq = Vec::new();
        for r in records {
            seq.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
            seq.extend_from_slice(&(r.len() as u32).to_le_bytes());
            seq.extend_from_slice(r);
        }
        let mut data = Vec::new();
        element(&mut data, 0x0004_1130, "CS", b"SYNTHETIC");
        data.extend_from_slice(&[0x04, 0x00, 0x20, 0x12]);
        data.extend_from_slice(b"SQ\0\0");
        data.extend_from_slice(&(seq.len() as u32).to_le_bytes());
        data.extend_from_slice(&seq);
        part10("1.2.840.10008.1.3.10", "2.25.0.9", &data)
    }

    fn part10(sop_class: &str, sop: &str, data: &[u8]) -> Vec<u8> {
        let mut meta = Vec::new();
        element(&mut meta, 0x0002_0001, "OB", &[0, 1]);
        element(&mut meta, tags::MEDIA_SOP_CLASS, "UI", sop_class.as_bytes());
        element(&mut meta, tags::MEDIA_SOP_INSTANCE, "UI", sop.as_bytes());
        element(
            &mut meta,
            tags::TRANSFER_SYNTAX,
            "UI",
            EXPLICIT_LE.as_bytes(),
        );
        element(&mut meta, 0x0002_0012, "UI", b"2.25.0.1000");
        let mut out = vec![0u8; PREAMBLE];
        out.extend_from_slice(b"DICM");
        element(
            &mut out,
            0x0002_0000,
            "UL",
            &(meta.len() as u32).to_le_bytes(),
        );
        out.extend_from_slice(&meta);
        out.extend_from_slice(data);
        out
    }

    /// One explicit-VR little-endian element, value padded to even length.
    pub fn element(out: &mut Vec<u8>, tag: u32, vr: &str, value: &[u8]) {
        let mut v = value.to_vec();
        if v.len() % 2 == 1 {
            v.push(if vr == "UI" || vr == "OB" { 0 } else { b' ' });
        }
        out.extend_from_slice(&((tag >> 16) as u16).to_le_bytes());
        out.extend_from_slice(&((tag & 0xFFFF) as u16).to_le_bytes());
        out.extend_from_slice(vr.as_bytes());
        let long = matches!(
            vr,
            "OB" | "OD" | "OF" | "OL" | "OW" | "SQ" | "UC" | "UN" | "UR" | "UT"
        );
        if long {
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(&(v.len() as u32).to_le_bytes());
        } else {
            out.extend_from_slice(&(v.len() as u16).to_le_bytes());
        }
        out.extend_from_slice(&v);
    }
}

#[cfg(test)]
mod tests {
    use super::synthetic::SyntheticStudy;
    use super::*;

    #[test]
    fn synthetic_instances_read_back_and_debug_hides_the_patient() {
        let dir = crate::testutil::temp_dir("dicom");
        let files = SyntheticStudy::new("US")
            .series(2)
            .instances(2)
            .seed(7)
            .write_to(&dir)
            .unwrap();
        assert_eq!(files.len(), 4);
        let h = read_header(std::fs::File::open(&files[3]).unwrap()).unwrap();
        assert_eq!(h.study_uid(), Some("2.25.0.7"));
        assert_eq!(h.series_uid(), Some("2.25.0.7.2"));
        assert_eq!(h.sop_instance_uid(), Some("2.25.0.7.2.2"));
        assert_eq!(h.modality(), Some("US"));
        assert_eq!(h.study_date().as_deref(), Some("2026-03-04"));
        assert_eq!(h.series_number(), Some(2));
        assert_eq!(h.patient_name().as_deref(), Some("Robin Testperson"));
        assert_eq!(h.patient_birth_date().as_deref(), Some("2020-02-02"));
        assert_eq!(h.institution(), Some("Example Imaging"));
        assert!(!format!("{h:?}").contains("Testperson"));
        assert!(is_dicom_file(&files[0]));
        assert!(!is_dicom_file(&dir.join("missing")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_dicomdir_lists_files_with_their_study_and_series() {
        let dir = crate::testutil::temp_dir("dicomdir");
        SyntheticStudy::new("CT")
            .series(2)
            .instances(1)
            .seed(3)
            .write_to(&dir)
            .unwrap();
        let entries = read_dicomdir(&dir.join("DICOMDIR")).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].path, PathBuf::from("IMAGES/S2/I1"));
        assert_eq!(entries[1].study_uid.as_deref(), Some("2.25.0.3"));
        assert_eq!(entries[1].series_uid.as_deref(), Some("2.25.0.3.2"));
        assert_eq!(entries[1].sop_uid.as_deref(), Some("2.25.0.3.2.1"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn summaries_count_series_and_instances() {
        let dir = crate::testutil::temp_dir("dicom-sum");
        let files = SyntheticStudy::new("US")
            .series(2)
            .instances(3)
            .write_to(&dir)
            .unwrap();
        let headers: Vec<Header> = files
            .iter()
            .map(|f| read_header(std::fs::File::open(f).unwrap()).unwrap())
            .collect();
        let s = StudySummary::from_headers(&headers);
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].series.len(), s[0].instances), (2, 6));
        assert_eq!(s[0].modalities, ["US"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn not_dicom_and_truncated_files_are_refused() {
        assert!(matches!(
            read_header(&b"%PDF-1.4"[..]),
            Err(DicomError::NotDicom)
        ));
        let mut f = vec![0u8; 128];
        f.extend_from_slice(b"DICM");
        f.extend_from_slice(&[0x02, 0x00, 0x10, 0x00, b'U', b'I', 40, 0, b'1']);
        assert!(matches!(read_header(&f[..]), Err(DicomError::Malformed(_))));
        assert_eq!(
            person_name("Family^Given^Middle^Dr"),
            "Dr Given Middle Family"
        );
    }
}
