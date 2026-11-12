//! The review queue (`_review/` and `_review/review.json`): documents intake
//! could not decide, or whose given values disagreed with what medrec read.
//! `review file` finishes the classification (from flags or by asking) and
//! moves the document to where it belongs; `review drop` discards it.

use pk_cli_core::CliError;
use serde::Serialize;
use serde_json::Value;

use crate::folder::{self, Folder};
use crate::ids;
use crate::intake::{self, classify, Given};
use crate::model::{
    filed_by, now_rfc3339, Document, Household, Origin, Provenance, ProvenanceSource, ReviewItem,
};
use crate::people;

#[derive(Debug, Serialize)]
pub struct Proposed {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    pub confidence: Value,
}

#[derive(Debug, Serialize)]
pub struct Row {
    pub id: String,
    pub original_name: String,
    pub received_at: String,
    pub origin: String,
    pub path: String,
    pub size: u64,
    pub media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pages: Option<u32>,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classified_by: Option<String>,
    pub proposed: Proposed,
}

fn classification(item: &ReviewItem) -> classify::Classification {
    serde_json::from_value(item.classification.clone()).unwrap_or_default()
}

fn row(h: &Household, item: &ReviewItem) -> Row {
    let class = classification(item);
    let person = class.person.value().map(str::to_string);
    Row {
        id: item.id.clone(),
        original_name: item.original_name.clone(),
        received_at: item.received_at.clone(),
        origin: item.origin.kind.clone(),
        path: item.path.clone(),
        size: item.size,
        media_type: item.media_type.clone(),
        pages: item.pages,
        reasons: item.reasons.clone(),
        classified_by: item.classified_by.clone(),
        proposed: Proposed {
            person_name: person.as_deref().map(|p| h.name_of(p)),
            person,
            date: class.date.value().map(str::to_string),
            doc_type: class.doc_type.value().map(str::to_string),
            organization: class.organization.value().map(str::to_string),
            confidence: intake::confidences(&class),
        },
    }
}

pub fn list(folder: &Folder) -> Result<Vec<Row>, CliError> {
    let h = folder.household()?;
    let mut rows: Vec<Row> = folder.review()?.items.iter().map(|i| row(&h, i)).collect();
    rows.sort_by(|a, b| a.received_at.cmp(&b.received_at));
    Ok(rows)
}

#[derive(Debug, Serialize)]
pub struct Detail {
    #[serde(flatten)]
    pub row: Row,
    /// The classification with its evidence, as intake recorded it.
    pub classification: Value,
    pub text_chars: usize,
    /// The extracted text's first characters; `--text` prints all of it.
    pub text: String,
}

pub fn show(folder: &Folder, id: &str, full_text: bool) -> Result<Detail, CliError> {
    let h = folder.household()?;
    let q = folder.review()?;
    let item = find(&q.items, id)?;
    let text = if full_text {
        item.text.clone()
    } else {
        item.text.chars().take(1200).collect()
    };
    Ok(Detail {
        row: row(&h, item),
        classification: item.classification.clone(),
        text_chars: item.text.chars().count(),
        text,
    })
}

fn find<'a>(items: &'a [ReviewItem], id: &str) -> Result<&'a ReviewItem, CliError> {
    items.iter().find(|i| i.id == id).ok_or_else(|| {
        CliError::NotFound(format!(
            "no review item `{id}`; `medrec review list` shows them"
        ))
    })
}

pub struct FileRequest {
    pub id: String,
    pub given: Given,
    pub interactive: bool,
}

#[derive(Debug, Serialize)]
pub struct Filed {
    pub review: String,
    /// `filed` or `duplicate`.
    pub status: String,
    pub person: String,
    pub person_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,
    pub path: String,
    pub doc_type: String,
    pub date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duplicate_of: Option<String>,
}

