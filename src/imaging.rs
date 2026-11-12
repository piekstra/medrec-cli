//! Imaging studies (PLAN §6a): DICOM from a CD, a folder, a zip or an ISO,
//! kept byte for byte under `<Person>/Imaging/<date>_<modality>_<body
//! part>_<facility>/` with a `study.json`.
//!
//! A study is identified by its StudyInstanceUID and an instance by its
//! SOPInstanceUID, so a second copy of a study (the same CD twice, or a
//! later disc with more series) adds only the instances not already filed.
//! Patient fields are read to tell whose study it is and never written to
//! the index.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};

use crate::dicom::{self, Header};
use crate::folder::{self, Folder};
use crate::ids;
use crate::model::{
    filed_by, now_rfc3339, Household, ImagingSeries, ImagingStudy, Person, PersonIndex, Provenance,
    ProvenanceSource, STUDY_SCHEMA,
};
use crate::people;
use crate::proc::{which, Bounded};
use crate::store;

/// Files read from one source; a CD with its viewer software is far less.
const MAX_FILES: usize = 500_000;
const MAX_DEPTH: usize = 32;
/// What an archive may expand to.
const MAX_EXPANDED: u64 = 64 * 1024 * 1024 * 1024;
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(3600);
const MEDIA_STORAGE_DIRECTORY: &str = "1.2.840.10008.1.3.10";
pub const STUDY_FILE: &str = "study.json";

pub struct IntakeRequest {
    pub path: PathBuf,
    pub person: Option<String>,
    pub allow_birth_date_mismatch: bool,
    pub dry_run: bool,
    pub interactive: bool,
}

#[derive(Debug, Serialize)]
pub struct IntakeReport {
    pub dry_run: bool,
    /// `folder`, `dicomdir`, `zip` or `iso`.
    pub source: String,
    pub files_seen: usize,
    pub dicom_files: usize,
    /// Files that are not DICOM (a CD's viewer, a PDF report): not filed.
    pub other_files: usize,
    /// DICOM files medrec could not read (a deflated or big-endian set).
    pub unreadable: usize,
    /// Files a DICOMDIR lists that are not on the media.
    #[serde(skip_serializing_if = "is_zero")]
    pub dicomdir_missing: usize,
    /// Non-DICOM documents found (`.pdf`): file them with `medrec intake`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub documents_found: Vec<String>,
    pub studies: Vec<StudyOutcome>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Serialize)]
