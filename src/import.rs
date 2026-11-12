//! Provider and insurer import (PLAN §7): run `<cli> records export
//! --patient <id> --ndjson` as a bounded subprocess, read its
//! `records-stream/v1` stream, and file it.
//!
//! Every resource is written verbatim, one file each, under
//! `<Person>/Records/<source>/<ResourceType>/<id>.json`; a resource that
//! changed at the source keeps its previous version beside it as
//! `<id>~<md5 prefix>.json`. A `Binary` document is also filed as a document
//! under `Documents/` (or `Claims/` for an EOB). Nothing is written unless
//! the stream ends with its trailer, the trailer says it is complete and its
//! counts match, and the provider exits 0.
//!
//! Providers are found by profile, not by name: any CLI whose `info --json`
//! lists `health-records/v1` is one.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, Read};
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use md5::Md5;
use pk_cli_core::CliError;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::folder::{self, Folder};
use crate::hash;
use crate::ids;
use crate::intake::{self, classify, filetype, text};
use crate::model::{
    filed_by, now_rfc3339, Document, Household, Identifier, ImportRun, Organization, Origin,
    Person, PersonIndex, PersonText, Provenance, ProvenanceSource, Record, Source, SourceResource,
};
use crate::normalize;
use crate::people;
use crate::proc::{which, Bounded};
use crate::stream::{self, Header, Item, RawResource, RecordsReader, Skip};

/// Provider CLIs medrec looks for without being told.
pub const KNOWN_PROVIDERS: &[&str] = &["mychart", "kp", "athena", "uhc"];
const INFO_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
pub struct Provider {
    pub cli: String,
    #[serde(skip)]
    pub path: PathBuf,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Provider {
    pub fn usable(&self) -> bool {
        self.status == "ok"
    }
}

/// Ask `<cli> info --json` what it is. `info` is the one command a family
/// CLI answers without reading a credential.
pub fn probe(cli: &str) -> Provider {
    let mut p = Provider {
        cli: cli.into(),
        path: PathBuf::new(),
        status: "not-installed".into(),
        version: None,
        profiles: Vec::new(),
        note: None,
    };
    let Some(path) = which(cli) else {
        return p;
    };
    p.path = path.clone();
    let r = match Bounded::new(&path)
        .args(["info", "--json"])
        .timeout(INFO_TIMEOUT)
        .max_stdout(1024 * 1024)
        .run()
    {
        Ok(r) => r,
        Err(e) => {
            p.status = "no-answer".into();
            p.note = Some(e.to_string());
            return p;
        }
    };
    if r.code != Some(0) {
        p.status = "error".into();
        p.note = Some(format!("`{cli} info --json` exited {}", exit_text(r.code)));
        return p;
    }
    let v: Value = match serde_json::from_slice(&r.stdout) {
        Ok(v) => v,
        Err(_) => {
            p.status = "error".into();
            p.note = Some("`info --json` did not print JSON".into());
            return p;
        }
    };
    if v.get("schema").and_then(Value::as_str) != Some("cli-info/v1") {
        p.status = "error".into();
        p.note = Some("`info --json` is not cli-info/v1".into());
        return p;
    }
    p.version = v.get("version").and_then(Value::as_str).map(str::to_string);
    p.profiles = v
        .get("profiles")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    p.status = if p.profiles.iter().any(|x| x == stream::PROFILE) {
        "ok".into()
    } else {
        p.note = Some(format!("does not declare {}", stream::PROFILE));
        "not-a-provider".into()
    };
    p
}

/// Every provider medrec knows of: the known names and the configured ones.
pub fn providers(cfg: &Config) -> Vec<Provider> {
    let mut names: Vec<String> = KNOWN_PROVIDERS.iter().map(|s| s.to_string()).collect();
    for n in cfg.extra_providers() {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names.iter().map(|n| probe(n)).collect()
}

pub struct Request {
    pub from: String,
    pub patient: String,
    pub person: Option<String>,
    pub since: Option<String>,
    pub org: Option<String>,
    pub allow_birth_date_mismatch: bool,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub import: String,
    pub person: String,
    pub person_name: String,
    pub cli: String,
    pub cli_version: String,
    pub organization: String,
    pub patient_id: String,
    pub source_dir: String,
    pub resources: u64,
    pub counts: BTreeMap<String, u64>,
    pub skipped: usize,
    pub resources_written: usize,
    pub resources_unchanged: usize,
    pub records_added: usize,
    pub records_updated: usize,
    pub records_unchanged: usize,
    pub documents_added: usize,
    pub documents_duplicate: usize,
    pub stream_bytes: u64,
}

/// The provider's bytes, hashed on the way through.
struct Tee<R: Read> {
    inner: R,
    hasher: Sha256,
    bytes: u64,
    /// `import_max_mb`: a stream past this is refused, since an import
    /// holds every resource until the trailer.
    max: u64,
}

impl<R: Read> Read for Tee<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        if self.bytes > self.max {
            return Err(std::io::Error::other(format!(
                "the stream is larger than import_max_mb ({} MiB); raise it with \
                 `medrec config set import_max_mb <MiB>` or import with --since",
                self.max / (1024 * 1024)
            )));
        }
        Ok(n)
    }
}

