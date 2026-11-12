//! Classifying a document from its text and file name: whom it is about,
//! its date of service, its type and the organization that produced it.
//! Each answer is a [`Proposal`] with a score, a confidence and the evidence
//! that produced it, so a person (or `medrec review`) can see why.
//!
//! Scoring, per candidate value: the heaviest piece of evidence, plus
//! [`AGREEMENT_BONUS`] when independent kinds of evidence agree (a name and
//! a birth date), plus 2 for each further hit (at most 6), capped at 99.
//! [`HIGH_AT`] and [`MEDIUM_AT`] turn the score into a confidence, and a
//! runner-up within [`AMBIGUITY_MARGIN`] of the leader caps it at medium.
//!
//! Evidence quotes the document's own words, which are personal data; it is
//! runtime output, kept inside the encrypted index and shown to the owner,
//! and identifiers (MRNs, member ids) are quoted with all but their last
//! two characters masked.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::model::{Organization, Person};

pub const HIGH_AT: u8 = 80;
pub const MEDIUM_AT: u8 = 50;
pub const AMBIGUITY_MARGIN: u8 = 15;
pub const AGREEMENT_BONUS: u8 = 15;
/// Alternatives shown beside the leader.
const ALTERNATIVES: usize = 3;
const ALTERNATIVE_MIN: u8 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
    /// Told on the command line or in review.
    Given,
}

impl Confidence {
    pub fn of(score: u8) -> Self {
        if score >= HIGH_AT {
            Confidence::High
        } else if score >= MEDIUM_AT {
            Confidence::Medium
        } else {
            Confidence::Low
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Low => "low",
            Confidence::Medium => "medium",
            Confidence::High => "high",
            Confidence::Given => "given",
        }
    }