pub struct StudyOutcome {
    /// `filed` (new study), `added` (new instances to a filed study),
    /// `unchanged`, `would_file`, `would_add`, `skipped`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub study: Option<String>,
    pub study_uid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub modalities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_part: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facility: Option<String>,
    pub series: usize,
    pub instances_new: usize,
    pub instances_existing: usize,
    pub bytes_new: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `study.json`: the index's record of the study, beside its files.
#[derive(Debug, Serialize, Deserialize)]
pub struct StudyFile {
    pub schema: String,
    pub person: String,
    #[serde(flatten)]
    pub study: ImagingStudy,
}

struct Instance {
    file: PathBuf,
    size: u64,
    header: Header,
}

/// The source, unpacked if it is an archive. The temp dir is removed on drop.
struct Source {
    root: PathBuf,
    kind: &'static str,
    _unpacked: Option<pk_cli_drive::TempDir>,
}

pub fn intake(folder: &Folder, req: &IntakeRequest) -> Result<IntakeReport, CliError> {
    let src = open_source(&req.path)?;
    let scan = scan(&src.root)?;
    let mut household = folder.household()?;
    let mut by_study: BTreeMap<String, Vec<Instance>> = BTreeMap::new();
    for i in scan.instances {
        let uid = i.header.study_uid().unwrap_or_default().to_string();
        by_study.entry(uid).or_default().push(i);
    }

    // Every person's index, once: a study already filed may be anyone's.
    let mut indexes: HashMap<String, PersonIndex> = HashMap::new();
    for p in &household.people {
        indexes.insert(p.id.clone(), folder.index(p)?);
    }
    let given = req
        .person
        .as_deref()
        .map(|q| people::resolve(&household, q))
        .transpose()?
        .cloned();
    let mut outcomes = Vec::new();
    let mut touched: HashSet<String> = HashSet::new();
    for (uid, instances) in by_study {
        let o = file_study(
            folder,
            &household,
            &mut indexes,
            given.as_ref(),
            req,
            &uid,
            instances,
        )?;
        if matches!(o.status.as_str(), "filed" | "added") {
            if let Some(pid) = &o.person {
                // Saved now, not after the last study: a failure on a later
                // study leaves this one indexed.
                let p = household
                    .person(pid)
                    .cloned()
                    .ok_or_else(|| CliError::Other(format!("unknown person {pid}")))?;
                let idx = indexes.get_mut(pid).expect("loaded above");
                folder.save_index(&p, idx)?;
                touched.insert(pid.clone());
            }
        }
        outcomes.push(o);
    }
    if !touched.is_empty() {
        // The household's rev moves so another machine's stale view refuses.
        folder.save_household(&mut household)?;
    }
    Ok(IntakeReport {
        dry_run: req.dry_run,
        source: src.kind.into(),
        files_seen: scan.files_seen,
        dicom_files: scan.dicom_files,
        other_files: scan.other_files,
        unreadable: scan.unreadable,
        dicomdir_missing: scan.dicomdir_missing,
        documents_found: scan.documents_found,
        studies: outcomes,
    })
}

#[allow(clippy::too_many_arguments)]
fn file_study(
    folder: &Folder,
    household: &Household,
    indexes: &mut HashMap<String, PersonIndex>,
    given: Option<&Person>,
    req: &IntakeRequest,
    uid: &str,
    instances: Vec<Instance>,
) -> Result<StudyOutcome, CliError> {
    let first = &instances[0].header;
    let mut out = StudyOutcome {
        status: "skipped".into(),
        study: None,
        study_uid: uid.into(),
        person: None,
        person_name: None,
        path: None,
        date: first.study_date(),
        modalities: Vec::new(),
        body_part: first.body_part().map(str::to_string),
        facility: first.institution().map(str::to_string),
        series: 0,
        instances_new: 0,
        instances_existing: 0,
        bytes_new: 0,
        report: None,
        reason: None,
    };
    let mut mods: Vec<String> = instances
        .iter()
        .filter_map(|i| i.header.modality().map(str::to_string))
        .collect();
    mods.sort();
    mods.dedup();
    out.modalities = mods.clone();
    let series_uids: HashSet<&str> = instances
        .iter()
        .filter_map(|i| i.header.series_uid())
        .collect();
    out.series = series_uids.len();
    if uid.is_empty() {
        out.reason = Some("these files carry no StudyInstanceUID".into());
        return Ok(out);
    }

    // Whose study: the person it is already filed under, the one named,
    // or the one person whose birth date the header carries.
    let filed_under = indexes
        .iter()
        .find(|(_, idx)| idx.studies.iter().any(|s| s.study_uid == uid))
        .map(|(pid, _)| pid.clone());
    let header_birth = first.patient_birth_date();
    let (person, classified_by) = match (&filed_under, given) {
        (Some(pid), Some(g)) if *pid != g.id => {
            out.reason = Some(format!(
                "this study is already filed under {}; it was not filed again",
                household.name_of(pid)
            ));
            return Ok(out);
        }
        (Some(pid), _) => (
            household
                .person(pid)
                .cloned()
                .expect("indexed person exists"),
            "medrec",
        ),
        (None, Some(g)) => {
            if let (Some(ours), Some(theirs)) = (&g.birth_date, &header_birth) {
                if ours != theirs && !req.allow_birth_date_mismatch {
                    out.reason = Some(format!(
                        "the study's patient has a different birth date than {} in medrec; check --person \
                         (or pass --allow-birth-date-mismatch)",
                        g.name
                    ));
                    return Ok(out);
                }
            }
            (g.clone(), "hand")
        }
        (None, None) => {
            let matches: Vec<&Person> = household
                .people
                .iter()
                .filter(|p| p.birth_date.is_some() && p.birth_date == header_birth)
                .collect();
            match matches.as_slice() {
                [one] => ((*one).clone(), "medrec"),
                _ if req.interactive => {
                    let options: Vec<(String, String)> = household
                        .people
                        .iter()
                        .map(|p| (p.id.clone(), p.name.clone()))
                        .collect();
                    let q = format!(
                        "Whose is the {} study of {}?",
                        mods.join("/"),
                        out.date.as_deref().unwrap_or("an unknown date")
                    );
                    match crate::prompt::choose(&q, &options, None)? {
                        Some(pid) => (
                            household
                                .person(&pid)
                                .cloned()
                                .expect("chosen from the list"),
                            "hand",
                        ),
                        None => {
                            out.reason = Some("skipped at the prompt".into());
                            return Ok(out);
                        }
                    }
                }
                _ => {
                    out.reason = Some(
                        "medrec cannot tell whose study this is; name them with --person".into(),
                    );
                    return Ok(out);
                }
            }
        }
    };
    out.person = Some(person.id.clone());
    out.person_name = Some(person.name.clone());

    let idx = indexes
        .get_mut(&person.id)
        .expect("every person's index is loaded");
    let existing = idx.studies.iter().position(|s| s.study_uid == uid);
    let mut study = match existing {
        Some(i) => idx.studies[i].clone(),
        None => new_study(folder, idx, &person, uid, &instances, &mods)?,
    };
    out.study = Some(study.id.clone());
    out.path = Some(study.path.clone());

    // What is new, and where each new instance goes in the study folder.
    let known: HashSet<String> = study
        .series
        .iter()
        .flat_map(|s| s.instances.keys().cloned())
        .collect();
    let mut taken: HashSet<String> = study
        .series
        .iter()
        .flat_map(|s| s.instances.values().cloned())
        .collect();
    let mut seen_now: HashSet<String> = HashSet::new();
    let mut staged: Vec<(PathBuf, String, String, u64)> = Vec::new(); // local, rel in study, sop, size
    for inst in &instances {
        let Some(sop) = inst.header.sop_instance_uid() else {
            continue;
        };
        if known.contains(sop) {
            out.instances_existing += 1;
            continue;
        }
        if !seen_now.insert(sop.to_string()) {
            continue;
        }
        let series_uid = inst.header.series_uid().unwrap_or("unknown").to_string();
        let dirs_taken = study.series_dirs_taken(&series_uid);
        let series = series_entry(&mut study, &series_uid, &inst.header);
        let dir = series_dir(series, &dirs_taken);
        let mut name = format!(
            "{dir}/{:05}.dcm",
            inst.header.instance_number().unwrap_or(0).max(0)
        );
        if inst.header.instance_number().is_none() || taken.contains(&name) {
            name = format!("{dir}/{}.dcm", folder::safe_component(sop));
        }
        taken.insert(name.clone());
        series.instances.insert(sop.to_string(), name.clone());
        staged.push((inst.file.clone(), name, sop.to_string(), inst.size));
        out.instances_new += 1;
        out.bytes_new += inst.size;
    }
    if staged.is_empty() {
        out.status = "unchanged".into();
        out.report = study.report.clone();
        return Ok(out);
    }
    if req.dry_run {
        out.status = if existing.is_some() {
            "would_add"
        } else {
            "would_file"
        }
        .into();
        return Ok(out);
    }

    let space = folder.store.space().unwrap_or(store::Space {
        total: None,
        free: None,
    });
    if space.free.is_some_and(|f| f < out.bytes_new) {
        return Err(CliError::Upstream(format!(
            "the records folder has less free space than the study's {} new bytes",
            out.bytes_new
        )));
    }

    // Stage the new files as they will lie in the study folder, upload them
    // in one copy, then check every file's MD5.
    let staging = pk_cli_drive::private_temp_dir("medrec-study")?;
    let mut sums: Vec<(String, String)> = Vec::new();
    for (local, rel, _, _) in &staged {
        let to = staging.path().join(rel);
        store::private_dirs(to.parent().expect("a series directory"))?;
        std::fs::copy(local, &to)
            .map_err(|e| CliError::Other(format!("copying {}: {e}", local.display())))?;
        sums.push((format!("{}/{rel}", study.path), store::md5_file(&to)?));
    }
    // A stale view of the index must not file anything.
    folder.index_unchanged(&person, idx)?;
    folder.store.put_tree(staging.path(), &study.path)?;
    let after: HashMap<String, Option<String>> = folder
        .store
        .hashes(&study.path)?
        .into_iter()
        .map(|h| (h.rel, h.md5))
        .collect();
    for (rel, md5) in &sums {
        if after.get(rel).and_then(|m| m.as_deref()) != Some(md5.as_str()) {
            return Err(CliError::Upstream(format!(
                "{} did not read back with the MD5 that was sent; the study's index was not changed",
                folder.store.display_path(rel)
            )));
        }
    }
    let prefix = format!("{}/", study.path);
    for (rel, md5) in sums {
        let within = rel.strip_prefix(&prefix).unwrap_or(&rel).to_string();
        study.files.insert(within, md5);
    }

    study.instances = study.series.iter().map(|s| s.instances.len()).sum();
    study.size += out.bytes_new;
    let mut all_mods: Vec<String> = study.modalities.iter().cloned().chain(mods).collect();
    all_mods.sort();
    all_mods.dedup();
    study.modalities = all_mods;
    study.provenance.push(Provenance {
        source: ProvenanceSource {
            kind: "media".into(),
            cli: None,
            cli_version: None,
            organization_id: None,
            fhir_base: None,
        },
        method: "media".into(),
        source_resource: None,
        fetched_at: None,
        filed_at: now_rfc3339(),
        raw_path: Some(study.path.clone()),
        classified_by: Some(classified_by.into()),
        derived_from: None,
        filed_by: filed_by(),
    });
    match existing {
        Some(i) => idx.studies[i] = study,
        None => idx.studies.push(study),
    }
    link_reports(idx);
    let study = idx
        .studies
        .iter()
        .find(|s| s.study_uid == uid)
        .expect("just stored")
        .clone();
    folder.write_json(
        &format!("{}/{STUDY_FILE}", study.path),
        &StudyFile {
            schema: STUDY_SCHEMA.into(),
            person: person.id.clone(),
            study: study.clone(),
        },
    )?;
    out.status = if existing.is_some() { "added" } else { "filed" }.into();
    out.report = study.report;
    Ok(out)
}

trait SeriesDirs {
    fn series_dirs_taken(&self, except_uid: &str) -> HashSet<String>;
}

impl SeriesDirs for ImagingStudy {
    /// The series folders other series already use.
    fn series_dirs_taken(&self, except_uid: &str) -> HashSet<String> {
        self.series
            .iter()
            .filter(|s| s.uid != except_uid)
            .filter_map(|s| s.instances.values().next())
            .filter_map(|f| f.split('/').next().map(str::to_string))
            .collect()
    }
}

fn series_entry<'a>(study: &'a mut ImagingStudy, uid: &str, h: &Header) -> &'a mut ImagingSeries {
    if let Some(i) = study.series.iter().position(|s| s.uid == uid) {
        return &mut study.series[i];
    }
    study.series.push(ImagingSeries {
        uid: uid.into(),
        number: h.series_number(),
        modality: h.modality().map(str::to_string),
        description: h.series_description().map(str::to_string),
        instances: BTreeMap::new(),
    });
    study.series.last_mut().expect("just pushed")
}