struct Binary {
    id: String,
    content_type: Option<String>,
    bytes: Vec<u8>,
}

struct DocRef {
    id: String,
    version: Option<String>,
    last_updated: Option<String>,
    type_code: Option<String>,
    type_text: Option<String>,
    date: Option<String>,
    title: Option<String>,
    /// `content[].attachment.url`: how the stream ties a Binary to it.
    urls: Vec<String>,
}

struct Streamed {
    header: Header,
    trailer: stream::Trailer,
    resources: Vec<RawResource>,
    binaries: Vec<Binary>,
    skipped: Vec<Skip>,
}

pub fn run(
    folder: &Folder,
    cfg: &Config,
    provider: &Provider,
    req: &Request,
) -> Result<Report, CliError> {
    let mut household = folder.household()?;
    let person = resolve_person(&household, req)?.clone();
    let max_binary = cfg.max_file_bytes();

    let mut cmd = Bounded::new(&provider.path)
        .args([
            "records",
            "export",
            "--patient",
            req.patient.as_str(),
            "--ndjson",
        ])
        .timeout(cfg.import_timeout())
        .max_stdout(usize::MAX);
    if let Some(s) = &req.since {
        cmd = cmd.args(["--since", s.as_str()]);
    }
    if let Some(o) = &req.org {
        cmd = cmd.args(["--org", o.as_str()]);
    }
    let mut child = cmd.stream()?;
    let mut tee = Tee {
        inner: child.stdout(),
        hasher: Sha256::new(),
        bytes: 0,
        max: cfg.import_max_bytes(),
    };
    // Lines stay within records-stream/v1's 64 MiB: a provider skips a
    // larger resource as too_large, so a Binary over about 48 MiB never
    // arrives, whatever max_file_mb says.
    let mut reader = RecordsReader::new(BufReader::with_capacity(256 * 1024, &mut tee));
    let read = read_stream(&mut reader, max_binary);
    let streamed = match read {
        Ok(v) => v,
        Err(e) => {
            let finished = child.finish_within(Duration::from_secs(5));
            return Err(stream_failure(&req.from, e, finished, req.org.as_deref()));
        }
    };
    let (stream_sha, stream_bytes) = (hash::hex(&tee.hasher.finalize()), tee.bytes);
    let finished = child.finish()?;
    if finished.status.code() != Some(0) {
        return Err(provider_exit(
            &req.from,
            finished.status.code(),
            &finished.stderr_tail,
            req.org.as_deref(),
        ));
    }
    let Streamed {
        header,
        trailer,
        resources,
        binaries,
        skipped: skipped_large,
    } = streamed;

    // A stream about someone else must not land in this person's records.
    let source_birth = resources
        .iter()
        .filter(|r| r.resource_type == "Patient")
        .filter_map(|r| r.value().ok())
        .find_map(|v| {
            v.get("birthDate")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if let (Some(ours), Some(theirs)) = (&person.birth_date, &source_birth) {
        if ours != theirs && !req.allow_birth_date_mismatch {
            return Err(CliError::Usage(format!(
                "the source's patient has a different birth date than {} in medrec; check --person \
                 (or pass --allow-birth-date-mismatch to import anyway). Nothing was written.",
                person.name
            )));
        }
    }

    let source = source_slug(&header.source.cli, &header.organization.id);
    let source_dir = folder::records_dir(&person, &source);
    // The provider may have run for many minutes: a household another
    // machine changed since stops the import before anything is uploaded.
    folder.household_unchanged(&household)?;
    let mut index = folder.index(&person)?;
    let written = write_raw(folder, &source_dir, &resources)?;

    let mut texts = folder.text(&person)?;
    let imported_at = now_rfc3339();
    let portal = header.method.as_deref() == Some("portal");
    let base_prov = Provenance {
        source: ProvenanceSource {
            kind: if portal { "portal" } else { "fhir" }.into(),
            cli: Some(header.source.cli.clone()),
            cli_version: Some(header.source.version.clone()),
            organization_id: Some(header.organization.id.clone()),
            fhir_base: Some(
                header
                    .organization
                    .fhir_base
                    .trim_end_matches('/')
                    .to_string(),
            ),
        },
        method: header.method.clone().unwrap_or_else(|| "fhir-api".into()),
        source_resource: None,
        fetched_at: Some(header.pulled_at.clone()),
        filed_at: imported_at.clone(),
        raw_path: None,
        classified_by: None,
        derived_from: None,
        filed_by: filed_by(),
    };

    let mut docrefs = Vec::new();
    let (mut added, mut updated, mut unchanged) = (0, 0, 0);
    for r in &resources {
        let Ok(v) = r.value() else { continue };
        if r.resource_type == "DocumentReference" {
            docrefs.push(docref(&v));
        }
        let mut prov = base_prov.clone();
        prov.source_resource = source_resource(&v);
        prov.raw_path = written
            .paths
            .get(&(r.resource_type.clone(), r.id.clone()))
            .cloned();
        if let Some(rec) = normalize::record(&v, prov) {
            match merge(&mut index, rec) {
                Merge::Added => added += 1,
                Merge::Updated => updated += 1,
                Merge::Unchanged => unchanged += 1,
            }
        }
    }
    let eob_docs = eob_document_links(&resources);

    let (docs_added, docs_dup) = file_binaries(
        folder,
        cfg,
        &household,
        &mut index,
        &mut texts,
        &person,
        &header,
        &base_prov,
        binaries,
        &docrefs,
        &eob_docs,
        &written.paths,
    )?;

    let counts = trailer.counts.clone();
    let total: u64 = counts.values().sum();
    let import_id = ids::new("i");
    let mut skipped = trailer.skipped.clone();
    skipped.extend(skipped_large);
    index.imports.push(ImportRun {
        id: import_id.clone(),
        source_dir: source_dir.clone(),
        stream_sha256: stream_sha,
        stream_bytes,
        cli: header.source.cli.clone(),
        cli_version: header.source.version.clone(),
        organization: header.organization.id.clone(),
        patient_id: header.patient.id.clone(),
        pulled_at: header.pulled_at.clone(),
        imported_at,
        counts: counts.clone(),
        skipped: skipped.clone(),
        records_added: added,
        records_updated: updated,
        records_unchanged: unchanged,
        resources_written: written.written,
        resources_unchanged: written.unchanged,
        documents_added: docs_added,
    });
    folder.save_index(&person, &mut index)?;
    folder.save_text(&person, &texts)?;

    record_source(&mut household, &person.id, &req.from, &header, portal);
    folder.save_household(&mut household)?;

    Ok(Report {
        import: import_id,
        person: person.id.clone(),
        person_name: person.name.clone(),
        cli: header.source.cli,
        cli_version: header.source.version,
        organization: header.organization.name,
        patient_id: header.patient.id,
        source_dir,
        resources: total,
        counts,
        skipped: skipped.len(),
        resources_written: written.written,
        resources_unchanged: written.unchanged,
        records_added: added,
        records_updated: updated,
        records_unchanged: unchanged,
        documents_added: docs_added,
        documents_duplicate: docs_dup,
        stream_bytes,
    })
}

fn resolve_person<'a>(h: &'a Household, req: &Request) -> Result<&'a Person, CliError> {
    if let Some(q) = &req.person {
        return people::resolve(h, q);
    }
    let pid = h
        .sources
        .iter()
        .find(|s| {
            s.cli.as_deref() == Some(req.from.as_str())
                && s.patient_id.as_deref() == Some(req.patient.as_str())
                && (req.org.is_none() || s.organization == req.org)
        })
        .map(|s| s.person.clone())
        .ok_or_else(|| {
            CliError::Usage(format!(
                "medrec has not imported patient `{}` from {} before; name whose records they are \
                 with --person",
                req.patient, req.from
            ))
        })?;
    h.person(&pid)
        .ok_or_else(|| CliError::Other(format!("source points at unknown person {pid}")))
}

