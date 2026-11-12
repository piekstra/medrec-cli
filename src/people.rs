//! Looking people up.

use pk_cli_core::{resolve, CliError};

use crate::model::{Household, Person};

/// A person by id, name, alias, folder name, or a unique part of a name.
pub fn resolve<'a>(h: &'a Household, query: &str) -> Result<&'a Person, CliError> {
    let q = query.trim();
    if let Some(p) = h.person(q) {
        return Ok(p);
    }
    if let Some(p) = h.people.iter().find(|p| {
        p.folder.eq_ignore_ascii_case(q) || p.aliases.iter().any(|a| a.eq_ignore_ascii_case(q))
    }) {
        return Ok(p);
    }
    resolve::pick(&h.people, q, |p| vec![p.id.clone()], |p| &p.name, "person")
}

/// `--person` when given, else every person.
pub fn selected<'a>(h: &'a Household, query: Option<&str>) -> Result<Vec<&'a Person>, CliError> {
    match query {
        Some(q) => Ok(vec![resolve(h, q)?]),
        None => Ok(h.people.iter().collect()),
    }
}

use std::collections::BTreeMap;

use serde::Serialize;

use crate::folder::{self, Folder};
use crate::ids;
use crate::model::{now_rfc3339, today, Identifier, Links, PersonIndex, Source};

/// An adult on `today` when born at least 18 years before it.
pub fn is_adult(birth_date: &str, today: &str) -> bool {
    let (Ok(b), Ok(t)) = (
        pk_cli_core::dates::parse_iso(birth_date),
        pk_cli_core::dates::parse_iso(today),
    ) else {
        return true;
    };
    (t.0 - b.0 > 18) || (t.0 - b.0 == 18 && (t.1, t.2) >= (b.1, b.2))
}

#[derive(Debug, Default)]
pub struct NewPerson {
    pub name: String,
    pub birth_date: Option<String>,
    /// Default: from the birth date, else adult.
    pub adult: Option<bool>,
    /// People (ids or names) who look after a child's records.
    pub guardians: Vec<String>,
    pub aliases: Vec<String>,
    pub huishouden_health_person: Option<String>,
    pub baby_profile: Option<String>,
}

fn check_name(name: &str) -> Result<String, CliError> {
    let n = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if n.is_empty() || n.chars().count() > 80 || n.chars().any(|c| c.is_control()) {
        return Err(CliError::Usage(
            "a person's name is 1 to 80 printable characters".into(),
        ));
    }
    Ok(n)
}

fn check_date(d: &str) -> Result<(), CliError> {
    pk_cli_core::dates::parse_iso(d).map(|_| ())
}

fn guardian_ids(
    h: &Household,
    queries: &[String],
    not: Option<&str>,
) -> Result<Vec<String>, CliError> {
    let mut out = Vec::new();
    for q in queries {
        let g = resolve(h, q)?;
        if Some(g.id.as_str()) == not {
            return Err(CliError::Usage(
                "a person cannot be their own guardian".into(),
            ));
        }
        if !g.adult {
            return Err(CliError::Usage(format!(
                "{} is not an adult, so cannot be a guardian",
                g.name
            )));
        }
        if !out.contains(&g.id) {
            out.push(g.id.clone());
        }
    }
    Ok(out)
}

pub fn add(folder: &Folder, np: NewPerson) -> Result<Person, CliError> {
    let mut h = folder.household()?;
    let name = check_name(&np.name)?;
    if let Some(d) = &np.birth_date {
        check_date(d)?;
    }
    if h.people
        .iter()
        .any(|p| p.name.eq_ignore_ascii_case(&name) && p.birth_date == np.birth_date)
    {
        return Err(CliError::Usage(format!(
            "{name} is already in this records folder"
        )));
    }
    let adult = np.adult.unwrap_or_else(|| {
        np.birth_date
            .as_deref()
            .is_none_or(|b| is_adult(b, &today()))
    });
    let taken: Vec<String> = h.people.iter().map(|p| p.folder.clone()).collect();
    let person = Person {
        id: ids::new("p"),
        folder: folder::person_folder(&name, &taken),
        name,
        birth_date: np.birth_date,
        adult,
        guardians: guardian_ids(&h, &np.guardians, None)?,
        aliases: np
            .aliases
            .into_iter()
            .map(|a| a.trim().to_string())
            .filter(|a| !a.is_empty())
            .collect(),
        identifiers: Vec::new(),
        links: Links {
            huishouden_health_person: np.huishouden_health_person,
            baby_profile: np.baby_profile,
        },
        created_at: now_rfc3339(),
    };
    h.people.push(person.clone());
    folder.save_household(&mut h)?;
    // The person's folder appears with their (empty) index.
    folder.save_index(&person, &mut PersonIndex::new(&person.id))?;
    Ok(person)
}

