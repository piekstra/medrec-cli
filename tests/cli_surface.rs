//! Black-box tests of the binary's surface: offline, no Keychain, no
//! network, no records folder. Each test gets its own config, cache and
//! home directory.

use assert_cmd::Command;
use serde_json::Value;
use std::sync::atomic::{AtomicU32, Ordering};

fn medrec() -> Command {
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "medrec-surface-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = Command::cargo_bin("medrec").unwrap();
    c.env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("HOME", &dir)
        .env_remove("NO_COLOR");
    c
}

fn json_of(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON document ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn every_command_renders_its_help() {
    let commands: &[&[&str]] = &[
        &[],
        &["init"],
        &["people"],
        &["people", "add"],
        &["people", "list"],
        &["people", "show"],
        &["people", "edit"],
        &["intake"],
        &["review"],
        &["review", "list"],
        &["review", "show"],
        &["review", "file"],
        &["review", "drop"],
        &["import"],
        &["providers", "list"],
        &["timeline"],
        &["search"],
        &["results"],
        &["meds"],
        &["conditions"],
        &["allergies"],
        &["immunizations"],
        &["documents", "list"],
        &["documents", "open"],
        &["note", "add"],
        &["note", "list"],
        &["images", "intake"],
        &["images", "list"],
        &["images", "show"],
        &["images", "open"],
        &["images", "export"],
        &["doctor"],
        &["verify"],
        &["backup"],
        &["cache"],
        &["cache", "clear"],
        &["config", "path"],
        &["config", "show"],
        &["config", "set"],
        &["config", "unset"],
        &["auth", "status"],
        &["self-update"],
        &["completions"],
        &["info"],
    ];
    for c in commands {
        let mut args: Vec<&str> = c.to_vec();
        args.push("--help");
        medrec().args(&args).assert().success();
    }
}

#[test]
fn info_says_no_credential_is_needed() {
    let out = medrec().args(["info", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v = json_of(&out);
    assert_eq!(v["schema"], "cli-info/v1");
    assert_eq!(v["name"], "medrec");
    assert_eq!(v["auth"]["required"], false);
    let out = medrec()
        .args(["auth", "status", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(json_of(&out)["schema"], "auth-status/v1");
}

#[test]
fn no_color_and_completions_work() {
    medrec()
        .env("NO_COLOR", "1")
        .args(["auth", "status"])
        .assert()
        .success();
    for shell in ["bash", "zsh", "fish"] {
        medrec().args(["completions", shell]).assert().success();
    }
}

#[test]
fn bad_arguments_exit_2_before_anything_is_read() {
    medrec().arg("no-such-command").assert().code(2);
    medrec().arg("intake").assert().code(2);
    medrec()
        .args([
            "import",
            "--from",
            "x",
            "--patient",
            "p",
            "--since",
            "03/04/2026",
        ])
        .assert()
        .code(2);
    medrec()
        .args(["timeline", "--since", "yesterday"])
        .assert()
        .code(2);
    // A typo in a kind is an error, not an empty timeline.
    medrec()
        .args(["timeline", "--kind", "results"])
        .assert()
        .code(2);
    for list in [
        &["search", "x", "--limit", "0"][..],
        &["documents", "list", "--limit", "0"],
        &["people", "list", "--limit", "-1"],
    ] {
        medrec().args(list).assert().code(2);
    }
    // --force means "do not ask" only; the birth-date check has its own flag.
    medrec()
        .args(["import", "--from", "x", "--patient", "p", "--force"])
        .assert()
        .code(2);
    medrec()
        .args(["images", "intake", "x", "--force"])
        .assert()
        .code(2);
    // Checked before the records folder is looked for: the message names
    // the argument, not the missing folder.
    let out = medrec()
        .args([
            "--json",
            "intake",
            "x.pdf",
            "--classified-by",
            "organize-scans",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stdout).contains("give at least one"));
    let out = medrec()
        .args(["--json", "intake", "x.pdf", "--date", "03/04/2026"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("records folder"));
    medrec()
        .args(["config", "set", "no_such_key", "1"])
        .assert()
        .code(2);
    medrec()
        .args(["config", "set", "ocr_max_pages", "many"])
        .assert()
        .code(2);
}

#[test]
fn without_a_records_folder_commands_say_how_to_set_one_up() {
    let out = medrec()
        .args(["--json", "people", "list"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v = json_of(&out);
    let text = v.to_string();
    assert!(text.contains("medrec init"), "{text}");
}

#[test]
fn a_mutation_needs_force_without_a_terminal() {
    let out = medrec()
        .args(["--json", "review", "drop", "q_aaaaaaaaaaaaaaaaaaaa"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(6));
    json_of(&out);
}

#[test]
fn cache_clear_works_with_nothing_cached() {
    let out = medrec()
        .args(["--json", "cache", "clear"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(json_of(&out)["files"], 0);
}

#[test]
fn config_round_trips() {
    let dir = std::env::temp_dir().join(format!("medrec-config-{}", std::process::id()));
    let run = |args: &[&str]| {
        let mut c = Command::cargo_bin("medrec").unwrap();
        c.env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_CACHE_HOME", dir.join("cache"));
        c.args(args).output().unwrap()
    };
    assert!(run(&["config", "set", "viewer", "none"]).status.success());
    assert!(run(&["config", "set", "folder", "gdrive:Medical Records"])
        .status
        .success());
    let v = json_of(&run(&["--json", "config", "show"]));
    assert_eq!(v["viewer"], "none");
    assert_eq!(v["folder"], "gdrive:Medical Records");
    assert!(run(&["config", "unset", "viewer"]).status.success());
    let v = json_of(&run(&["--json", "config", "show"]));
    assert!(v.get("viewer").is_none());
    let _ = std::fs::remove_dir_all(dir);
}