fn read_stream<R: std::io::BufRead>(
    reader: &mut RecordsReader<R>,
    max_binary: u64,
) -> Result<Streamed, ReadFailure> {
    let header = reader.header().map_err(ReadFailure::Stream)?.clone();
    let mut resources = Vec::new();
    let mut binaries = Vec::new();
    let mut skipped = Vec::new();
    let trailer = loop {
        match reader.next() {
            Some(Ok(Item::Resource(raw))) => {
                if raw.resource_type == "Binary" {
                    let v = raw
                        .value()
                        .map_err(|e| ReadFailure::Other(format!("Binary {}: {e}", raw.id)))?;
                    let data: Vec<u8> = v
                        .get("data")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .bytes()
                        .filter(|b| !b.is_ascii_whitespace())
                        .collect();
                    // Its size from the base64 length, before decoding: a
                    // Binary over max_file_mb is neither decoded nor kept.
                    if (data.len() as u64 / 4) * 3 > max_binary {
                        skipped.push(Skip::resource(
                            "Binary",
                            raw.id.clone(),
                            pk_med_records::stream::skip::TOO_LARGE,
                            "larger than config max_file_mb; not kept",
                        ));
                        continue;
                    }
                    let bytes = base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .map_err(|_| {
                            ReadFailure::Other(format!("Binary {} carries no valid base64", raw.id))
                        })?;
                    if !bytes.is_empty() {
                        binaries.push(Binary {
                            id: raw.id.clone(),
                            content_type: v
                                .get("contentType")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            bytes,
                        });
                    }
                }
                resources.push(raw);
            }
            Some(Ok(Item::Trailer(t))) => break t,
            Some(Err(e)) => return Err(ReadFailure::Stream(e)),
            None => return Err(ReadFailure::Stream(stream::StreamError::MissingTrailer)),
        }
    };
    // Read to the end so a line after the trailer is refused.
    if let Some(Err(e)) = reader.next() {
        return Err(ReadFailure::Stream(e));
    }
    Ok(Streamed {
        header,
        trailer,
        resources,
        binaries,
        skipped,
    })
}