#[derive(Debug, Default)]
pub struct Edit {
    pub name: Option<String>,
    pub birth_date: Option<String>,
    pub adult: Option<bool>,
    pub add_guardians: Vec<String>,
    pub remove_guardians: Vec<String>,
    pub add_aliases: Vec<String>,
    pub remove_aliases: Vec<String>,
    /// `source=value` (`mychart:example-health=E1234`).
    pub add_identifiers: Vec<String>,
    pub remove_identifiers: Vec<String>,
    pub huishouden_health_person: Option<String>,
    pub baby_profile: Option<String>,
    pub clear_links: bool,
}

/// Change a person. Their folder keeps its name: renaming it would move
/// every file, and the folder name is only a label.
pub fn edit(folder: &Folder, query: &str, e: Edit) -> Result<Person, CliError> {
    let mut h = folder.household()?;
    let id = resolve(&h, query)?.id.clone();
    let add_g = guardian_ids(&h, &e.add_guardians, Some(&id))?;
    let remove_g: Vec<String> = e
        .remove_guardians
        .iter()
        .map(|q| resolve(&h, q).map(|p| p.id.clone()))
        .collect::<Result<_, _>>()?;
    let p = h.person_mut(&id).expect("resolved");
    if let Some(n) = &e.name {
        p.name = check_name(n)?;
    }
    if let Some(d) = &e.birth_date {
        check_date(d)?;
        p.birth_date = Some(d.clone());
    }
    if let Some(a) = e.adult {
        p.adult = a;
    }
    p.guardians.retain(|g| !remove_g.contains(g));
    for g in add_g {
        if !p.guardians.contains(&g) {
            p.guardians.push(g);
        }
    }
    p.aliases
        .retain(|a| !e.remove_aliases.iter().any(|r| r.eq_ignore_ascii_case(a)));
    for a in e
        .add_aliases
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
    {
        if !p.aliases.iter().any(|x| x.eq_ignore_ascii_case(a)) {
            p.aliases.push(a.to_string());
        }
    }
    for spec in &e.add_identifiers {
        let (source, value) = spec
            .split_once('=')
            .filter(|(s, v)| !s.trim().is_empty() && !v.trim().is_empty())
            .ok_or_else(|| CliError::Usage(format!("`{spec}` is not source=value")))?;
        let ident = Identifier {
            source: source.trim().into(),
            value: value.trim().into(),
            kind: None,
        };
        if !p
            .identifiers
            .iter()
            .any(|i| i.source == ident.source && i.value == ident.value)
        {
            p.identifiers.push(ident);
        }
    }
    p.identifiers.retain(|i| {
        !e.remove_identifiers
            .iter()
            .any(|r| *r == i.source || *r == format!("{}={}", i.source, i.value))
    });
    if e.clear_links {
        p.links = Links::default();
    }
    if let Some(v) = e.huishouden_health_person {
        p.links.huishouden_health_person = Some(v);
    }
    if let Some(v) = e.baby_profile {
        p.links.baby_profile = Some(v);
    }
    let out = p.clone();
    folder.save_household(&mut h)?;
    Ok(out)
}

#[derive(Debug, Serialize)]
pub struct Detail {
    #[serde(flatten)]
    pub person: Person,
    pub guardian_names: Vec<String>,
    pub documents: usize,
    pub records: BTreeMap<String, usize>,
    pub studies: usize,
    pub notes: usize,
    pub imports: usize,
    pub sources: Vec<Source>,
}

pub fn show(folder: &Folder, query: &str) -> Result<Detail, CliError> {
    let h = folder.household()?;
    let p = resolve(&h, query)?.clone();
    let idx = folder.index(&p)?;
    let mut records = BTreeMap::new();
    for r in &idx.records {
        *records.entry(r.kind.clone()).or_insert(0) += 1;
    }
    Ok(Detail {
        guardian_names: p.guardians.iter().map(|g| h.name_of(g)).collect(),
        documents: idx.documents.len(),
        records,
        studies: idx.studies.len(),
        notes: folder.notes(&p)?.len(),
        imports: idx.imports.len(),
        sources: h
            .sources
            .iter()
            .filter(|s| s.person == p.id)
            .cloned()
            .collect(),
        person: p,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adulthood_turns_on_the_eighteenth_birthday() {
        assert!(is_adult("2008-03-04", "2026-03-04"));
        assert!(!is_adult("2008-03-05", "2026-03-04"));
        assert!(!is_adult("2026-01-15", "2026-11-12"));
    }
}