/// `S001` from the series number; `S001_2` when another series has it.
fn series_dir(series: &ImagingSeries, taken: &HashSet<String>) -> String {
    if let Some(dir) = series
        .instances
        .values()
        .next()
        .and_then(|f| f.split('/').next())
    {
        return dir.to_string();
    }
    let base = format!("S{:03}", series.number.unwrap_or(0).clamp(0, 99_999));
    if !taken.contains(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}_{n}"))
        .find(|d| !taken.contains(d))
        .expect("an unbounded range finds a free name")
}

fn new_study(
    folder: &Folder,
    idx: &PersonIndex,
    person: &Person,
    uid: &str,
    instances: &[Instance],
    mods: &[String],
) -> Result<ImagingStudy, CliError> {
    let h = &instances[0].header;
    let date = h.study_date();
    let name = study_folder_name(date.as_deref(), mods, h.body_part(), h.institution());
    // Taken in the index, or in the folder itself: another machine's study,
    // or what a stopped run left, is never shared.
    let taken: HashSet<&str> = idx.studies.iter().map(|s| s.path.as_str()).collect();
    let mut path = folder::imaging_dir(person, &name);
    let mut n = 2;
    while taken.contains(path.as_str()) || folder.store.exists(&path)? {
        path = folder::imaging_dir(person, &format!("{name}_{n}"));
        n += 1;
    }
    Ok(ImagingStudy {
        id: ids::new("m"),
        study_uid: uid.into(),
        path,
        date,
        modalities: Vec::new(),
        body_part: h.body_part().map(str::to_string),
        description: h.study_description().map(str::to_string),
        facility: h.institution().map(str::to_string),
        accession: h.accession().map(str::to_string),
        referring: h.referring_physician(),
        series: Vec::new(),
        instances: 0,
        size: 0,
        report: None,
        files: BTreeMap::new(),
        filed_at: now_rfc3339(),
        provenance: Vec::new(),
    })
}