#[derive(Debug)]
enum ReadFailure {
    Stream(stream::StreamError),
    Other(String),
}

/// `mychart-nyu-langone`: the CLI and the organization, as a folder name.
pub fn source_slug(cli: &str, org: &str) -> String {
    let s = folder::slug(&format!("{cli} {org}"), 60).replace('_', "-");
    if s.is_empty() {
        "source".into()
    } else {
        s
    }
}

/// A FHIR id as a file name: ids are `[A-Za-z0-9.-]{1,64}`; anything else
/// is named by its hash.
fn id_file(id: &str) -> String {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        && !id.starts_with('.');
    if ok {
        id.to_string()
    } else {
        format!("id-{}", &hash::sha256_hex(id.as_bytes())[..32])
    }
}

struct Written {
    /// (resource type, id) → path in the records folder.
    paths: HashMap<(String, String), String>,
    written: usize,
    unchanged: usize,
}

/// Write each resource verbatim under `source_dir`, skipping those already
/// there byte for byte and keeping the previous version of those that
/// changed. One upload for the lot, then every file's MD5 checked.
fn write_raw(
    folder: &Folder,
    source_dir: &str,
    resources: &[RawResource],
) -> Result<Written, CliError> {
    let existing: HashMap<String, Option<String>> = folder
        .store
        .hashes(source_dir)?
        .into_iter()
        .map(|h| (h.rel, h.md5))
        .collect();
    let staging = pk_cli_drive::private_temp_dir("medrec-import")?;
    let mut paths = HashMap::new();
    let mut staged: Vec<(String, String)> = Vec::new();
    let mut unchanged = 0;
    let mut latest: HashMap<String, usize> = HashMap::new();
    for (i, r) in resources.iter().enumerate() {
        // A stream carries each resource once; if it repeats one, the last
        // copy wins.
        latest.insert(format!("{}/{}", r.resource_type, id_file(&r.id)), i);
    }
    for (sub, i) in &latest {
        let r = &resources[*i];
        let mut body = r.raw.clone();
        body.push('\n');
        let md5 = hash::md5_hex(body.as_bytes());
        let rel = format!("{source_dir}/{sub}.json");
        paths.insert((r.resource_type.clone(), r.id.clone()), rel.clone());
        match existing.get(&rel) {
            Some(Some(old)) if *old == md5 => {
                unchanged += 1;
                continue;
            }
            Some(old) => {
                // The current version moves aside before the new one is
                // uploaded (uploads never replace a file). When that version
                // is already kept, from an earlier change back and forth,
                // the copy at `rel` goes: its bytes are in the kept file.
                let tag = old
                    .as_deref()
                    .and_then(|m| m.get(..8))
                    .unwrap_or("prev")
                    .to_string();
                let mut kept = format!("{source_dir}/{sub}~{tag}.json");
                let mut n = 2;
                loop {
                    match existing.get(&kept) {
                        None => {
                            folder.store.move_no_clobber(&rel, &kept)?;
                            break;
                        }
                        Some(k) if old.is_some() && k == old => {
                            folder.store.delete(&rel)?;
                            break;
                        }
                        Some(_) => {
                            kept = format!("{source_dir}/{sub}~{tag}-{n}.json");
                            n += 1;
                        }
                    }
                }
            }
            None => {}
        }
        let local = staging.path().join(format!("{sub}.json"));
        if let Some(parent) = local.parent() {
            crate::store::private_dirs(parent)?;
        }
        crate::write_private(&local, body.as_bytes())?;
        staged.push((rel, md5));
    }
    if !staged.is_empty() {
        folder.store.put_tree(staging.path(), source_dir)?;
        let after: HashMap<String, Option<String>> = folder
            .store
            .hashes(source_dir)?
            .into_iter()
            .map(|h| (h.rel, h.md5))
            .collect();
        for (rel, md5) in &staged {
            match after.get(rel) {
                Some(Some(got)) if got == md5 => {}
                _ => {
                    return Err(CliError::Upstream(format!(
                        "{} did not read back with the MD5 that was sent",
                        folder.store.display_path(rel)
                    )))
                }
            }
        }
    }
    Ok(Written {
        paths,
        written: staged.len(),
        unchanged,
    })
}

