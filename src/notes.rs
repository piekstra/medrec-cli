//! `<Person>/notes.md`: dated notes, readable and editable in the Drive app.
//! The file is the notes' source; medrec parses it back, so a note added by
//! hand in the same shape is a note.
//!
//! ```text
//! # Notes: <name>
//!
//! ## 2026-03-04 (n_…)
//! Tags: question, follow-up
//! About: d_…
//! By: <name>
//!
//! The note's text, any number of lines.
//! ```

use crate::hash;
use crate::model::Note;

pub fn render(person_name: &str, notes: &[Note]) -> String {
    let mut sorted: Vec<&Note> = notes.iter().collect();
    sorted.sort_by(|a, b| a.date.cmp(&b.date).then(a.id.cmp(&b.id)));
    let mut out = format!("# Notes: {person_name}\n");
    for n in sorted {
        out.push_str(&format!("\n## {} ({})\n", n.date, n.id));
        if !n.tags.is_empty() {
            out.push_str(&format!("Tags: {}\n", n.tags.join(", ")));
        }
        if !n.about.is_empty() {
            out.push_str(&format!("About: {}\n", n.about.join(", ")));
        }
        if let Some(a) = &n.author {
            out.push_str(&format!("By: {a}\n"));
        }
        out.push('\n');
        for line in n.text.trim_end().lines() {
            if line.starts_with("## ") || line.starts_with('\\') {
                out.push('\\');
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

pub fn parse(text: &str) -> Vec<Note> {
    let mut notes = Vec::new();
    let mut current: Option<(String, Option<String>, Vec<String>)> = None;
    let flush = |cur: Option<(String, Option<String>, Vec<String>)>, notes: &mut Vec<Note>| {
        if let Some((date, id, lines)) = cur {
            notes.push(build(date, id, &lines));
        }
    };
    for line in text.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            flush(current.take(), &mut notes);
            let head = head.trim();
            let (date, id) = match head.split_once(' ') {
                Some((d, rest)) => {
                    let id = rest
                        .trim()
                        .trim_start_matches('(')
                        .trim_end_matches(')')
                        .trim();
                    (
                        d.to_string(),
                        crate::ids::is_valid("n", id).then(|| id.to_string()),
                    )
                }
                None => (head.to_string(), None),
            };
            current = Some((date, id, Vec::new()));
        } else if let Some((_, _, lines)) = current.as_mut() {
            lines.push(line.to_string());
        }
    }
    flush(current, &mut notes);
    notes
}

fn build(date: String, id: Option<String>, lines: &[String]) -> Note {
    let mut tags = Vec::new();
    let mut about = Vec::new();
    let mut author = None;
    let mut i = 0;
    while i < lines.len() {
        let l = &lines[i];
        if let Some(v) = l.strip_prefix("Tags:") {
            tags = list(v);
        } else if let Some(v) = l.strip_prefix("About:") {
            about = list(v);
        } else if let Some(v) = l.strip_prefix("By:") {
            author = Some(v.trim().to_string()).filter(|s| !s.is_empty());
        } else {
            break;
        }
        i += 1;
    }
    let body: Vec<&str> = lines[i..]
        .iter()
        .map(|l| l.strip_prefix('\\').unwrap_or(l))
        .collect();
    let text = body.join("\n").trim().to_string();
    let id = id.unwrap_or_else(|| {
        format!(
            "n_{}",
            &hash::sha256_hex(format!("{date}\n{text}").as_bytes())[..20]
        )
    });
    Note {
        id,
        date,
        author,
        text,
        tags,
        about,
    }
}

fn list(v: &str) -> Vec<String> {
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Add a note to a person's `notes.md`. `about` must name documents,
/// records or studies in that person's index.
pub fn add(
    folder: &crate::folder::Folder,
    person: &crate::model::Person,
    text: &str,
    date: Option<&str>,
    tags: Vec<String>,
    about: Vec<String>,
    author: Option<String>,
) -> Result<Note, pk_cli_core::CliError> {
    use pk_cli_core::CliError;
    let text = text.trim();
    if text.is_empty() {
        return Err(CliError::Usage("a note needs text".into()));
    }
    let date = match date {
        Some(d) => {
            pk_cli_core::dates::parse_iso(d)?;
            d.to_string()
        }
        None => crate::model::today(),
    };
    let tags: Vec<String> = tags
        .into_iter()
        .flat_map(|t| {
            t.split(',')
                .map(|s| s.trim().to_lowercase())
                .collect::<Vec<_>>()
        })
        .filter(|t| !t.is_empty())
        .collect();
    if !about.is_empty() {
        let idx = folder.index(person)?;
        for a in &about {
            let known = idx.documents.iter().any(|d| &d.id == a)
                || idx.records.iter().any(|r| &r.id == a)
                || idx.studies.iter().any(|s| &s.id == a);
            if !known {
                return Err(CliError::NotFound(format!(
                    "{} has no document, record or study `{a}`",
                    person.name
                )));
            }
        }
    }
    let mut notes = folder.notes(person)?;
    let note = Note {
        id: crate::ids::new("n"),
        date,
        author,
        text: text.to_string(),
        tags,
        about,
    };
    notes.push(note.clone());
    folder.save_notes(person, &notes)?;
    Ok(note)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, date: &str, text: &str) -> Note {
        Note {
            id: id.into(),
            date: date.into(),
            author: Some("Alex".into()),
            text: text.into(),
            tags: vec!["question".into()],
            about: vec!["d_aaaaaaaaaaaaaaaaaaaa".into()],
        }
    }

    #[test]
    fn notes_round_trip_through_markdown() {
        let notes = vec![
            note(
                "n_bbbbbbbbbbbbbbbbbbbb",
                "2026-03-05",
                "Second.\n## not a heading\n\\ backslash",
            ),
            note(
                "n_aaaaaaaaaaaaaaaaaaaa",
                "2026-03-04",
                "First line\n\nsecond paragraph",
            ),
        ];
        let md = render("Robin", &notes);
        assert!(md.starts_with("# Notes: Robin\n\n## 2026-03-04 (n_aaaaaaaaaaaaaaaaaaaa)\n"));
        let back = parse(&md);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].text, "First line\n\nsecond paragraph");
        assert_eq!(back[1].text, "Second.\n## not a heading\n\\ backslash");
        assert_eq!(back[1].tags, ["question"]);
        assert_eq!(back[1].author.as_deref(), Some("Alex"));
    }

    #[test]
    fn a_note_written_by_hand_gets_a_stable_id() {
        let md = "# Notes\n\n## 2026-01-02\nCalled the clinic.\n";
        let a = parse(md);
        assert_eq!(a.len(), 1);
        assert!(a[0].id.starts_with("n_"));
        assert_eq!(a[0].id, parse(md)[0].id);
        assert_eq!(a[0].text, "Called the clinic.");
    }
}