/// `2026-03-04_us_abdomen_example_imaging`.
pub fn study_folder_name(
    date: Option<&str>,
    mods: &[String],
    body_part: Option<&str>,
    facility: Option<&str>,
) -> String {
    let mut parts = vec![date.unwrap_or("undated").to_string()];
    let m = folder::slug(&mods.join(" "), 20);
    parts.push(if m.is_empty() { "study".into() } else { m });
    for p in [body_part, facility].into_iter().flatten() {
        let s = folder::slug(p, 40);
        if !s.is_empty() {
            parts.push(s);
        }
    }
    parts.join("_")
}

/// Link each study without a report to the one radiology report filed for
/// the same person on the same date, when there is exactly one.
pub fn link_reports(idx: &mut PersonIndex) -> usize {
    let mut linked = 0;
    let reports: Vec<(String, Option<String>)> = idx
        .documents
        .iter()
        .filter(|d| d.doc_type.as_deref() == Some("imaging_report"))
        .map(|d| (d.id.clone(), d.date.clone()))
        .collect();
    for s in idx.studies.iter_mut().filter(|s| s.report.is_none()) {
        let same_day: Vec<&String> = reports
            .iter()
            .filter(|(_, d)| d.is_some() && *d == s.date)
            .map(|(id, _)| id)
            .collect();
        if let [one] = same_day.as_slice() {
            s.report = Some((*one).clone());
            linked += 1;
        }
    }
    linked
}