    pub fn decided(self) -> bool {
        matches!(self, Confidence::High | Confidence::Given)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    /// A stable rule id (`patient_label`, `service_date_label`, …).
    pub rule: String,
    /// `text` or `filename`.
    pub origin: String,
    pub weight: u8,
    /// The words matched, as the document spells them (at most 80 chars).
    pub matched: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub value: String,
    pub score: u8,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Proposal {
    pub fn given(value: impl Into<String>) -> Self {
        Proposal {
            value: value.into(),
            score: 99,
            confidence: Confidence::Given,
            evidence: Vec::new(),
            note: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Field {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<Proposal>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<Proposal>,
}

impl Field {
    pub fn decided(&self) -> bool {
        self.proposal
            .as_ref()
            .is_some_and(|p| p.confidence.decided())
    }

    pub fn value(&self) -> Option<&str> {
        self.proposal.as_ref().map(|p| p.value.as_str())
    }

    pub fn set_given(&mut self, value: impl Into<String>) {
        self.proposal = Some(Proposal::given(value));
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Classification {
    pub person: Field,
    pub date: Field,
    pub doc_type: Field,
    pub organization: Field,
}

impl Classification {
    /// Person, date of service and type are each certain. The organization
    /// is recorded with whatever confidence it has; it never holds a
    /// document back.
    pub fn decided(&self) -> bool {
        self.person.decided() && self.date.decided() && self.doc_type.decided()
    }

    /// The fields that are not certain, by name.
    pub fn unsure(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if !self.person.decided() {
            v.push("person");
        }
        if !self.date.decided() {
            v.push("date");
        }
        if !self.doc_type.decided() {
            v.push("type");
        }
        v
    }
}

/// The house vocabulary of document types, with a LOINC document code where
/// one fits.
pub const DOC_TYPES: &[(&str, &str, Option<&str>)] = &[
    ("visit_summary", "Visit summary", Some("34133-9")),
    ("lab_report", "Lab report", Some("11502-2")),
    ("imaging_report", "Imaging report", Some("18748-4")),
    (
        "immunization_record",
        "Immunization record",
        Some("11369-6"),
    ),
    ("discharge_summary", "Discharge summary", Some("18842-5")),
    ("referral", "Referral", Some("57133-1")),
    ("prescription", "Prescription", None),
    ("letter", "Letter", None),
    ("bill", "Bill", None),
    ("eob", "Explanation of benefits", None),
    ("insurance_card", "Insurance card", None),
    ("other", "Other", None),
];

pub fn doc_type_label(code: &str) -> Option<&'static str> {
    DOC_TYPES
        .iter()
        .find(|(c, _, _)| *c == code)
        .map(|(_, l, _)| *l)
}

pub fn doc_type_loinc(code: &str) -> Option<&'static str> {
    DOC_TYPES
        .iter()
        .find(|(c, _, _)| *c == code)
        .and_then(|(_, _, l)| *l)
}

pub fn is_doc_type(code: &str) -> bool {
    DOC_TYPES.iter().any(|(c, _, _)| *c == code)
}

/// Phrases per document type: `(type, phrase, weight)`. A phrase in the
/// first lines (a title) weighs [`TITLE_BONUS`] more.
const TYPE_RULES: &[(&str, &str, u8)] = &[
    ("eob", "explanation of benefits", 92),
    ("eob", "this is not a bill", 80),
    ("eob", "you may owe", 55),
    ("eob", "amount plan paid", 70),
    ("eob", "plan paid", 50),
    ("bill", "amount due", 70),
    ("bill", "balance due", 70),
    ("bill", "please pay", 65),
    ("bill", "pay this amount", 70),
    ("bill", "statement date", 55),
    ("bill", "account number", 35),
    ("insurance_card", "rx bin", 85),
    ("insurance_card", "rxbin", 85),
    ("insurance_card", "rx pcn", 80),
    ("insurance_card", "rxpcn", 80),
    ("insurance_card", "rx grp", 70),
    ("insurance_card", "member id", 60),
    ("insurance_card", "group number", 45),
    ("insurance_card", "payer id", 55),
    ("lab_report", "lab report", 85),
    ("lab_report", "laboratory report", 88),
    ("lab_report", "reference range", 80),
    ("lab_report", "specimen", 60),
    ("lab_report", "laboratory", 55),
    ("lab_report", "collected", 35),
    ("lab_report", "lab results", 80),
    ("imaging_report", "radiology report", 90),
    ("imaging_report", "imaging report", 88),
    ("imaging_report", "impression", 50),
    ("imaging_report", "x ray", 60),
    ("imaging_report", "ultrasound", 60),
    ("imaging_report", "mri", 55),
    ("imaging_report", "ct scan", 60),
    ("immunization_record", "immunization record", 92),
    ("immunization_record", "vaccination record", 92),
    ("immunization_record", "immunization history", 90),
    ("immunization_record", "vaccine administration", 80),
    ("immunization_record", "immunization", 60),
    ("immunization_record", "vaccine", 55),
    ("discharge_summary", "discharge summary", 92),
    ("discharge_summary", "discharge instructions", 82),
    ("visit_summary", "after visit summary", 92),
    ("visit_summary", "visit summary", 85),
    ("visit_summary", "progress note", 80),
    ("visit_summary", "office visit", 60),
    ("visit_summary", "chief complaint", 60),
    ("visit_summary", "reason for visit", 60),
    ("referral", "referral", 72),
    ("referral", "referred to", 60),
    ("prescription", "prescription", 60),
    ("prescription", "sig", 45),
    ("prescription", "refills", 50),
    ("prescription", "dispense", 50),
    ("letter", "dear", 50),
    ("letter", "sincerely", 45),
];
const TITLE_BONUS: u8 = 5;
const TITLE_LINES: usize = 8;

/// Labels before a date that make it the date of service.
const SERVICE_LABELS: &[&str] = &[
    "date of service",
    "service date",
    "dates of service",
    "dos",
    "visit date",
    "date of visit",
    "encounter date",
    "appointment date",
    "appt date",
    "date collected",
    "collection date",
    "collected",
    "exam date",
    "study date",
    "procedure date",
    "date of procedure",
    "admission date",
    "admit date",
    "discharge date",
    "date administered",
    "administered",
    "date given",
    "vaccination date",
    "service from",
];
/// Labels that make a date something other than the date of service.
const OTHER_DATE_LABELS: &[&str] = &[
    "dob",
    "date of birth",
    "birth date",
    "birthdate",
    "born",
    "printed",
    "print date",
    "due date",
    "pay by",
    "payment due",
    "due by",
    "expires",
    "expiration",
    "exp",
    "next appointment",
    "follow up",
    "return",
];
const WEAK_LABELS: &[&str] = &[
    "date",
    "report date",
    "reported",
    "statement date",
    "issued",
];

const ORG_WORDS: &[&str] = &[
    "hospital",
    "medical center",
    "medical group",
    "clinic",
    "pediatrics",
    "pediatric",
    "health",
    "healthcare",
    "laboratory",
    "laboratories",
    "labs",
    "imaging",
    "radiology",
    "urgent care",
    "pharmacy",
    "physicians",
    "associates",
    "insurance",
    "health plan",
    "dental",
    "dentistry",
    "orthopedics",
    "family medicine",
];

pub struct Known<'a> {
    pub people: &'a [Person],
    pub organizations: &'a [Organization],
}

/// Classify a document. `today` bounds dates of service (`YYYY-MM-DD`).
pub fn classify(text: &str, filename: &str, known: &Known, today: &str) -> Classification {
    let doc = Squashed::new(text);
    let name = Squashed::new(&filename.replace(['_', '-', '.'], " "));
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    Classification {
        person: rank(person_hits(&doc, &name, &lines, known)),
        date: rank(date_hits(text, filename, known, today)),
        doc_type: rank(type_hits(&doc, &lines)),
        organization: rank(org_hits(&doc, &name, &lines, known)),
    }
}

/// One piece of evidence for one candidate value.
struct Hit {
    value: String,
    group: &'static str,
    ev: Evidence,
}

fn hit(
    value: &str,
    group: &'static str,
    rule: &str,
    origin: &str,
    weight: u8,
    matched: &str,
) -> Hit {
    Hit {
        value: value.to_string(),
        group,
        ev: Evidence {
            rule: rule.into(),
            origin: origin.into(),
            weight,
            matched: clip(matched),
        },
    }
}

fn rank(hits: Vec<Hit>) -> Field {
    let mut by_value: BTreeMap<String, Vec<Hit>> = BTreeMap::new();
    for h in hits {
        by_value.entry(h.value.clone()).or_default().push(h);
    }
    let mut proposals: Vec<Proposal> = by_value
        .into_iter()
        .filter(|(_, hs)| hs.iter().any(|h| h.ev.weight > 0))
        .map(|(value, mut hs)| {
            hs.sort_by_key(|h| std::cmp::Reverse(h.ev.weight));
            let mut seen = std::collections::HashSet::new();
            hs.retain(|h| seen.insert((h.ev.rule.clone(), h.ev.matched.to_lowercase())));
            let max = hs[0].ev.weight;
            let groups: std::collections::HashSet<&str> = hs.iter().map(|h| h.group).collect();
            let bonus = if groups.len() >= 2 {
                AGREEMENT_BONUS
            } else {
                0
            };
            let extra = ((hs.len() as u8).saturating_sub(1) * 2).min(6);
            let score = max.saturating_add(bonus).saturating_add(extra).min(99);
            Proposal {
                value,
                score,
                confidence: Confidence::of(score),
                evidence: hs.into_iter().take(4).map(|h| h.ev).collect(),
                note: None,
            }
        })
        .collect();
    proposals.sort_by_key(|p| std::cmp::Reverse(p.score));
    let mut field = Field::default();
    let mut iter = proposals.into_iter();
    if let Some(mut lead) = iter.next() {
        let rest: Vec<Proposal> = iter.collect();
        if let Some(second) = rest.first() {
            if second.score >= MEDIUM_AT && lead.score - second.score < AMBIGUITY_MARGIN {
                if lead.confidence == Confidence::High {
                    lead.confidence = Confidence::Medium;
                }
                lead.note = Some(format!(
                    "ambiguous: `{}` scores {} against {}",
                    second.value, second.score, lead.score
                ));
            }
        }
        field.alternatives = rest
            .into_iter()
            .filter(|p| p.score >= ALTERNATIVE_MIN)
            .take(ALTERNATIVES)
            .collect();
        field.proposal = Some(lead);
    }
    field
}

// ---- people ---------------------------------------------------------------

const PATIENT_LABELS: &[&str] = &[
    "patient",
    "patient name",
    "name",
    "pt",
    "member",
    "child",
    "client",
];

fn person_hits(doc: &Squashed, name: &Squashed, lines: &[&str], known: &Known) -> Vec<Hit> {
    let mut hits = Vec::new();
    for p in known.people {
        let mut spellings = vec![p.name.as_str()];
        spellings.extend(p.aliases.iter().map(String::as_str));
        for spelling in spellings {
            let tokens: Vec<String> = fold(spelling)
                .split_whitespace()
                .map(str::to_string)
                .collect();
            if tokens.is_empty() {
                continue;
            }
            let full = tokens.join(" ");
            if tokens.len() >= 2 {
                for (s, e) in doc.find(&full) {
                    hits.push(hit(
                        &p.id,
                        "name",
                        "full_name",
                        "text",
                        75,
                        doc.original(s, e),
                    ));
                }
                let last = tokens.last().unwrap();
                let reversed = format!("{last} {}", tokens[..tokens.len() - 1].join(" "));
                for (s, e) in doc.find(&reversed) {
                    hits.push(hit(
                        &p.id,
                        "name",
                        "full_name_reversed",
                        "text",
                        75,
                        doc.original(s, e),
                    ));
                }
                if !name.find(&full).is_empty() || !name.find(&reversed).is_empty() {
                    hits.push(hit(
                        &p.id,
                        "name",
                        "name_in_filename",
                        "filename",
                        70,
                        spelling,
                    ));
                }
                // "Patient: <name>" on one line.
                for line in lines {
                    let l = Squashed::new(line);
                    let named = !l.find(&full).is_empty() || !l.find(&reversed).is_empty();
                    if named && PATIENT_LABELS.iter().any(|lab| l.starts_with_label(lab)) {
                        hits.push(hit(&p.id, "name", "patient_label", "text", 90, line.trim()));
                    }
                }
                let first = &tokens[0];
                if first.chars().count() >= 3 {
                    for (s, e) in doc.find(first).into_iter().take(1) {
                        hits.push(hit(
                            &p.id,
                            "name",
                            "first_name",
                            "text",
                            40,
                            doc.original(s, e),
                        ));
                    }
                }
            } else if tokens[0].chars().count() >= 3 {
                for (s, e) in doc.find(&tokens[0]).into_iter().take(1) {
                    hits.push(hit(
                        &p.id,
                        "name",
                        "single_name",
                        "text",
                        55,
                        doc.original(s, e),
                    ));
                }
            }
        }
        if let Some(dob) = &p.birth_date {
            for (iso, matched, _) in all_dates(&doc.raw) {
                if &iso == dob {
                    hits.push(hit(&p.id, "birth_date", "birth_date", "text", 80, &matched));
                }
            }
        }
        for ident in &p.identifiers {
            let v = fold(&ident.value);
            if v.replace(' ', "").chars().count() >= 4 && !doc.find(&v).is_empty() {
                hits.push(hit(
                    &p.id,
                    "identifier",
                    "identifier",
                    "text",
                    95,
                    &mask(&ident.value),
                ));
            }
        }
    }
    hits
}

fn mask(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let keep = chars.len().min(2);
    let mut out: String = "•".repeat(chars.len() - keep);
    out.extend(&chars[chars.len() - keep..]);
    out
}

// ---- dates ----------------------------------------------------------------

fn date_hits(text: &str, filename: &str, known: &Known, today: &str) -> Vec<Hit> {
    let birth_dates: Vec<&str> = known
        .people
        .iter()
        .filter_map(|p| p.birth_date.as_deref())
        .collect();
    let mut hits = Vec::new();
    for line in text.lines() {
        for (iso, matched, start) in all_dates(line) {
            if iso.as_str() > today || iso.as_str() < "1900-01-01" {
                continue;
            }
            if birth_dates.contains(&iso.as_str()) {
                continue;
            }
            let before = fold(&line[..start]);
            let before = before.trim_end();
            let tail: String = before
                .chars()
                .rev()
                .take(40)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            let label_of = |labels: &'static [&'static str]| -> Option<&'static str> {
                labels
                    .iter()
                    .filter(|l| {
                        ends_with_word(&tail, l) || ends_with_word(&tail, &format!("{l} s"))
                    })
                    .max_by_key(|l| l.len())
                    .copied()
            };
            if label_of(OTHER_DATE_LABELS).is_some() {
                continue;
            }
            if let Some(label) = label_of(SERVICE_LABELS) {
                hits.push(hit(
                    &iso,
                    "label",
                    "service_date_label",
                    "text",
                    85,
                    &format!("{label} {matched}"),
                ));
            } else if let Some(label) = label_of(WEAK_LABELS) {
                hits.push(hit(
                    &iso,
                    "label",
                    "date_label",
                    "text",
                    55,
                    &format!("{label} {matched}"),
                ));
            } else {
                hits.push(hit(&iso, "bare", "date_in_text", "text", 40, &matched));
            }
        }
    }
    for (iso, matched, _) in all_dates(filename)
        .into_iter()
        .chain(compact_dates(filename))
    {
        if iso.as_str() <= today && !birth_dates.contains(&iso.as_str()) {
            hits.push(hit(
                &iso,
                "filename",
                "date_in_filename",
                "filename",
                60,
                &matched,
            ));
        }
    }
    hits
}

fn ends_with_word(hay: &str, needle: &str) -> bool {
    hay == needle || hay.ends_with(&format!(" {needle}"))
}

const MONTHS: &[&str] = &[
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

fn month_of(word: &str) -> Option<u32> {
    let w = word.to_ascii_lowercase();
    let w = w.trim_end_matches('.');
    if w.len() < 3 {
        return None;
    }
    let full = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    MONTHS
        .iter()
        .position(|m| w == *m || (w == "sept" && *m == "sep"))
        .or_else(|| full.iter().position(|m| w == *m))
        .map(|i| i as u32 + 1)
}

fn date_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?ix)
            \b(?P<iy>\d{4})-(?P<im>\d{1,2})-(?P<id>\d{1,2})\b
            | \b(?P<um>\d{1,2})[/-](?P<ud>\d{1,2})[/-](?P<uy>\d{4}|\d{2})\b
            | \b(?P<mn>[a-z]{3,9})\.?\s+(?P<md>\d{1,2})(?:st|nd|rd|th)?,?\s+(?P<my>\d{4})\b
            | \b(?P<dd>\d{1,2})\s+(?P<dm>[a-z]{3,9})\.?,?\s+(?P<dy>\d{4})\b
            ",
        )
        .expect("date regex")
    })
}