fn stream_failure(
    cli: &str,
    e: ReadFailure,
    finished: Result<crate::proc::Finished, CliError>,
    org: Option<&str>,
) -> CliError {
    // A provider that exited nonzero says why better than a cut stream.
    match finished {
        Ok(f) if f.status.code() != Some(0) => {
            provider_exit(cli, f.status.code(), &f.stderr_tail, org)
        }
        Err(timeout) => timeout,
        _ => {
            let why = match e {
                ReadFailure::Stream(s) => s.to_string(),
                ReadFailure::Other(s) => s,
            };
            CliError::Upstream(format!(
                "{cli}'s records stream was refused: {why}. Nothing was written."
            ))
        }
    }
}

/// A provider's exit code, read as the family contract defines it.
pub fn provider_exit(cli: &str, code: Option<i32>, stderr: &str, org: Option<&str>) -> CliError {
    let why = stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let why = if why.is_empty() {
        String::new()
    } else {
        format!(" ({why})")
    };
    let login = match org {
        Some(o) => format!("{cli} auth login --org {o}"),
        None => format!("{cli} auth login"),
    };
    match code {
        Some(3) => CliError::Auth(format!(
            "{cli} needs a sign-in: run `{login}`, then import again{why}"
        )),
        Some(4) => CliError::NotFound(format!("{cli} does not know that patient{why}")),
        Some(5) => CliError::Upstream(format!(
            "{cli}'s provider did not answer; try again later{why}"
        )),
        Some(2) => CliError::Other(format!(
            "{cli} rejected the export arguments medrec sent{why}"
        )),
        other => CliError::Upstream(format!(
            "{cli} exited {}{why}; nothing was written",
            exit_text(other)
        )),
    }
}

fn exit_text(code: Option<i32>) -> String {
    code.map(|c| c.to_string())
        .unwrap_or_else(|| "on a signal".into())
}