fn open_source(path: &Path) -> Result<Source, CliError> {
    let md = std::fs::metadata(path)
        .map_err(|e| CliError::NotFound(format!("{}: {e}", path.display())))?;
    if md.is_dir() {
        return Ok(Source {
            root: path.to_path_buf(),
            kind: "folder",
            _unpacked: None,
        });
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name == "dicomdir" {
        return Ok(Source {
            root: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            kind: "dicomdir",
            _unpacked: None,
        });
    }
    let mut magic = [0u8; 4];
    let _ = std::fs::File::open(path).and_then(|mut f| std::io::Read::read(&mut f, &mut magic));
    let kind = if &magic == b"PK\x03\x04" || name.ends_with(".zip") {
        "zip"
    } else if name.ends_with(".iso") {
        "iso"
    } else {
        return Err(CliError::Usage(format!(
            "{} is not a folder, a DICOMDIR, a .zip or an .iso",
            path.display()
        )));
    };
    let tar = bsdtar().ok_or_else(|| {
        CliError::Other(
            "unpacking an archive needs bsdtar (macOS has it; on Linux install libarchive-tools)"
                .into(),
        )
    })?;
    let expanded = archive_size(&tar, path)?;
    if expanded > MAX_EXPANDED {
        return Err(CliError::Usage(format!(
            "{} would expand to {expanded} bytes, more than medrec unpacks ({MAX_EXPANDED})",
            path.display()
        )));
    }
    let tmp = pk_cli_drive::private_temp_dir("medrec-media")?;
    if store::statvfs_space(tmp.path())
        .free
        .is_some_and(|f| f < expanded)
    {
        return Err(CliError::Other(
            "not enough free space in the temp directory to unpack it".into(),
        ));
    }
    // bsdtar refuses absolute paths, `..` and extracting through a symlink
    // unless told otherwise, and keeps no owner.
    let r = Bounded::new(&tar)
        .arg("-xf")
        .arg(path)
        .arg("-C")
        .arg(tmp.path())
        .args(["--no-same-owner", "--no-same-permissions"])
        .timeout(EXTRACT_TIMEOUT)
        .max_stdout(64 * 1024)
        .run()?;
    if r.code != Some(0) {
        return Err(CliError::Other(format!(
            "unpacking {} failed: {}",
            path.display(),
            r.stderr_tail
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("bsdtar failed")
        )));
    }
    Ok(Source {
        root: tmp.path().to_path_buf(),
        kind,
        _unpacked: Some(tmp),
    })
}

fn bsdtar() -> Option<PathBuf> {
    which("bsdtar").or_else(|| {
        if cfg!(target_os = "macos") {
            Some(PathBuf::from("/usr/bin/tar"))
        } else {
            None
        }
    })
}

/// The archive's expanded size, from `bsdtar -tv`'s size column.
fn archive_size(tar: &Path, path: &Path) -> Result<u64, CliError> {
    let r = Bounded::new(tar)
        .arg("-tvf")
        .arg(path)
        .timeout(Duration::from_secs(600))
        .max_stdout(256 * 1024 * 1024)
        .run()?;
    if r.code != Some(0) {
        return Err(CliError::Other(format!(
            "{} is not an archive medrec can read",
            path.display()
        )));
    }
    Ok(String::from_utf8_lossy(&r.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().nth(4)?.parse::<u64>().ok())
        .sum())
}

struct Scan {
    instances: Vec<Instance>,
    files_seen: usize,
    dicom_files: usize,
    other_files: usize,
    unreadable: usize,
    dicomdir_missing: usize,
    documents_found: Vec<String>,
}

fn scan(root: &Path) -> Result<Scan, CliError> {
    let mut s = Scan {
        instances: Vec::new(),
        files_seen: 0,
        dicom_files: 0,
        other_files: 0,
        unreadable: 0,
        dicomdir_missing: 0,
        documents_found: Vec::new(),
    };
    let mut files = Vec::new();
    walk(root, 0, &mut files)?;
    let mut dicomdirs = Vec::new();
    for f in &files {
        s.files_seen += 1;
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.eq_ignore_ascii_case("DICOMDIR") {
            dicomdirs.push(f.clone());
            continue;
        }
        if !dicom::is_dicom_file(f) {
            s.other_files += 1;
            if name.to_ascii_lowercase().ends_with(".pdf") {
                s.documents_found
                    .push(f.strip_prefix(root).unwrap_or(f).display().to_string());
            }
            continue;
        }
        s.dicom_files += 1;
        let header = std::fs::File::open(f)
            .map_err(dicom::DicomError::Io)
            .and_then(dicom::read_header);
        match header {
            Ok(h) if h.sop_class_uid() == Some(MEDIA_STORAGE_DIRECTORY) => {}
            Ok(h) if h.sop_instance_uid().is_some() => {
                let size = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
                s.instances.push(Instance {
                    file: f.clone(),
                    size,
                    header: h,
                });
            }
            _ => s.unreadable += 1,
        }
    }
    // A DICOMDIR names its files in upper case; media read on a
    // case-sensitive file system may carry them in lower case.
    let present: HashSet<String> = files
        .iter()
        .map(|f| f.to_string_lossy().to_ascii_lowercase())
        .collect();
    for d in dicomdirs {
        let base = d.parent().unwrap_or(root);
        if let Ok(entries) = dicom::read_dicomdir(&d) {
            s.dicomdir_missing += entries
                .iter()
                .filter(|e| {
                    !present.contains(&base.join(&e.path).to_string_lossy().to_ascii_lowercase())
                })
                .count();
        }
    }
    Ok(s)
}

/// Regular files under `dir`, not following symlinks.
fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<(), CliError> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| CliError::Other(format!("reading {}: {e}", dir.display())))?
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            walk(&e.path(), depth + 1, out)?;
        } else if ft.is_file() {
            if out.len() >= MAX_FILES {
                return Err(CliError::Usage(format!(
                    "more than {MAX_FILES} files under {}; point medrec at the study folder",
                    dir.display()
                )));
            }
            out.push(e.path());
        }
    }
    Ok(())
}

