//! The records folder's data model (docs/data-contract.md). Field names
//! follow FHIR element names where a FHIR shape exists; codes keep
//! `{system, code, display}`. Everything here is plain JSON in the folder.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const HOUSEHOLD_SCHEMA: &str = "medrec-household/v1";
pub const INDEX_SCHEMA: &str = "medrec-index/v1";
pub const TEXT_SCHEMA: &str = "medrec-text/v1";
pub const REVIEW_SCHEMA: &str = "medrec-review/v1";
pub const STUDY_SCHEMA: &str = "medrec-study/v1";
/// The layout version `household.json` declares. A newer one is refused.
pub const FORMAT: u32 = 1;

fn is_false(b: &bool) -> bool {
    !*b
}

/// `household.json`: people, organizations and sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Household {
    pub schema: String,
    pub format: u32,
    /// Random id of this records folder, so a mount and a remote can be
    /// told to be the same folder.
    pub folder_id: String,
    pub rev: u64,
    pub created_at: String,
    #[serde(default)]
    pub people: Vec<Person>,
    #[serde(default)]
    pub organizations: Vec<Organization>,
    #[serde(default)]
    pub sources: Vec<Source>,
}

impl Household {
    pub fn new(folder_id: &str) -> Self {
        Household {
            schema: HOUSEHOLD_SCHEMA.into(),
            format: FORMAT,
            folder_id: folder_id.into(),
            rev: 0,
            created_at: now_rfc3339(),
            people: Vec::new(),
            organizations: Vec::new(),
            sources: Vec::new(),
        }
    }

    pub fn person(&self, id: &str) -> Option<&Person> {
        self.people.iter().find(|p| p.id == id)
    }

    pub fn person_mut(&mut self, id: &str) -> Option<&mut Person> {
        self.people.iter_mut().find(|p| p.id == id)
    }

    pub fn name_of(&self, id: &str) -> String {
        match self.person(id) {
            Some(p) => p.name.clone(),
            None => id.into(),
        }
    }
}

/// A household member. `id` is opaque and stable; `folder` is the
/// person's folder in the records folder, fixed when they were added.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub folder: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birth_date: Option<String>,
    /// Adults can be guardians.
    #[serde(default, skip_serializing_if = "is_false")]
    pub adult: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guardians: Vec<String>,
    /// Other spellings of the name, for classifying documents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Identifiers per source: MRNs, a portal's patient id, a member id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identifiers: Vec<Identifier>,
    #[serde(default, skip_serializing_if = "Links::is_empty")]
    pub links: Links,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identifier {
    /// Where the identifier is valid: `mychart:<org>`, an organization name.
    pub source: String,
    pub value: String,
    /// `mrn`, `patient`, `member`, … (free text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// Links to Huishouden (PLAN §9). Fields only: medrec writes nothing to
/// Huishouden in phase 1.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Links {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub huishouden_health_person: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baby_profile: Option<String>,
}

impl Links {
    pub fn is_empty(&self) -> bool {
        self.huishouden_health_person.is_none() && self.baby_profile.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fhir_base: Option<String>,
}

/// Where a person's records come from (FHIR `Organization` + `Endpoint`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    /// `fhir`, `scan`, `drive`, `email`, `manual`, `huishouden`.
    pub kind: String,
    pub person: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fhir_base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patient_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pull: Option<String>,
}

/// `_review/review.json`: what intake could not decide.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewQueue {
    pub schema: String,
    pub rev: u64,
    #[serde(default)]
    pub items: Vec<ReviewItem>,
}

impl Default for ReviewQueue {
    fn default() -> Self {
        ReviewQueue {
            schema: REVIEW_SCHEMA.into(),
            rev: 0,
            items: Vec::new(),
        }
    }
}

/// A document intake kept but could not file: its person, date or type was
/// not certain, or what organize-scans said disagreed with what medrec
/// found. The file sits in `_review/` until `medrec review` files it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewItem {
    pub id: String,
    /// Path in the records folder (`_review/<name>`).
    pub path: String,
    pub sha256: String,
    pub md5: String,
    pub size: u64,
    pub media_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    pub original_name: String,
    pub received_at: String,
    pub origin: Origin,
    /// Why it waits: `unsure` fields, or a disagreement.
    pub reasons: Vec<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_info: Option<TextInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub minhash: Vec<u32>,
    pub classification: Value,
    /// Who classified it before medrec, when someone did
    /// (`organize-scans`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classified_by: Option<String>,
}

/// How a document reached the records folder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Origin {
    /// `file`, `inbox`, `drive`, `fhir`, `review`.
    pub kind: String,
    /// The file name as received (never a full local path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `copied` or `moved`.
    pub action: String,
}

/// Extracted text, as stored beside the index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextInfo {
    pub chars: usize,
    /// `pdftotext 26.10.0`, `tesseract 5.5.4`, `xml`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr_pages: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `<Person>/index.json`: the normalized index of the files and the raw
/// records beside it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonIndex {
    pub schema: String,
    pub person: String,
    pub rev: u64,
    #[serde(default)]
    pub documents: Vec<Document>,
    #[serde(default)]
    pub records: Vec<Record>,
    #[serde(default)]
    pub studies: Vec<ImagingStudy>,
    #[serde(default)]
    pub imports: Vec<ImportRun>,
}