/// Every date in `s`: `(YYYY-MM-DD, as written, byte offset)`.
pub fn all_dates(s: &str) -> Vec<(String, String, usize)> {
    let mut out = Vec::new();
    for c in date_regex().captures_iter(s) {
        let m = c.get(0).unwrap();
        let num = |k: &str| c.name(k).and_then(|v| v.as_str().parse::<u32>().ok());
        let (y, mo, d) = if let (Some(y), Some(mo), Some(d)) = (num("iy"), num("im"), num("id")) {
            (y, mo, d)
        } else if let (Some(mo), Some(d), Some(y)) = (num("um"), num("ud"), c.name("uy")) {
            let y = if y.as_str().len() == 2 {
                let yy: u32 = y.as_str().parse().unwrap_or(0);
                if yy <= 69 {
                    2000 + yy
                } else {
                    1900 + yy
                }
            } else {
                y.as_str().parse().unwrap_or(0)
            };
            (y, mo, d)
        } else if let (Some(mn), Some(d), Some(y)) = (c.name("mn"), num("md"), num("my")) {
            match month_of(mn.as_str()) {
                Some(mo) => (y, mo, d),
                None => continue,
            }
        } else if let (Some(d), Some(mn), Some(y)) = (num("dd"), c.name("dm"), num("dy")) {
            match month_of(mn.as_str()) {
                Some(mo) => (y, mo, d),
                None => continue,
            }
        } else {
            continue;
        };
        if let Some(iso) = valid_iso(y, mo, d) {
            out.push((iso, m.as_str().to_string(), m.start()));
        }
    }
    out
}