/// One row of `images list`.
#[derive(Debug, Serialize)]
pub struct StudyRow {
    pub id: String,
    pub person: String,
    pub person_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    pub modalities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_part: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facility: Option<String>,
    pub series: usize,
    pub instances: usize,
    pub size: u64,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
}

pub fn list(
    folder: &Folder,
    h: &Household,
    person: Option<&str>,
) -> Result<Vec<StudyRow>, CliError> {
    let mut rows = Vec::new();
    for p in people::selected(h, person)? {
        for s in folder.index(p)?.studies {
            rows.push(StudyRow {
                id: s.id,
                person: p.id.clone(),
                person_name: p.name.clone(),
                date: s.date,
                modalities: s.modalities,
                body_part: s.body_part,
                description: s.description,
                facility: s.facility,
                series: s.series.len(),
                instances: s.instances,
                size: s.size,
                path: s.path,
                report: s.report,
            });
        }
    }
    rows.sort_by(|a, b| b.date.cmp(&a.date).then(a.person_name.cmp(&b.person_name)));
    Ok(rows)
}

/// A study by its id or StudyInstanceUID, with whose it is.
pub fn find(
    folder: &Folder,
    h: &Household,
    query: &str,
) -> Result<(Person, ImagingStudy), CliError> {
    for p in &h.people {
        if let Some(s) = folder
            .index(p)?
            .studies
            .into_iter()
            .find(|s| s.id == query || s.study_uid == query)
        {
            return Ok((p.clone(), s));
        }
    }
    Err(CliError::NotFound(format!(
        "no study `{query}`; `medrec images list` shows them"
    )))
}

