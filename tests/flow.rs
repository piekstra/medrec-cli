//! The whole flow, through the binary, on a local records folder with
//! synthetic data only (`medrec::testdata`): init, people, intake of scans
//! (duplicates, near-duplicates, review, the organize-scans hand-off),
//! imaging, an import from a fake provider, the views, notes, verify and
//! backup. Offline; no Keychain; rclone is not used.

use std::path::{Path, PathBuf};
use std::process::Output;

use assert_cmd::Command;
use serde_json::Value;

struct Env {
    root: PathBuf,
    path: String,
}

impl Env {
    fn new(tag: &str) -> Env {
        let root = std::env::temp_dir().join(format!("medrec-flow-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = std::env::var("PATH").unwrap_or_default();
        Env { root, path }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::cargo_bin("medrec").unwrap();
        c.env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("HOME", &self.root)
            .env("PATH", &self.path)
            .env_remove("NO_COLOR");
        for (k, v) in env {
            c.env(k, v);
        }
        c.args(args).output().unwrap()
    }

    /// `--json` run that must succeed; its document.
    fn ok(&self, args: &[&str]) -> Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let out = self.run(&a);
        assert!(
            out.status.success(),
            "medrec {args:?} exited {:?}: {}{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("one JSON document")
    }

    fn records(&self) -> PathBuf {
        self.root.join("records")
    }
}

fn has(tool: &str) -> bool {
    medrec::proc::which(tool).is_some()
}

fn ids_of(v: &Value) -> Vec<String> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

fn setup(e: &Env) {
    let r = e.ok(&["init", e.records().to_str().unwrap()]);
    assert_eq!(r["status"], "created");
    assert_eq!(
        e.ok(&["init", e.records().to_str().unwrap()])["status"],
        "existing"
    );
    e.ok(&[
        "people",
        "add",
        "Alex Testperson",
        "--birth-date",
        "1990-01-01",
    ]);
    let robin = e.ok(&[
        "people",
        "add",
        "Robin Testperson",
        "--birth-date",
        "2020-02-02",
        "--guardian",
        "Alex",
    ]);
    assert_ne!(robin["adult"], true);
    assert_eq!(robin["guardians"].as_array().unwrap().len(), 1);
}

#[test]
fn init_refuses_a_folder_holding_other_files() {
    let e = Env::new("init");
    let d = e.root.join("busy");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("taxes.pdf"), b"%PDF-1.4").unwrap();
    assert_eq!(e.run(&["init", d.to_str().unwrap()]).status.code(), Some(2));
    assert!(e
        .run(&["init", d.to_str().unwrap(), "--allow-other-files"])
        .status
        .success());
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn scans_file_duplicates_are_caught_and_review_finishes_the_rest() {
    if !has("pdftotext") {
        eprintln!("skipped: pdftotext (poppler) is not installed");
        return;
    }
    let e = Env::new("scans");
    setup(&e);
    let scans = e.root.join("scans");
    medrec::testdata::write_scans(&scans);
    let f = |n: &str| scans.join(n).to_string_lossy().into_owned();
    let files = [
        "lab_robin.pdf",
        "lab_robin_copy.pdf",
        "lab_robin_rescan.pdf",
        "visit_alex.pdf",
        "eob_alex.pdf",
        "ambiguous.pdf",
    ];
    let mut args = vec!["intake".to_string()];
    args.extend(files.iter().map(|n| f(n)));
    let a: Vec<&str> = args.iter().map(String::as_str).collect();

    let dry = e.ok(&[&a[..], &["--dry-run"]].concat());
    assert_eq!(dry["counts"]["would_file"], 4, "{dry}");
    assert_eq!(dry["counts"]["duplicate"], 1);
    assert!(!e.records().join("Robin Testperson/Documents").exists());

    let r = e.ok(&a);
    assert_eq!(
        (
            r["counts"]["filed"].as_u64(),
            r["counts"]["duplicate"].as_u64(),
            r["counts"]["review"].as_u64()
        ),
        (Some(4), Some(1), Some(1)),
        "{r}"
    );
    let item = |src: &str| {
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["source"] == src)
            .unwrap()
            .clone()
    };
    assert_eq!(item("lab_robin.pdf")["doc_type"], "lab_report");
    assert_eq!(item("lab_robin.pdf")["date"], "2026-03-04");
    assert_eq!(
        item("eob_alex.pdf")["path"],
        "Alex Testperson/Claims/2026/2026-02-10_eob_example_health_plan_eob_alex.pdf"
    );
    assert_eq!(
        item("lab_robin_rescan.pdf")["near_duplicates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Named files are copied, never moved, unless --move.
    assert!(scans.join("lab_robin.pdf").exists());

    // The ambiguous page: review it, then file it by hand.
    let q = ids_of(&e.ok(&["review", "list"]));
    assert_eq!(q.len(), 1);
    assert_eq!(e.run(&["review", "file", &q[0]]).status.code(), Some(2));
    let filed = e.ok(&[
        "review",
        "file",
        &q[0],
        "--person",
        "Alex",
        "--type",
        "letter",
        "--date",
        "2026-01-20",
    ]);
    assert_eq!(filed["status"], "filed");
    assert!(e.records().join(filed["path"].as_str().unwrap()).exists());
    assert!(ids_of(&e.ok(&["review", "list"])).is_empty());

    // organize-scans hands over values medrec disagrees with: held.
    let variant = e.root.join("variant.pdf");
    let mut bytes = std::fs::read(scans.join("lab_robin.pdf")).unwrap();
    bytes.extend_from_slice(b"\n% variant\n");
    std::fs::write(&variant, bytes).unwrap();
    let v = variant.to_str().unwrap();
    let held = e.ok(&[
        "intake",
        v,
        "--person",
        "Alex",
        "--type",
        "letter",
        "--date",
        "2026-01-01",
        "--classified-by",
        "organize-scans",
        "--move",
    ]);
    let i = &held["items"][0];
    assert_eq!(i["status"], "review");
    assert!(
        i["reasons"].to_string().contains(
            "organize-scans said person `Alex Testperson`; medrec found `Robin Testperson`"
        ),
        "{i}"
    );
    assert!(
        !variant.exists(),
        "a moved file goes once it is held and read back"
    );
    let q = ids_of(&e.ok(&["review", "list"]));
    assert_eq!(e.run(&["review", "drop", &q[0]]).status.code(), Some(6));
    e.ok(&["review", "drop", &q[0], "--force"]);

    // The inbox is drained: files move.
    std::fs::copy(
        scans.join("visit_alex.pdf"),
        e.records().join("inbox/scan_0002.pdf"),
    )
    .unwrap();
    let inbox = e.ok(&["intake", "--inbox"]);
    assert_eq!(inbox["items"][0]["status"], "duplicate");
    assert!(!e.records().join("inbox/scan_0002.pdf").exists());

    let v = e.ok(&["verify"]);
    assert_eq!(v["missing"].as_array().unwrap().len(), 0);
    let _ = std::fs::remove_dir_all(&e.root);
}

#[test]
fn imaging_discs_add_only_new_images() {
    let e = Env::new("imaging");
    setup(&e);
    let dicom = e.root.join("dicom");
    medrec::testdata::write_dicom(&dicom);
    let cd = |n: &str| dicom.join(n).to_string_lossy().into_owned();
    let first = e.ok(&["images", "intake", &cd("cd1")]);
    assert_eq!(first["studies"][0]["status"], "filed");
    assert_eq!(first["studies"][0]["instances_new"], 3);
    let second = e.ok(&["images", "intake", &cd("cd2")]);
    assert_eq!(second["studies"][0]["status"], "added");
    assert_eq!(
        (
            second["studies"][0]["instances_new"].as_u64(),
            second["studies"][0]["instances_existing"].as_u64()
        ),
        (Some(3), Some(3))
    );
    let again = e.ok(&["images", "intake", &cd("cd1/DICOMDIR")]);
    assert_eq!(again["studies"][0]["status"], "unchanged");
    if dicom.join("ct.zip").exists() {
        let ct = e.ok(&["images", "intake", &cd("ct.zip")]);
        assert_eq!(ct["source"], "zip");
        assert_eq!(ct["studies"][0]["person_name"], "Alex Testperson");
    }
    let list = e.ok(&["images", "list", "--person", "Robin"]);
    let study = list["items"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(list["items"][0]["instances"], 6);
    let open = e.ok(&["images", "open", &study, "--no-launch"]);
    assert_eq!(open["launched"], false);
    let opened = PathBuf::from(open["path"].as_str().unwrap());
    assert!(opened.join("S002/00003.dcm").exists());
    let out = e.root.join("export");
    let ex = e.ok(&["images", "export", &study, "-o", out.to_str().unwrap()]);
    assert_eq!(ex["files"], 7);
    assert_eq!(
        e.run(&["images", "export", &study, "-o", out.to_str().unwrap()])
            .status
            .code(),
        Some(2)
    );
    let v = e.ok(&["verify"]);
    assert!(v["study_files"].as_u64().unwrap() >= 6);
    assert!(v["unindexed"].as_array().unwrap().is_empty(), "{v}");
    let _ = std::fs::remove_dir_all(&e.root);
}

fn provider_env(e: &Env, stream: &str) -> Env {
    let dir = medrec::testdata::write_provider(&e.root.join("provider"), stream);
    Env {
        root: e.root.clone(),
        path: format!("{}:{}", dir.display(), e.path),
    }
}

#[test]
fn an_import_files_records_and_documents_and_is_idempotent() {
    let base = Env::new("import");
    setup(&base);
    let e = provider_env(&base, medrec::testdata::STREAM);
    e.ok(&["config", "set", "providers", "fakeprov"]);
    let providers = e.ok(&["providers", "list"]);
    assert!(providers["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["cli"] == "fakeprov" && p["status"] == "ok"));

    // Whose records: required the first time, and the birth date must agree.
    assert_eq!(
        e.run(&["import", "--from", "fakeprov", "--patient", "pat-0001"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        e.run(&[
            "import",
            "--from",
            "fakeprov",
            "--patient",
            "pat-0001",
            "--person",
            "Alex"
        ])
        .status
        .code(),
        Some(2)
    );
    let r = e.ok(&[
        "import",
        "--from",
        "fakeprov",
        "--patient",
        "pat-0001",
        "--person",
        "Robin",
    ]);
    assert_eq!(r["resources"], 14);
    assert_eq!(r["records_added"], 9);
    assert_eq!(r["documents_added"], 2);
    assert_eq!(r["skipped"], 1);
    let raw = e
        .records()
        .join("Robin Testperson/Records/fakeprov-example-health");
    let obs = std::fs::read_to_string(raw.join("Observation/obs-0001.json")).unwrap();
    assert!(
        medrec::testdata::STREAM.contains(obs.trim_end()),
        "kept verbatim"
    );
    assert!(
        e.records()
            .join("Robin Testperson/Claims/2026")
            .read_dir()
            .unwrap()
            .count()
            == 1
    );

    let again = e.ok(&["import", "--from", "fakeprov", "--patient", "pat-0001"]);
    assert_eq!(
        (
            again["records_unchanged"].as_u64(),
            again["resources_written"].as_u64(),
            again["documents_added"].as_u64()
        ),
        (Some(9), Some(0), Some(0))
    );

    // A result corrected at the source: updated, the old version kept.
    let changed = medrec::testdata::STREAM
        .replace("\"value\":12.5", "\"value\":12.7")
        .replace(
            "\"id\":\"obs-0001\",\"meta\":{\"versionId\":\"1\"",
            "\"id\":\"obs-0001\",\"meta\":{\"versionId\":\"2\"",
        );
    let e2 = provider_env(&base, &changed);
    let upd = e2.ok(&["import", "--from", "fakeprov", "--patient", "pat-0001"]);
    assert_eq!(
        (
            upd["records_updated"].as_u64(),
            upd["resources_written"].as_u64()
        ),
        (Some(1), Some(1)),
        "{upd}"
    );
    let kept: Vec<String> = raw
        .join("Observation")
        .read_dir()
        .unwrap()
        .map(|d| d.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert!(kept.iter().any(|k| k.starts_with("obs-0001~")));

    // A stream cut short writes nothing; a sign-in failure says how to sign in.
    let cut: String = changed.lines().take(5).map(|l| format!("{l}\n")).collect();
    let e3 = provider_env(&base, &cut);
    let before = std::fs::read_to_string(e.records().join("Robin Testperson/index.json")).unwrap();
    assert_eq!(
        e3.run(&["import", "--from", "fakeprov", "--patient", "pat-0001"])
            .status
            .code(),
        Some(5)
    );
    // A trailer that says the export was not complete: refused too.
    let partial = changed.replace("\"complete\":true", "\"complete\":false");
    assert_ne!(partial, changed);
    let e4 = provider_env(&base, &partial);
    assert_eq!(
        e4.run(&["import", "--from", "fakeprov", "--patient", "pat-0001"])
            .status
            .code(),
        Some(5)
    );
    let auth = e2.run_env(
        &[
            "import",
            "--from",
            "fakeprov",
            "--patient",
            "pat-0001",
            "--org",
            "example-health",
        ],
        &[("FAKEPROV_EXIT", "3")],
    );
    assert_eq!(auth.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&auth.stderr).contains("fakeprov auth login --org example-health")
    );
    assert_eq!(
        std::fs::read_to_string(e.records().join("Robin Testperson/index.json")).unwrap(),
        before
    );

    // The views read what the import brought.
    let tl = e.ok(&["timeline", "--person", "Robin"]);
    let kinds: Vec<&str> = tl["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["kind"].as_str().unwrap())
        .collect();
    for k in [
        "encounter",
        "result",
        "report",
        "condition",
        "medication",
        "allergy",
        "immunization",
        "claim",
        "coverage",
        "document",
    ] {
        assert!(kinds.contains(&k), "timeline lacks {k}: {kinds:?}");
    }
    assert_eq!(e.ok(&["results"])["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        e.ok(&["results", "--code", "718-7"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        e.ok(&["results", "--code", "hemoglobin"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(e.ok(&["results", "--code", "2345-7"])["items"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        e.ok(&["meds", "--active"])["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(e.ok(&["conditions"])["items"][0]["code"], "J45.909");
    assert_eq!(e.ok(&["allergies"])["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        e.ok(&["immunizations"])["items"].as_array().unwrap().len(),
        1
    );
    let claims = e.ok(&["timeline", "--kind", "claim"]);
    assert_eq!(
        claims["items"][0]["detail"],
        "billed 150.0 USD, plan paid 110.0 USD"
    );
    let hits = e.ok(&["search", "hemo"]);
    assert!(hits["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["kind"] == "result"));
    let eobs = e.ok(&["documents", "list", "--type", "eob"]);
    assert_eq!(eobs["items"].as_array().unwrap().len(), 1);
    let doc = eobs["items"][0]["id"].as_str().unwrap().to_string();
    let open = e.ok(&["documents", "open", &doc, "--no-launch"]);
    let copy = PathBuf::from(open["path"].as_str().unwrap());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(copy.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    // Notes.
    let rec = e.ok(&["results"])["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    e.ok(&[
        "note",
        "add",
        "Robin",
        "Recheck the hemoglobin at the next visit.",
        "--tag",
        "follow-up",
        "--about",
        &rec,
    ]);
    assert_eq!(
        e.run(&[
            "note",
            "add",
            "Robin",
            "x",
            "--about",
            "d_aaaaaaaaaaaaaaaaaaaa"
        ])
        .status
        .code(),
        Some(4)
    );
    let notes = e.ok(&["note", "list", "--tag", "follow-up"]);
    assert_eq!(notes["items"].as_array().unwrap().len(), 1);
    assert!(
        std::fs::read_to_string(e.records().join("Robin Testperson/notes.md"))
            .unwrap()
            .contains("Recheck the hemoglobin")
    );
    assert!(e.ok(&["search", "recheck"])["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["kind"] == "note"));

    // Verify catches a changed file; backup copies and checks everything.
    assert_eq!(
        e.ok(&["verify", "--deep"])["changed"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let eob_path = e.records().join(eobs["items"][0]["path"].as_str().unwrap());
    let original = std::fs::read(&eob_path).unwrap();
    std::fs::write(&eob_path, b"%PDF-1.4 tampered").unwrap();
    let bad = e.run(&["--json", "verify"]);
    assert_eq!(bad.status.code(), Some(1));
    std::fs::write(&eob_path, original).unwrap();
    let b = e.ok(&["backup", "--to", e.root.join("backup").to_str().unwrap()]);
    assert_eq!(b["files"], b["verified"]);
    assert!(b["mismatched"].as_array().unwrap().is_empty());
    let shown = e.ok(&["people", "show", "Robin"]);
    assert_eq!(shown["documents"], 2);
    assert_eq!(shown["sources"].as_array().unwrap().len(), 1);
    let _ = std::fs::remove_dir_all(Path::new(&e.root));
}

#[test]
fn a_local_folder_is_owner_only_and_lists_take_a_limit() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new("modes");
    setup(&e);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&e.records()), 0o700);
    assert_eq!(mode(&e.records().join("household.json")), 0o600);

    assert_eq!(
        e.ok(&["people", "list"])["items"].as_array().unwrap().len(),
        2
    );
    let one = e.ok(&["people", "list", "--limit", "1"]);
    assert_eq!(one["items"].as_array().unwrap().len(), 1);

    let folder_mode = |e: &Env| -> String {
        let out = e.run(&["--json", "doctor"]);
        let v: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["check"] == "folder mode")
            .map(|c| c["status"].as_str().unwrap().to_string())
            .expect("a folder mode check")
    };
    assert_eq!(folder_mode(&e), "ok");
    std::fs::set_permissions(e.records(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(folder_mode(&e), "warn");
    let _ = std::fs::remove_dir_all(Path::new(&e.root));
}