impl PersonIndex {
    pub fn new(person: &str) -> Self {
        PersonIndex {
            schema: INDEX_SCHEMA.into(),
            person: person.into(),
            rev: 0,
            documents: Vec::new(),
            records: Vec::new(),
            studies: Vec::new(),
            imports: Vec::new(),
        }
    }
}

/// `<Person>/text.json`: extracted text per document, apart from the index
/// so listing documents never downloads every page of text. Derived.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonText {
    pub schema: String,
    pub person: String,
    #[serde(default)]
    pub texts: BTreeMap<String, String>,
}

impl PersonText {
    pub fn new(person: &str) -> Self {
        PersonText {
            schema: TEXT_SCHEMA.into(),
            person: person.into(),
            texts: BTreeMap::new(),
        }
    }
}

/// A filed file (FHIR `DocumentReference` + `Binary`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    /// Path in the records folder (`<Person>/Documents/<YYYY>/<name>`).
    pub path: String,
    /// SHA-256 of the bytes: the document's identity.
    pub sha256: String,
    /// MD5, the hash Drive keeps, so `verify` checks without downloading.
    pub md5: String,
    pub size: u64,
    pub media_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// House vocabulary (`lab_report`, `eob`, …), see `doctype`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_type: Option<String>,
    /// LOINC document code where one is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loinc: Option<String>,
    /// Date of service, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    pub filed_at: String,
    pub origin: Origin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<TextInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub near_duplicates: Vec<NearDuplicate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub minhash: Vec<u32>,
    /// What intake detected and how each field was decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<Value>,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NearDuplicate {
    pub document: String,
    /// Estimated Jaccard similarity of the normalized text, 0..1.
    pub similarity: f32,
}

/// A normalized clinical fact (one row of the index), derived from raw
/// resources, which stay beside it verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    /// `encounter`, `result`, `report`, `condition`, `medication`,
    /// `allergy`, `immunization`, `procedure`, `coverage`, `claim`, `bill`.
    pub kind: String,
    /// The merge key: kind + code + date, so the same fact from two sources
    /// is one record with two provenances.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Code>,
    pub display: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Kind-specific fields, named as FHIR names them.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub fields: serde_json::Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<String>,
    pub provenance: Vec<Provenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Code {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

/// A dated note, kept in `<Person>/notes.md` (that file is the source).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub date: String,
    /// Who wrote it (the `me` setting's person), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Document or record ids the note is about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub about: Vec<String>,
}

/// One `medrec import` run. Its resources are under `source_dir`, one
/// verbatim file per resource.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRun {
    pub id: String,
    /// `<Person>/Records/<source>/`.
    pub source_dir: String,
    /// SHA-256 of the whole stream as received.
    pub stream_sha256: String,
    pub stream_bytes: u64,
    pub cli: String,
    pub cli_version: String,
    pub organization: String,
    pub patient_id: String,
    pub pulled_at: String,
    pub imported_at: String,
    pub counts: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<Value>,
    pub records_added: usize,
    pub records_updated: usize,
    pub records_unchanged: usize,
    pub resources_written: usize,
    pub resources_unchanged: usize,
    pub documents_added: usize,
}

/// An imaging study (FHIR `ImagingStudy`), its files kept as received
/// under `path`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagingStudy {
    pub id: String,
    pub study_uid: String,
    /// `<Person>/Imaging/<date>_<modality>_<body part>_<facility>/`.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modalities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_part: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facility: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accession: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referring: Option<String>,
    pub series: Vec<ImagingSeries>,
    pub instances: usize,
    pub size: u64,
    /// The radiology report: a document or record id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    pub filed_at: String,
    pub provenance: Vec<Provenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagingSeries {
    pub uid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// SOPInstanceUID → file name in the study folder, so a second copy of
    /// the study adds only what is new.
    pub instances: BTreeMap<String, String>,
}

/// Where a record came from (`provenance/v1` fields of pk-med-records).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub source: ProvenanceSource,
    /// `fhir-api`, `portal`, `scan`, `file`, `media`, `manual`, `huishouden`.
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_resource: Option<SourceResource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    pub filed_at: String,
    /// The raw file in the records folder: the verbatim resource, or the
    /// document as received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_path: Option<String>,
    /// Who decided the person, date and type: `medrec` (its rules),
    /// `organize-scans`, `hand` (on the command line or in review).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classified_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<DerivedFrom>,
    /// The medrec version that filed it.
    pub filed_by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceSource {
    /// `fhir`, `portal`, `scan`, `drive`, `email`, `media`, `manual`,
    /// `huishouden`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fhir_base: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceResource {
    #[serde(rename = "type")]
    pub resource_type: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedFrom {
    pub document_id: String,
    pub rule_id: String,
}

pub fn filed_by() -> String {
    format!("medrec {}", env!("CARGO_PKG_VERSION"))
}

pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    pk_cli_core::dates::fmt_rfc3339(secs)
}

pub fn today() -> String {
    pk_cli_core::dates::fmt_iso(pk_cli_core::dates::today_local())
}