fn source_resource(r: &Value) -> Option<SourceResource> {
    Some(SourceResource {
        resource_type: r.get("resourceType")?.as_str()?.into(),
        id: r.get("id").and_then(Value::as_str).unwrap_or("").into(),
        version_id: normalize::path_str(r, "meta.versionId").map(str::to_string),
        last_updated: normalize::path_str(r, "meta.lastUpdated").map(str::to_string),
    })
}

enum Merge {
    Added,
    Updated,
    Unchanged,
}

/// The same source resource updates its record; the same fact from another
/// source joins it with a second provenance; anything else is new.
fn merge(index: &mut PersonIndex, rec: Record) -> Merge {
    let incoming = rec.provenance[0].clone();
    let same_resource = |p: &Provenance| {
        p.source.fhir_base == incoming.source.fhir_base
            && p.source_resource
                .as_ref()
                .map(|s| (&s.resource_type, &s.id))
                == incoming
                    .source_resource
                    .as_ref()
                    .map(|s| (&s.resource_type, &s.id))
    };
    if let Some(existing) = index
        .records
        .iter_mut()
        .find(|r| r.provenance.iter().any(same_resource))
    {
        let changed = existing.fields != rec.fields
            || existing.display != rec.display
            || existing.status != rec.status
            || existing.code != rec.code
            || existing.date != rec.date;
        let version_changed = existing
            .provenance
            .iter()
            .find(|p| same_resource(p))
            .map(|p| p.source_resource != incoming.source_resource)
            .unwrap_or(true);
        if !changed && !version_changed {
            return Merge::Unchanged;
        }
        existing.fields = rec.fields;
        existing.display = rec.display;
        existing.status = rec.status;
        existing.code = rec.code;
        existing.date = rec.date;
        existing.key = rec.key;
        existing.provenance.retain(|p| !same_resource(p));
        existing.provenance.push(incoming);
        return Merge::Updated;
    }
    if let Some(existing) = index.records.iter_mut().find(|r| r.key == rec.key) {
        existing.provenance.push(incoming);
        return Merge::Updated;
    }
    index.records.push(rec);
    Merge::Added
}

fn docref(v: &Value) -> DocRef {
    let concept = v
        .get("type")
        .and_then(|t| normalize::codeable(t, &[normalize::LOINC]));
    DocRef {
        id: v.get("id").and_then(Value::as_str).unwrap_or("").into(),
        version: normalize::path_str(v, "meta.versionId").map(str::to_string),
        last_updated: normalize::path_str(v, "meta.lastUpdated").map(str::to_string),
        type_code: concept.as_ref().and_then(|c| c.code.clone()),
        type_text: concept.and_then(|c| c.display),
        date: normalize::path_str(v, "date")
            .or_else(|| normalize::path_str(v, "context.period.start"))
            .map(|d| d.chars().take(10).collect()),
        title: normalize::path_str(v, "description")
            .or_else(|| normalize::path_str(v, "content.0.attachment.title"))
            .map(str::to_string),
        urls: v
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| normalize::path_str(c, "attachment.url"))
            .map(str::to_string)
            .collect(),
    }
}

/// The DocumentReference whose attachment is `Binary/<id>` (relative or
/// absolute URL).
fn docref_for<'a>(docrefs: &'a [DocRef], binary: &str) -> Option<&'a DocRef> {
    let tail = format!("Binary/{binary}");
    docrefs.iter().find(|d| {
        d.urls.iter().any(|u| {
            let u = u.trim_end_matches('/');
            u == tail || u.ends_with(&format!("/{tail}"))
        })
    })
}