/// `20260304` in a file name.
fn compact_dates(s: &str) -> Vec<(String, String, usize)> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?:^|[^0-9])((?:19|20)\d{2})(\d{2})(\d{2})(?:[^0-9]|$)").unwrap()
    });
    re.captures_iter(s)
        .filter_map(|c| {
            let y = c[1].parse().ok()?;
            let m = c[2].parse().ok()?;
            let d = c[3].parse().ok()?;
            let all = c.get(0)?;
            valid_iso(y, m, d).map(|iso| {
                (
                    iso,
                    all.as_str()
                        .trim_matches(|ch: char| !ch.is_ascii_digit())
                        .to_string(),
                    all.start(),
                )
            })
        })
        .collect()
}

pub fn valid_iso(y: u32, m: u32, d: u32) -> Option<String> {
    if !(1900..=2100).contains(&y) || !(1..=12).contains(&m) || d == 0 {
        return None;
    }
    let leap = (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    (d <= days[m as usize - 1]).then(|| format!("{y:04}-{m:02}-{d:02}"))
}

// ---- types ----------------------------------------------------------------

fn type_hits(doc: &Squashed, lines: &[&str]) -> Vec<Hit> {
    let head = Squashed::new(
        &lines
            .iter()
            .take(TITLE_LINES)
            .copied()
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let mut hits = Vec::new();
    for (code, phrase, weight) in TYPE_RULES {
        let found = doc.find(phrase);
        if let Some(&(s, e)) = found.first() {
            let titled = !head.find(phrase).is_empty();
            let w = if titled {
                weight.saturating_add(TITLE_BONUS)
            } else {
                *weight
            };
            let rule = if titled {
                "type_phrase_in_title"
            } else {
                "type_phrase"
            };
            hits.push(hit(
                code,
                phrase_group(phrase),
                rule,
                "text",
                w,
                doc.original(s, e),
            ));
        }
    }
    hits
}

/// Every phrase is its own group, so two different phrases for one type
/// earn the agreement bonus.
fn phrase_group(phrase: &str) -> &'static str {
    TYPE_RULES
        .iter()
        .find(|(_, p, _)| *p == phrase)
        .map(|(_, p, _)| *p)
        .unwrap_or("type")
}

// ---- organizations ----------------------------------------------------------

fn org_hits(doc: &Squashed, name: &Squashed, lines: &[&str], known: &Known) -> Vec<Hit> {
    let mut hits = Vec::new();
    let head: Vec<&str> = lines.iter().take(6).copied().collect();
    let head_sq = Squashed::new(&head.join("\n"));
    for org in known.organizations {
        let mut spellings = vec![org.name.as_str()];
        spellings.extend(org.aliases.iter().map(String::as_str));
        for s in spellings {
            let f = fold(s);
            if f.split_whitespace().map(str::len).sum::<usize>() < 3 {
                continue;
            }
            if let Some(&(a, b)) = head_sq.find(&f).first() {
                hits.push(hit(
                    &org.name,
                    "known",
                    "known_org_letterhead",
                    "text",
                    85,
                    head_sq.original(a, b),
                ));
            } else if let Some(&(a, b)) = doc.find(&f).first() {
                hits.push(hit(
                    &org.name,
                    "known",
                    "known_org_in_text",
                    "text",
                    65,
                    doc.original(a, b),
                ));
            }
            if !name.find(&f).is_empty() {
                hits.push(hit(
                    &org.name,
                    "filename",
                    "known_org_in_filename",
                    "filename",
                    70,
                    s,
                ));
            }
        }
    }
    if hits.is_empty() {
        for line in &head {
            let l = Squashed::new(line);
            if ORG_WORDS.iter().any(|w| !l.find(w).is_empty()) {
                let value = clip(line.trim());
                if value.chars().any(char::is_alphabetic) {
                    hits.push(hit(
                        &value,
                        "letterhead",
                        "org_word_in_letterhead",
                        "text",
                        50,
                        &value,
                    ));
                    break;
                }
            }
        }
    }
    hits
}

// ---- text normalization -------------------------------------------------------

/// Text folded for matching: lowercase, Latin diacritics removed, every run
/// of non-alphanumerics one space, space-padded, so `find(" word ")` is a
/// whole-word search. `map` points each folded char back at its byte offset
/// in the original, so evidence quotes the document's own spelling.
pub struct Squashed {
    raw: String,
    folded: Vec<char>,
    map: Vec<usize>,
}

impl Squashed {
    pub fn new(raw: &str) -> Self {
        let mut folded = vec![' '];
        let mut map = vec![0];
        for (i, c) in raw.char_indices() {
            for f in fold_char(c) {
                if f.is_alphanumeric() {
                    folded.push(f);
                    map.push(i);
                } else if folded.last() != Some(&' ') {
                    folded.push(' ');
                    map.push(i);
                }
            }
        }
        if folded.last() != Some(&' ') {
            folded.push(' ');
            map.push(raw.len());
        }
        Squashed {
            raw: raw.to_string(),
            folded,
            map,
        }
    }

    /// Whole-word occurrences of a folded phrase, as char spans.
    pub fn find(&self, phrase: &str) -> Vec<(usize, usize)> {
        let needle: Vec<char> = format!(" {} ", fold(phrase).trim()).chars().collect();
        if needle.len() <= 2 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i + needle.len() <= self.folded.len() {
            if self.folded[i..i + needle.len()] == needle[..] {
                out.push((i + 1, i + needle.len() - 1));
                i += needle.len() - 1;
            } else {
                i += 1;
            }
        }
        out
    }

    /// Byte offsets in the original text of words that start with the
    /// folded `prefix` (`hemo` finds `Hémoglobine`).
    pub fn find_prefix(&self, prefix: &str) -> Vec<usize> {
        let needle: Vec<char> = format!(" {}", fold(prefix).trim()).chars().collect();
        if needle.len() <= 1 {
            return Vec::new();
        }
        (0..self.folded.len().saturating_sub(needle.len() - 1))
            .filter(|&i| self.folded[i..i + needle.len()] == needle[..])
            .map(|i| self.map.get(i + 1).copied().unwrap_or(0))
            .collect()
    }

    fn starts_with_label(&self, label: &str) -> bool {
        let needle: Vec<char> = format!(" {} ", fold(label)).chars().collect();
        self.folded.len() >= needle.len() && self.folded[..needle.len()] == needle[..]
    }

    /// The original text behind a folded span.
    pub fn original(&self, start: usize, end: usize) -> &str {
        let a = self.map.get(start).copied().unwrap_or(0);
        let b = self.map.get(end).copied().unwrap_or(self.raw.len()).max(a);
        let mut b = b.min(self.raw.len());
        while !self.raw.is_char_boundary(b) {
            b += 1;
        }
        self.raw[a..b].trim()
    }

    pub fn words(&self) -> impl Iterator<Item = String> + '_ {
        self.folded
            .split(|c| *c == ' ')
            .filter(|w| !w.is_empty())
            .map(|w| w.iter().collect())
    }
}

