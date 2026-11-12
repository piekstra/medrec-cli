//! Read views over the index: the timeline, search, the record lists
//! (results, medications, conditions, allergies, immunizations, claims),
//! documents and notes. Everything is computed in memory from a person's
//! `index.json`, `text.json` and `notes.md`; nothing here writes.

use pk_cli_core::CliError;
use serde::Serialize;
use serde_json::Value;

use crate::folder::Folder;
use crate::intake::classify;
use crate::model::{Household, Note, Person, Record};
use crate::people;

#[derive(Debug, Clone, Default)]
pub struct Range {
    pub since: Option<String>,
    pub until: Option<String>,
}

impl Range {
    pub fn validate(&self) -> Result<(), CliError> {
        for d in [&self.since, &self.until].into_iter().flatten() {
            pk_cli_core::dates::parse_iso(d)?;
        }
        Ok(())
    }

    /// An undated item shows only when no range is asked for.
    fn holds(&self, date: Option<&str>) -> bool {
        match date {
            None => self.since.is_none() && self.until.is_none(),
            Some(d) => {
                let d = &d[..d.len().min(10)];
                self.since.as_deref().is_none_or(|s| d >= s)
                    && self.until.as_deref().is_none_or(|u| d <= u)
            }
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Event {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub person: String,
    pub person_name: String,
    /// `document`, `study`, `note`, or a record kind (`encounter`,
    /// `result`, `report`, `condition`, `medication`, `allergy`,
    /// `immunization`, `procedure`, `coverage`, `claim`).
    pub kind: String,
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Where it came from: an organization, a provider CLI, a scan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

pub fn timeline(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    range: &Range,
    kinds: &[String],
) -> Result<Vec<Event>, CliError> {
    range.validate()?;
    let want = |k: &str| kinds.is_empty() || kinds.iter().any(|x| x == k);
    let mut out = Vec::new();
    for p in people::selected(h, person)? {
        let idx = folder.index(p)?;
        if want("document") {
            for d in &idx.documents {
                if !range.holds(d.date.as_deref()) {
                    continue;
                }
                out.push(Event {
                    date: d.date.clone(),
                    person: p.id.clone(),
                    person_name: p.name.clone(),
                    kind: "document".into(),
                    id: d.id.clone(),
                    title: d
                        .title
                        .clone()
                        .unwrap_or_else(|| label(d.doc_type.as_deref()).to_string()),
                    detail: d.original_name.clone(),
                    source: d.organization.clone(),
                });
            }
        }
        for r in &idx.records {
            if !want(&r.kind) || !range.holds(r.date.as_deref()) {
                continue;
            }
            out.push(Event {
                date: r.date.clone(),
                person: p.id.clone(),
                person_name: p.name.clone(),
                kind: r.kind.clone(),
                id: r.id.clone(),
                title: r.display.clone(),
                detail: record_detail(r),
                source: record_source(h, r),
            });
        }
        if want("study") {
            for s in &idx.studies {
                if !range.holds(s.date.as_deref()) {
                    continue;
                }
                out.push(Event {
                    date: s.date.clone(),
                    person: p.id.clone(),
                    person_name: p.name.clone(),
                    kind: "study".into(),
                    id: s.id.clone(),
                    title: format!(
                        "{} {}",
                        s.modalities.join("/"),
                        s.description
                            .clone()
                            .or_else(|| s.body_part.clone())
                            .unwrap_or_default()
                    )
                    .trim()
                    .to_string(),
                    detail: Some(format!("{} series, {} images", s.series.len(), s.instances)),
                    source: s.facility.clone(),
                });
            }
        }
        if want("note") {
            for n in folder.notes(p)? {
                if !range.holds(Some(&n.date)) {
                    continue;
                }
                out.push(Event {
                    date: Some(n.date.clone()),
                    person: p.id.clone(),
                    person_name: p.name.clone(),
                    kind: "note".into(),
                    id: n.id.clone(),
                    title: first_line(&n.text, 80),
                    detail: (!n.tags.is_empty()).then(|| n.tags.join(", ")),
                    source: n.author.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| {
        b.date
            .cmp(&a.date)
            .then(a.kind.cmp(&b.kind))
            .then(a.title.cmp(&b.title))
    });
    Ok(out)
}

fn label(doc_type: Option<&str>) -> &'static str {
    doc_type
        .and_then(classify::doc_type_label)
        .unwrap_or("Document")
}

fn first_line(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max - 1).collect::<String>())
    } else {
        line.to_string()
    }
}

fn record_detail(r: &Record) -> Option<String> {
    let f = &r.fields;
    let text = |k: &str| f.get(k).and_then(Value::as_str).map(str::to_string);
    match r.kind.as_str() {
        "result" => {
            let v = f.get("value").map(value_text).filter(|s| !s.is_empty());
            let flag = text("interpretation");
            match (v, flag) {
                (Some(v), Some(i)) => Some(format!("{v} ({i})")),
                (v, i) => v.or(i),
            }
        }
        "medication" => text("dosage").or_else(|| r.status.clone()),
        "claim" => {
            let billed = f
                .get("total")
                .and_then(|t| t.get(0))
                .and_then(|t| t.get("amount"))
                .map(value_text)
                .filter(|s| !s.is_empty());
            let paid = f.get("payment").map(value_text).filter(|s| !s.is_empty());
            match (billed, paid) {
                (Some(b), Some(p)) => Some(format!("billed {b}, plan paid {p}")),
                (Some(b), None) => Some(format!("billed {b}")),
                (None, Some(p)) => Some(format!("plan paid {p}")),
                (None, None) => r.status.clone(),
            }
        }
        _ => r.status.clone(),
    }
}

/// Every string and number in `v`, space-separated, for search.
fn flat_text(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => {
            out.push_str(s);
            out.push(' ');
        }
        Value::Number(n) => {
            out.push_str(&n.to_string());
            out.push(' ');
        }
        Value::Array(a) => a.iter().for_each(|x| flat_text(x, out)),
        Value::Object(o) => o.values().for_each(|x| flat_text(x, out)),
        _ => {}
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Object(o) => {
            let num = o
                .get("value")
                .or_else(|| o.get("amount"))
                .map(value_text)
                .unwrap_or_default();
            let unit = o
                .get("unit")
                .or_else(|| o.get("currency"))
                .and_then(Value::as_str)
                .unwrap_or("");
            format!("{num} {unit}").trim().to_string()
        }
        Value::Array(a) => a.iter().map(value_text).collect::<Vec<_>>().join("; "),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
    }
}

fn record_source(h: &Household, r: &Record) -> Option<String> {
    let p = r.provenance.first()?;
    let base = p.source.fhir_base.as_deref();
    h.organizations
        .iter()
        .find(|o| base.is_some() && o.fhir_base.as_deref() == base)
        .map(|o| o.name.clone())
        .or_else(|| p.source.cli.clone())
}

/// One row of a record list.
#[derive(Debug, Serialize)]
pub struct RecordRow {
    pub id: String,
    pub person: String,
    pub person_name: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub display: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub sources: Vec<String>,
    pub fields: serde_json::Map<String, Value>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<String>,
}

/// Records of `kinds` for the selected people, newest first. `active`
/// keeps those whose status is not ended (`active`, `on-hold`,
/// `unconfirmed` and unknown).
pub fn records(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    kinds: &[&str],
    range: &Range,
    active: bool,
    code: Option<&str>,
) -> Result<Vec<RecordRow>, CliError> {
    range.validate()?;
    let mut out = Vec::new();
    for p in people::selected(h, person)? {
        for r in folder.index(p)?.records {
            if !kinds.contains(&r.kind.as_str()) || !range.holds(r.date.as_deref()) {
                continue;
            }
            if active && is_ended(r.status.as_deref()) {
                continue;
            }
            if code.is_some_and(|q| !matches_code(&r, q)) {
                continue;
            }
            out.push(row(h, p, r));
        }
    }
    out.sort_by(|a, b| b.date.cmp(&a.date).then(a.display.cmp(&b.display)));
    Ok(out)
}

/// `q` is the record's code (`718-7`, `J45.909`), or words of its name
/// (`hemoglobin`), case-insensitive.
fn matches_code(r: &Record, q: &str) -> bool {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    let code = r.code.as_ref();
    if code
        .and_then(|c| c.code.as_deref())
        .is_some_and(|c| c.to_lowercase() == q)
    {
        return true;
    }
    let names = [
        Some(r.display.as_str()),
        code.and_then(|c| c.display.as_deref()),
    ];
    names
        .into_iter()
        .flatten()
        .any(|n| n.to_lowercase().contains(&q))
}

fn is_ended(status: Option<&str>) -> bool {
    matches!(
        status,
        Some(
            "completed"
                | "stopped"
                | "inactive"
                | "resolved"
                | "remission"
                | "entered-in-error"
                | "cancelled"
                | "refuted"
        )
    )
}

fn row(h: &Household, p: &Person, r: Record) -> RecordRow {
    let mut sources: Vec<String> = r
        .provenance
        .iter()
        .filter_map(|pv| {
            let base = pv.source.fhir_base.as_deref();
            h.organizations
                .iter()
                .find(|o| base.is_some() && o.fhir_base.as_deref() == base)
                .map(|o| o.name.clone())
                .or_else(|| pv.source.cli.clone())
        })
        .collect();
    sources.dedup();
    let detail = record_detail(&r).filter(|d| Some(d) != r.status.as_ref());
    RecordRow {
        detail,
        id: r.id,
        person: p.id.clone(),
        person_name: p.name.clone(),
        kind: r.kind,
        date: r.date,
        display: r.display,
        status: r.status,
        code: r.code.and_then(|c| c.code),
        sources,
        fields: r.fields,
        documents: r.documents,
    }
}

#[derive(Debug, Serialize)]
pub struct Hit {
    pub person: String,
    pub person_name: String,
    /// `document`, `note`, `study`, or a record kind.
    pub kind: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub title: String,
    pub snippet: String,
    /// Matching terms counted over the item; higher first.
    pub score: usize,
}

/// Every item whose text holds all the query's words (case, accents and
/// punctuation ignored), best first.
pub fn search(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    query: &str,
    range: &Range,
    limit: usize,
) -> Result<Vec<Hit>, CliError> {
    range.validate()?;
    let terms: Vec<String> = classify::fold(query)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect();
    if terms.is_empty() {
        return Err(CliError::Usage("search needs at least one word".into()));
    }
    let mut hits = Vec::new();
    for p in people::selected(h, person)? {
        let idx = folder.index(p)?;
        let texts = folder.text(p)?;
        let mut consider = |kind: &str, id: &str, date: Option<&str>, title: String, body: &str| {
            if !range.holds(date) {
                return;
            }
            let hay = format!("{title}\n{body}");
            if let Some((score, snippet)) = matches(&hay, &terms) {
                hits.push(Hit {
                    person: p.id.clone(),
                    person_name: p.name.clone(),
                    kind: kind.into(),
                    id: id.into(),
                    date: date.map(str::to_string),
                    title,
                    snippet,
                    score,
                });
            }
        };
        for d in &idx.documents {
            let title = d
                .title
                .clone()
                .or_else(|| d.original_name.clone())
                .unwrap_or_else(|| label(d.doc_type.as_deref()).into());
            let meta = format!(
                "{} {}",
                d.organization.as_deref().unwrap_or(""),
                label(d.doc_type.as_deref())
            );
            let body = texts.texts.get(&d.id).map(String::as_str).unwrap_or("");
            consider(
                "document",
                &d.id,
                d.date.as_deref(),
                title,
                &format!("{meta}\n{body}"),
            );
        }
        for r in &idx.records {
            let mut body = String::new();
            flat_text(&Value::Object(r.fields.clone()), &mut body);
            consider(&r.kind, &r.id, r.date.as_deref(), r.display.clone(), &body);
        }
        for s in &idx.studies {
            let body = format!(
                "{} {} {} {}",
                s.modalities.join(" "),
                s.body_part.as_deref().unwrap_or(""),
                s.facility.as_deref().unwrap_or(""),
                s.series
                    .iter()
                    .filter_map(|x| x.description.as_deref())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            consider(
                "study",
                &s.id,
                s.date.as_deref(),
                s.description
                    .clone()
                    .unwrap_or_else(|| "Imaging study".into()),
                &body,
            );
        }
        for n in folder.notes(p)? {
            // The title is the text's first line; the rest is the body.
            let rest: String = n.text.trim().lines().skip(1).collect::<Vec<_>>().join("\n");
            let body = format!("{}\n{rest}", n.tags.join(" "));
            consider("note", &n.id, Some(&n.date), first_line(&n.text, 80), &body);
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(b.date.cmp(&a.date)));
    hits.truncate(limit.max(1));
    Ok(hits)
}

/// All terms present (each as a word prefix): the count of their
/// occurrences and a snippet around the first.
fn matches(hay: &str, terms: &[String]) -> Option<(usize, String)> {
    let sq = classify::Squashed::new(hay);
    let mut score = 0;
    let mut first = usize::MAX;
    for t in terms {
        let found = sq.find_prefix(t);
        if found.is_empty() {
            return None;
        }
        score += found.len();
        first = first.min(found[0]);
    }
    let floor = |mut i: usize| {
        i = i.min(hay.len());
        while !hay.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let start = floor(first.saturating_sub(60));
    let end = floor(first.saturating_add(100));
    let mut snippet = hay[start..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if start > 0 {
        snippet.insert(0, '…');
    }
    if end < hay.len() {
        snippet.push('…');
    }
    Some((score, snippet))
}

#[derive(Debug, Serialize)]
pub struct DocumentRow {
    pub id: String,
    pub person: String,
    pub person_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub path: String,
    pub size: u64,
    pub media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    /// `fhir`, `inbox`, `drive`, `file`.
    pub origin: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub near_duplicates: Vec<String>,
}

pub fn documents(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    doc_type: Option<&str>,
    range: &Range,
) -> Result<Vec<DocumentRow>, CliError> {
    range.validate()?;
    if let Some(t) = doc_type {
        if !classify::is_doc_type(t) {
            return Err(CliError::Usage(format!("`{t}` is not a document type")));
        }
    }
    let mut out = Vec::new();
    for p in people::selected(h, person)? {
        for d in folder.index(p)?.documents {
            if doc_type.is_some_and(|t| d.doc_type.as_deref() != Some(t))
                || !range.holds(d.date.as_deref())
            {
                continue;
            }
            out.push(DocumentRow {
                id: d.id,
                person: p.id.clone(),
                person_name: p.name.clone(),
                date: d.date,
                doc_type: d.doc_type,
                organization: d.organization,
                title: d.title.or(d.original_name),
                path: d.path,
                size: d.size,
                media_type: d.media_type,
                pages: d.pages,
                origin: d.origin.kind,
                near_duplicates: d.near_duplicates.into_iter().map(|n| n.document).collect(),
            });
        }
    }
    out.sort_by(|a, b| b.date.cmp(&a.date).then(a.path.cmp(&b.path)));
    Ok(out)
}

/// A document by id, with whose it is.
pub fn find_document(
    folder: &Folder,
    h: &Household,
    id: &str,
) -> Result<(Person, crate::model::Document), CliError> {
    for p in &h.people {
        if let Some(d) = folder.index(p)?.documents.into_iter().find(|d| d.id == id) {
            return Ok((p.clone(), d));
        }
    }
    Err(CliError::NotFound(format!(
        "no document `{id}`; `medrec documents list` shows them"
    )))
}

#[derive(Debug, Serialize)]
pub struct NoteRow {
    pub person: String,
    pub person_name: String,
    #[serde(flatten)]
    pub note: Note,
}

pub fn notes(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
    tag: Option<&str>,
    range: &Range,
) -> Result<Vec<NoteRow>, CliError> {
    range.validate()?;
    let mut out = Vec::new();
    for p in people::selected(h, person)? {
        for n in folder.notes(p)? {
            if tag.is_some_and(|t| !n.tags.iter().any(|x| x.eq_ignore_ascii_case(t)))
                || !range.holds(Some(&n.date))
            {
                continue;
            }
            out.push(NoteRow {
                person: p.id.clone(),
                person_name: p.name.clone(),
                note: n,
            });
        }
    }
    out.sort_by(|a, b| b.note.date.cmp(&a.note.date));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_terms_must_match_and_the_snippet_surrounds_the_first() {
        let terms = vec!["hemoglobin".to_string(), "a1c".to_string()];
        let (score, snip) = matches(
            "Results: Hemoglobin A1c 5.4 %, normal. Hemoglobin again",
            &terms,
        )
        .unwrap();
        assert_eq!(score, 3);
        assert!(snip.contains("Hemoglobin A1c"));
        assert!(matches("Hemoglobin only", &terms).is_none());
    }

    #[test]
    fn ranges_include_their_ends_and_hide_undated_items() {
        let r = Range {
            since: Some("2026-01-01".into()),
            until: Some("2026-03-04".into()),
        };
        assert!(r.holds(Some("2026-03-04T10:00:00Z")));
        assert!(!r.holds(Some("2025-12-31")));
        assert!(!r.holds(None));
        assert!(Range::default().holds(None));
    }
}
