//! Black-box tests of the binary's surface: offline, no Keychain, no network.

use assert_cmd::Command;
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
        .env_remove("NO_COLOR");
    c
}

#[test]
fn info_says_no_credential_is_needed() {
    let out = medrec().args(["info", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], "cli-info/v1");
    assert_eq!(v["auth"]["required"], false);
}

#[test]
fn help_renders_and_no_color_is_accepted() {
    medrec().arg("--help").assert().success();
    medrec()
        .env("NO_COLOR", "1")
        .args(["auth", "status"])
        .assert()
        .success();
}