/// Lowercase, Latin diacritics removed, non-alphanumerics to spaces.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        for f in fold_char(c) {
            out.push(if f.is_alphanumeric() { f } else { ' ' });
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fold_char(c: char) -> Vec<char> {
    let base = match c {
        'À'..='Å' | 'à'..='å' | 'Ā' | 'ā' | 'Ă' | 'ă' | 'Ą' | 'ą' => 'a',
        'Ç' | 'ç' | 'Ć' | 'ć' | 'Č' | 'č' => 'c',
        'Ď' | 'ď' | 'Đ' | 'đ' => 'd',
        'È'..='Ë' | 'è'..='ë' | 'Ē' | 'ē' | 'Ė' | 'ė' | 'Ę' | 'ę' | 'Ě' | 'ě' => 'e',
        'Ì'..='Ï' | 'ì'..='ï' | 'Ī' | 'ī' | 'Į' | 'į' | 'İ' | 'ı' => 'i',
        'Ñ' | 'ñ' | 'Ń' | 'ń' | 'Ň' | 'ň' => 'n',
        'Ò'..='Ö' | 'Ø' | 'ò'..='ö' | 'ø' | 'Ō' | 'ō' | 'Ő' | 'ő' => 'o',
        'Ù'..='Ü' | 'ù'..='ü' | 'Ū' | 'ū' | 'Ů' | 'ů' | 'Ű' | 'ű' => 'u',
        'Ý' | 'ý' | 'ÿ' | 'Ÿ' => 'y',
        'Ś' | 'ś' | 'Š' | 'š' | 'Ş' | 'ş' => 's',
        'Ź' | 'ź' | 'Ż' | 'ż' | 'Ž' | 'ž' => 'z',
        'Ł' | 'ł' => 'l',
        'Ř' | 'ř' => 'r',
        'Ť' | 'ť' => 't',
        'ß' => return vec!['s', 's'],
        'Æ' | 'æ' => return vec!['a', 'e'],
        _ => return c.to_lowercase().collect(),
    };
    vec![base]
}