#[derive(Debug, Serialize)]
pub struct Exported {
    pub study: String,
    pub path: String,
    pub files: usize,
    pub bytes: u64,
}

/// Copy a study's files (and its `study.json`) into `dir`, which must be
/// new or empty, then check each file's MD5 against the store's.
pub fn export(folder: &Folder, study: &ImagingStudy, dir: &Path) -> Result<Exported, CliError> {
    if dir.exists() {
        let empty = std::fs::read_dir(dir)
            .map_err(|e| CliError::Usage(format!("{}: {e}", dir.display())))?
            .next()
            .is_none();
        if !empty {
            return Err(CliError::Usage(format!(
                "{} is not empty; name a new or empty folder",
                dir.display()
            )));
        }
    } else {
        store::private_dirs(dir)?;
    }
    folder.store.fetch_tree(&study.path, dir)?;
    // Owner-only, whatever made the copy: the study is someone's record.
    store::restrict_tree(dir)?;
    let expected = folder.store.hashes(&study.path)?;
    let (mut files, mut bytes) = (0, 0);
    for h in &expected {
        let rel = h
            .rel
            .strip_prefix(&format!("{}/", study.path))
            .unwrap_or(&h.rel);
        let local = dir.join(rel);
        let got = store::md5_file(&local)?;
        if h.md5.as_deref().is_some_and(|m| m != got) {
            return Err(CliError::Upstream(format!(
                "{} arrived with a different MD5 than the store's",
                local.display()
            )));
        }
        files += 1;
        bytes += h.size;
    }
    Ok(Exported {
        study: study.id.clone(),
        path: dir.display().to_string(),
        files,
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dicom::synthetic::SyntheticStudy;
    use crate::model::Household;
    use crate::store::Location;

    fn household_with(folder: &Folder, birth: &str) -> Person {
        let mut h = folder.household().unwrap();
        let p = Person {
            id: ids::new("p"),
            name: "Robin Testperson".into(),
            folder: "Robin Testperson".into(),
            birth_date: Some(birth.into()),
            adult: false,
            guardians: vec![],
            aliases: vec![],
            identifiers: vec![],
            links: Default::default(),
            created_at: now_rfc3339(),
        };
        h.people.push(p.clone());
        folder.save_household(&mut h).unwrap();
        p
    }

    fn new_folder(tag: &str) -> (PathBuf, Folder) {
        let root = crate::testutil::temp_dir(tag);
        let loc = Location::Dir(root.join("records"));
        let st =
            crate::store::Store::open(&loc, crate::store::Reads::Fresh, Duration::from_secs(60));
        let _: Household = folder::create(&st).unwrap();
        let f = Folder::open_at(&loc, folder::Access::Write, Duration::from_secs(60)).unwrap();
        (root, f)
    }

    fn req(path: &Path) -> IntakeRequest {
        IntakeRequest {
            path: path.into(),
            person: None,
            allow_birth_date_mismatch: false,
            dry_run: false,
            interactive: false,
        }
    }

    #[test]
    fn a_study_files_by_birth_date_and_a_second_copy_adds_only_new_instances() {
        let (root, f) = new_folder("img-intake");
        let p = household_with(&f, "2020-02-02");
        let cd1 = root.join("cd1");
        SyntheticStudy::new("US")
            .series(1)
            .instances(2)
            .seed(11)
            .write_to(&cd1)
            .unwrap();
        std::fs::write(cd1.join("viewer.exe"), b"MZ not dicom").unwrap();

        let r = intake(&f, &req(&cd1)).unwrap();
        assert_eq!(
            (
                r.files_seen,
                r.dicom_files,
                r.other_files,
                r.dicomdir_missing
            ),
            (4, 2, 1, 0)
        );
        assert_eq!(r.studies.len(), 1);
        let s = &r.studies[0];
        assert_eq!((s.status.as_str(), s.instances_new), ("filed", 2));
        assert_eq!(s.person.as_deref(), Some(p.id.as_str()));
        assert_eq!(
            s.path.as_deref(),
            Some("Robin Testperson/Imaging/2026-03-04_us_abdomen_example_imaging")
        );

        let again = intake(&f, &req(&cd1)).unwrap();
        assert_eq!(
            (
                again.studies[0].status.as_str(),
                again.studies[0].instances_existing
            ),
            ("unchanged", 2)
        );

        // A later disc of the same study with a second series.
        let cd2 = root.join("cd2");
        SyntheticStudy::new("US")
            .series(2)
            .instances(2)
            .seed(11)
            .write_to(&cd2)
            .unwrap();
        let more = intake(&f, &req(&cd2)).unwrap();
        assert_eq!(
            (
                more.studies[0].status.as_str(),
                more.studies[0].instances_new,
                more.studies[0].instances_existing
            ),
            ("added", 2, 2)
        );

        let idx = f.index(&p).unwrap();
        assert_eq!(
            (
                idx.studies.len(),
                idx.studies[0].instances,
                idx.studies[0].series.len()
            ),
            (1, 4, 2)
        );
        let study_json = std::fs::read_to_string(
            root.join("records")
                .join(&idx.studies[0].path)
                .join(STUDY_FILE),
        )
        .unwrap();
        assert!(study_json.contains(STUDY_SCHEMA) && !study_json.contains("Testperson^"));
        // Kept byte for byte.
        let filed = root
            .join("records")
            .join(&idx.studies[0].path)
            .join("S001/00001.dcm");
        assert_eq!(
            std::fs::read(filed).unwrap(),
            std::fs::read(cd1.join("IMAGES/S1/I1")).unwrap()
        );

        let out = root.join("export");
        let e = export(&f, &idx.studies[0], &out).unwrap();
        assert_eq!(e.files, 5);
        assert!(matches!(
            export(&f, &idx.studies[0], &out),
            Err(CliError::Usage(_))
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unknown_patient_is_skipped_and_a_wrong_person_is_refused() {
        let (root, f) = new_folder("img-who");
        let p = household_with(&f, "1990-01-01");
        let cd = root.join("cd");
        SyntheticStudy::new("CT").seed(12).write_to(&cd).unwrap();
        let r = intake(&f, &req(&cd)).unwrap();
        assert_eq!(r.studies[0].status, "skipped");
        assert!(r.studies[0].reason.as_deref().unwrap().contains("--person"));

        let mut named = req(&cd);
        named.person = Some(p.id.clone());
        let r = intake(&f, &named).unwrap();
        assert!(r.studies[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("birth date"));

        named.allow_birth_date_mismatch = true;
        named.dry_run = true;
        assert_eq!(intake(&f, &named).unwrap().studies[0].status, "would_file");
        assert!(f.index(&p).unwrap().studies.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_leftover_study_folder_is_not_reused_and_verify_sees_changed_bytes() {
        let (root, f) = new_folder("img-leftover");
        let p = household_with(&f, "2020-02-02");
        // What a run on another machine, or one that stopped, left behind.
        let name = "Robin Testperson/Imaging/2026-03-04_us_abdomen_example_imaging";
        let left = root.join("records").join(name).join("S001");
        std::fs::create_dir_all(&left).unwrap();
        std::fs::write(left.join("00001.dcm"), b"partial").unwrap();

        let cd = root.join("cd");
        SyntheticStudy::new("US")
            .series(1)
            .instances(2)
            .seed(11)
            .write_to(&cd)
            .unwrap();
        let r = intake(&f, &req(&cd)).unwrap();
        assert_eq!(r.studies[0].status, "filed");
        let path = format!("{name}_2");
        assert_eq!(r.studies[0].path.as_deref(), Some(path.as_str()));
        assert_eq!(std::fs::read(left.join("00001.dcm")).unwrap(), b"partial");

        let idx = f.index(&p).unwrap();
        assert_eq!(idx.studies[0].files.len(), 2);
        assert!(crate::maint::verify(&f, false).unwrap().changed.is_empty());
        let filed = root.join("records").join(&path).join("S001/00001.dcm");
        let mut bytes = std::fs::read(&filed).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&filed, bytes).unwrap();
        assert_eq!(
            crate::maint::verify(&f, false).unwrap().changed,
            vec![format!("{path}/S001/00001.dcm")]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_names_are_slugs() {
        assert_eq!(
            study_folder_name(
                Some("2026-03-04"),
                &["CT".into(), "PT".into()],
                Some("CHEST"),
                Some("Example Imaging, Inc.")
            ),
            "2026-03-04_ct_pt_chest_example_imaging_inc"
        );
        assert_eq!(study_folder_name(None, &[], None, None), "undated_study");
    }
}