/// DocumentReference ids an ExplanationOfBenefit points at
/// (`supportingInfo[].valueReference`), so those documents file as EOBs
/// under `Claims/`.
fn eob_document_links(resources: &[RawResource]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for r in resources
        .iter()
        .filter(|r| r.resource_type == "ExplanationOfBenefit")
    {
        let Ok(v) = r.value() else { continue };
        for info in v
            .get("supportingInfo")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(reference) = normalize::path_str(info, "valueReference.reference") {
                if let Some(id) = reference.strip_prefix("DocumentReference/") {
                    out.insert(id.to_string(), r.id.clone());
                }
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn file_binaries(
    folder: &Folder,
    cfg: &Config,
    household: &Household,
    index: &mut PersonIndex,
    texts: &mut PersonText,
    person: &Person,
    header: &Header,
    base: &Provenance,
    binaries: Vec<Binary>,
    docrefs: &[DocRef],
    eob_docs: &HashMap<String, String>,
    raw_paths: &HashMap<(String, String), String>,
) -> Result<(usize, usize), CliError> {
    if binaries.is_empty() {
        return Ok((0, 0));
    }
    let ctx = intake::Context::new(cfg, household);
    let (mut added, mut dup) = (0, 0);
    for b in binaries {
        let sha = hash::sha256_hex(&b.bytes);
        if index.documents.iter().any(|d| d.sha256 == sha) {
            dup += 1;
            continue;
        }
        let kind = filetype::sniff(&b.bytes).or_else(|| {
            b.content_type
                .as_deref()
                .and_then(filetype::Kind::from_media_type)
        });
        let dr = docref_for(docrefs, &b.id);
        let claim = dr.and_then(|d| eob_docs.get(&d.id));
        let doc_type = if claim.is_some() {
            "eob".to_string()
        } else {
            dr.and_then(|d| d.type_code.as_deref())
                .and_then(|c| {
                    classify::DOC_TYPES
                        .iter()
                        .find(|t| t.2 == Some(c))
                        .map(|t| t.0.to_string())
                })
                .unwrap_or_else(|| "other".into())
        };
        let date = dr
            .and_then(|d| d.date.clone())
            .unwrap_or_else(|| header.pulled_at.chars().take(10).collect());
        let ext = kind.map(|k| k.extension()).unwrap_or("bin");
        let title = dr.and_then(|d| d.title.clone().or_else(|| d.type_text.clone()));
        let name = folder::filed_name(
            &date,
            &doc_type,
            Some(&header.organization.name),
            title.as_deref(),
            &format!("{}.{ext}", id_file(&b.id)),
        );
        let dir = folder::document_dir(person, &doc_type, &date);
        let scratch = pk_cli_drive::private_temp_dir("medrec-doc")?;
        let local = scratch.path().join(format!("doc.{ext}"));
        crate::write_private(&local, &b.bytes)?;
        let md5 = {
            let mut h = Md5::new();
            h.update(&b.bytes);
            hash::hex(&h.finalize())
        };
        let path = folder.put_new_file(&local, &dir, &name, &md5)?;
        let extracted = match kind {
            Some(k) => text::extract(&ctx.tools, k, &local, &ctx.opts),
            None => text::Extracted::default(),
        };
        let mut prov = base.clone();
        prov.source_resource = Some(match dr {
            Some(d) => SourceResource {
                resource_type: "DocumentReference".into(),
                id: d.id.clone(),
                version_id: d.version.clone(),
                last_updated: d.last_updated.clone(),
            },
            None => SourceResource {
                resource_type: "Binary".into(),
                id: b.id.clone(),
                version_id: None,
                last_updated: None,
            },
        });
        prov.raw_path = raw_paths
            .get(&("Binary".to_string(), b.id.clone()))
            .cloned();
        let id = ids::new("d");
        if !extracted.text.is_empty() {
            texts.texts.insert(id.clone(), extracted.text.clone());
        }
        let media_type = kind
            .map(|k| k.media_type().to_string())
            .or_else(|| b.content_type.clone())
            .unwrap_or_else(|| "application/octet-stream".into());
        index.documents.push(Document {
            id: id.clone(),
            path,
            sha256: sha,
            md5,
            size: b.bytes.len() as u64,
            media_type,
            pages: extracted.pages,
            original_name: None,
            title,
            loinc: dr.and_then(|d| d.type_code.clone()),
            doc_type: Some(doc_type),
            date: Some(date),
            organization: Some(header.organization.name.clone()),
            filed_at: base.filed_at.clone(),
            origin: Origin {
                kind: "fhir".into(),
                name: None,
                action: "imported".into(),
            },
            text: Some(intake::text_info(&extracted)),
            near_duplicates: Vec::new(),
            minhash: intake::dupes::signature(&extracted.text),
            classification: None,
            provenance: prov,
        });
        // The claim the EOB document belongs to points back at it.
        if let Some(claim_id) = claim {
            for r in index.records.iter_mut().filter(|r| {
                r.kind == "claim"
                    && r.provenance.iter().any(|p| {
                        p.source_resource.as_ref().is_some_and(|s| {
                            s.resource_type == "ExplanationOfBenefit" && &s.id == claim_id
                        })
                    })
            }) {
                if !r.documents.contains(&id) {
                    r.documents.push(id.clone());
                }
            }
        }
        added += 1;
    }
    Ok((added, dup))
}

fn record_source(h: &mut Household, pid: &str, cli: &str, header: &Header, portal: bool) {
    let base = header
        .organization
        .fhir_base
        .trim_end_matches('/')
        .to_string();
    match h.sources.iter_mut().find(|s| {
        s.person == pid
            && s.cli.as_deref() == Some(cli)
            && s.patient_id.as_deref() == Some(header.patient.id.as_str())
            && s.organization.as_deref() == Some(header.organization.id.as_str())
    }) {
        Some(s) => s.last_pull = Some(header.pulled_at.clone()),
        None => h.sources.push(Source {
            id: ids::new("s"),
            kind: if portal { "portal" } else { "fhir" }.into(),
            person: pid.into(),
            cli: Some(cli.into()),
            organization: Some(header.organization.id.clone()),
            organization_name: Some(header.organization.name.clone()),
            fhir_base: Some(base.clone()),
            patient_id: Some(header.patient.id.clone()),
            last_pull: Some(header.pulled_at.clone()),
        }),
    }
    if !h.organizations.iter().any(|o| {
        o.fhir_base.as_deref() == Some(base.as_str()) || o.name == header.organization.name
    }) {
        h.organizations.push(Organization {
            id: ids::new("g"),
            name: header.organization.name.clone(),
            aliases: Vec::new(),
            fhir_base: Some(base),
        });
    }
    let ident = Identifier {
        source: format!("{cli}:{}", header.organization.id),
        value: header.patient.id.clone(),
        kind: Some("patient".into()),
    };
    if let Some(p) = h.person_mut(pid) {
        if !p.identifiers.contains(&ident) {
            p.identifiers.push(ident);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_over_max_file_mb_is_skipped_and_not_kept() {
        let mut r = RecordsReader::new(crate::testdata::STREAM.as_bytes());
        let all = read_stream(&mut r, u64::MAX).unwrap();
        let binaries = all
            .resources
            .iter()
            .filter(|x| x.resource_type == "Binary")
            .count();
        assert!(binaries > 0);

        let mut r = RecordsReader::new(crate::testdata::STREAM.as_bytes());
        let s = read_stream(&mut r, 1).unwrap();
        assert!(s.binaries.is_empty());
        assert!(!s.resources.iter().any(|x| x.resource_type == "Binary"));
        let too_large = s
            .skipped
            .iter()
            .filter(|k| k.code == pk_med_records::stream::skip::TOO_LARGE)
            .count();
        assert_eq!(too_large, binaries);
    }

    #[test]
    fn a_stream_past_import_max_mb_is_refused() {
        let data = [b'x'; 64];
        let mut t = Tee {
            inner: &data[..],
            hasher: Sha256::new(),
            bytes: 0,
            max: 32,
        };
        let mut out = Vec::new();
        let e = std::io::Read::read_to_end(&mut t, &mut out).unwrap_err();
        assert!(e.to_string().contains("import_max_mb"));
    }

    #[test]
    fn ids_and_sources_become_safe_names() {
        assert_eq!(id_file("abc-1.2"), "abc-1.2");
        assert!(id_file("../etc").starts_with("id-"));
        assert!(id_file(".hidden").starts_with("id-"));
        assert_eq!(
            source_slug("mychart", "example-health"),
            "mychart-example-health"
        );
    }

    #[test]
    fn exit_codes_map_to_the_family_contract() {
        assert!(
            matches!(provider_exit("x", Some(3), "", Some("o")), CliError::Auth(m) if m.contains("x auth login --org o"))
        );
        assert!(matches!(
            provider_exit("x", Some(4), "", None),
            CliError::NotFound(_)
        ));
        assert!(matches!(
            provider_exit("x", Some(5), "", None),
            CliError::Upstream(_)
        ));
        assert!(
            matches!(provider_exit("x", None, "", None), CliError::Upstream(m) if m.contains("signal"))
        );
    }
}