fn clip(s: &str) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 80 {
        let mut c: String = collapsed.chars().take(80).collect();
        c.push('…');
        c
    } else {
        collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Identifier, Links};

    fn person(id: &str, name: &str, dob: Option<&str>) -> Person {
        Person {
            id: id.into(),
            name: name.into(),
            folder: name.into(),
            birth_date: dob.map(str::to_string),
            adult: true,
            guardians: vec![],
            aliases: vec![],
            identifiers: vec![],
            links: Links::default(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn people() -> Vec<Person> {
        let mut a = person(
            "p_aaaaaaaaaaaaaaaaaaaa",
            "Alex Testperson",
            Some("1990-01-01"),
        );
        a.identifiers.push(Identifier {
            source: "Example Clinic".into(),
            value: "MRN000111".into(),
            kind: Some("mrn".into()),
        });
        let b = person(
            "p_bbbbbbbbbbbbbbbbbbbb",
            "Sam Testperson",
            Some("1991-02-02"),
        );
        let mut c = person(
            "p_cccccccccccccccccccc",
            "Robin Testperson",
            Some("2026-01-15"),
        );
        c.adult = false;
        vec![a, b, c]
    }

    fn orgs() -> Vec<Organization> {
        vec![Organization {
            id: "g_aaaaaaaaaaaaaaaaaaaa".into(),
            name: "Example Pediatrics".into(),
            aliases: vec!["Example Peds".into()],
            fhir_base: None,
        }]
    }

    fn run(text: &str, file: &str) -> Classification {
        let p = people();
        let o = orgs();
        classify(
            text,
            file,
            &Known {
                people: &p,
                organizations: &o,
            },
            "2026-11-12",
        )
    }

    #[test]
    fn a_lab_report_with_a_labelled_patient_and_service_date_is_decided() {
        let c = run(
            "Example Pediatrics\nLaboratory Report\nPatient: Robin Testperson   DOB: 01/15/2026\n\
             Collected: 03/04/2026\nTest  Result  Reference range\nHemoglobin 12.1 11-15\n",
            "scan0001.pdf",
        );
        assert_eq!(c.person.value(), Some("p_cccccccccccccccccccc"));
        assert_eq!(c.date.value(), Some("2026-03-04"));
        assert_eq!(c.doc_type.value(), Some("lab_report"));
        assert_eq!(c.organization.value(), Some("Example Pediatrics"));
        assert!(c.decided(), "{c:#?}");
    }

    #[test]
    fn a_birth_date_is_never_the_date_of_service() {
        let c = run(
            "Patient: Robin Testperson\nDOB 2026-01-15\nVisit summary\n",
            "x.pdf",
        );
        assert_ne!(c.date.value(), Some("2026-01-15"));
        assert!(!c.date.decided());
    }

    #[test]
    fn a_parent_and_a_child_on_one_page_is_ambiguous() {
        let c = run(
            "Statement\nResponsible party: Alex Testperson\nPatient: Robin Testperson\n\
             Guarantor Alex Testperson DOB 01/01/1990\nAmount due $0.00\n",
            "bill.pdf",
        );
        let p = c.person.proposal.as_ref().unwrap();
        assert!(!c.person.decided(), "{p:#?}");
        assert!(p.note.as_deref().unwrap_or("").starts_with("ambiguous"));
    }

    #[test]
    fn an_identifier_match_is_masked_in_evidence() {
        let c = run(
            "Lab results\nMRN: MRN000111\nCollected 2026-02-03\n",
            "a.pdf",
        );
        let p = c.person.proposal.unwrap();
        assert_eq!(p.value, "p_aaaaaaaaaaaaaaaaaaaa");
        let ev = p.evidence.iter().find(|e| e.rule == "identifier").unwrap();
        assert_eq!(ev.matched, "•••••••11");
    }

    #[test]
    fn eob_letters_bills_and_cards_are_told_apart() {
        let eob = run(
            "Explanation of Benefits\nTHIS IS NOT A BILL\nAmount plan paid 10.00\n",
            "a.pdf",
        );
        assert_eq!(eob.doc_type.value(), Some("eob"));
        assert!(eob.doc_type.decided());
        let bill = run(
            "Example Pediatrics\nStatement date 2026-01-02\nAmount due 25.00\nPlease pay\n",
            "b.pdf",
        );
        assert_eq!(bill.doc_type.value(), Some("bill"));
        let card = run(
            "Example Health Plan\nMember ID ZZZ000000001\nRxBIN 000000\nRxPCN ABC\n",
            "c.jpg",
        );
        assert_eq!(card.doc_type.value(), Some("insurance_card"));
    }

    #[test]
    fn dates_parse_in_common_spellings_and_reject_impossible_ones() {
        let found: Vec<String> =
            all_dates("on March 4, 2026 and 4 Mar 2026 and 3/4/26 and 2026-03-04 and 02/30/2026")
                .into_iter()
                .map(|d| d.0)
                .collect();
        assert_eq!(found, ["2026-03-04"; 4]);
        assert_eq!(compact_dates("scan_20260304_1.pdf")[0].0, "2026-03-04");
    }

    #[test]
    fn future_dates_are_not_dates_of_service() {
        let c = run(
            "Visit summary\nNext appointment 2027-01-01\nVisit date 2026-10-01\n",
            "a.pdf",
        );
        assert_eq!(c.date.value(), Some("2026-10-01"));
    }

    #[test]
    fn folding_matches_accented_and_punctuated_spellings() {
        let s = Squashed::new("Clínica São-José, Inc.");
        assert_eq!(s.find("clinica sao jose").len(), 1);
        assert_eq!(s.original(1, 17), "Clínica São-José");
        assert_eq!(fold("Straße"), "strasse");
    }

    #[test]
    fn nothing_known_proposes_nothing() {
        let c = classify(
            "blank",
            "x.pdf",
            &Known {
                people: &[],
                organizations: &[],
            },
            "2026-11-12",
        );
        assert!(c.person.proposal.is_none() && c.doc_type.proposal.is_none());
        assert_eq!(c.unsure(), ["person", "date", "type"]);
    }
}