pub fn file(folder: &Folder, req: &FileRequest) -> Result<Filed, CliError> {
    let household = folder.household()?;
    let mut queue = folder.review()?;
    let item = find(&queue.items, &req.id)?.clone();
    intake::validate_given(&req.given)?;
    let given_person = req
        .given
        .person
        .as_deref()
        .map(|q| people::resolve(&household, q).map(|p| p.id.clone()))
        .transpose()?;
    let mut class = classification(&item);
    intake::apply_given(&mut class, &req.given, given_person.as_deref());
    if !class.decided() && req.interactive {
        intake::ask(
            &item.original_name,
            &mut class,
            &household,
            &household.organizations,
        )?;
    }
    if !class.decided() {
        return Err(CliError::Usage(format!(
            "medrec is still unsure of {}; pass --person, --date and --type (or run it in a terminal to be asked)",
            class.unsure().join(", ")
        )));
    }
    let pid = class.person.value().expect("decided").to_string();
    let person = household
        .person(&pid)
        .ok_or_else(|| CliError::Usage(format!("no person `{pid}` in this records folder")))?
        .clone();
    let doc_type = class.doc_type.value().expect("decided").to_string();
    let date = class.date.value().expect("decided").to_string();
    let org = class.organization.value().map(str::to_string);

    let mut index = folder.index(&person)?;
    if let Some(dup) = index.documents.iter().find(|d| d.sha256 == item.sha256) {
        // Already filed: the queued copy goes, once the filed one checks out.
        folder.check_md5(&dup.path, &dup.md5)?;
        let (dup_id, dup_path) = (dup.id.clone(), dup.path.clone());
        folder.store.delete(&item.path)?;
        queue.items.retain(|i| i.id != item.id);
        folder.save_review(&mut queue)?;
        return Ok(Filed {
            review: item.id,
            status: "duplicate".into(),
            person: person.id.clone(),
            person_name: person.name.clone(),
            document: None,
            path: dup_path,
            doc_type,
            date,
            organization: org,
            duplicate_of: Some(dup_id),
        });
    }

    let name = folder::filed_name(&date, &doc_type, org.as_deref(), None, &item.original_name);
    let dir = folder::document_dir(&person, &doc_type, &date);
    let path = folder.move_new_file(&item.path, &dir, &name, &item.md5)?;
    let signatures: Vec<intake::Signature> = index
        .documents
        .iter()
        .map(|d| {
            (
                person.id.clone(),
                d.id.clone(),
                d.minhash.clone(),
                d.doc_type.clone(),
                d.date.clone(),
            )
        })
        .collect();
    let near = intake::near_duplicates_in(&signatures, &item.minhash, &class);
    let filed_at = now_rfc3339();
    let id = ids::new("d");
    index.documents.push(Document {
        id: id.clone(),
        path: path.clone(),
        sha256: item.sha256.clone(),
        md5: item.md5.clone(),
        size: item.size,
        media_type: item.media_type.clone(),
        pages: item.pages,
        original_name: Some(item.original_name.clone()),
        title: None,
        loinc: classify::doc_type_loinc(&doc_type).map(str::to_string),
        doc_type: Some(doc_type.clone()),
        date: Some(date.clone()),
        organization: org.clone(),
        filed_at: filed_at.clone(),
        origin: Origin {
            kind: item.origin.kind.clone(),
            name: item.origin.name.clone(),
            action: item.origin.action.clone(),
        },
        text: item.text_info.clone(),
        near_duplicates: near,
        minhash: item.minhash.clone(),
        classification: serde_json::to_value(&class).ok(),
        provenance: Provenance {
            source: ProvenanceSource {
                kind: if item.origin.kind == "drive" {
                    "drive"
                } else {
                    "scan"
                }
                .into(),
                cli: None,
                cli_version: None,
                organization_id: None,
                fhir_base: None,
            },
            method: if item
                .text_info
                .as_ref()
                .and_then(|t| t.ocr_pages)
                .unwrap_or(0)
                > 0
            {
                "scan"
            } else {
                "file"
            }
            .into(),
            source_resource: None,
            fetched_at: None,
            filed_at,
            raw_path: Some(path.clone()),
            classified_by: Some("hand".into()),
            derived_from: None,
            filed_by: filed_by(),
        },
    });
    crate::imaging::link_reports(&mut index);
    let mut texts = folder.text(&person)?;
    if !item.text.is_empty() {
        texts.texts.insert(id.clone(), item.text.clone());
    }
    folder.save_index(&person, &mut index)?;
    folder.save_text(&person, &texts)?;
    queue.items.retain(|i| i.id != item.id);
    folder.save_review(&mut queue)?;
    Ok(Filed {
        review: item.id,
        status: "filed".into(),
        person: person.id.clone(),
        person_name: person.name.clone(),
        document: Some(id),
        path,
        doc_type,
        date,
        organization: org,
        duplicate_of: None,
    })
}

#[derive(Debug, Serialize)]
pub struct Dropped {
    pub review: String,
    pub path: String,
    pub file_removed: bool,
}

/// Discard a queued document: its file in `_review/` and its queue entry.
pub fn drop(folder: &Folder, id: &str) -> Result<Dropped, CliError> {
    let mut queue = folder.review()?;
    let item = find(&queue.items, id)?.clone();
    let removed = folder.store.delete(&item.path)?;
    queue.items.retain(|i| i.id != item.id);
    folder.save_review(&mut queue)?;
    Ok(Dropped {
        review: item.id,
        path: item.path,
        file_removed: removed,
    })
}
