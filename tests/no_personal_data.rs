//! The repository carries no personal data; runtime output may.
//!
//! Walks every file git tracks or would add (`git ls-files --cached
//! --others --exclude-standard`, so a file not yet committed is checked
//! before it is) and looks for the shapes
//! personal data takes (phone, member, record and card numbers; email
//! addresses; street addresses; DICOM UIDs outside the synthetic root),
//! rather than a denylist of real values, which would publish exactly what
//! it exists to keep out. Synthetic values follow obvious patterns: 555
//! numbers, `example.com`, all-zero ids, `Testperson` names.

use std::process::Command;

fn tracked_files() -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("git ls-files");
    assert!(out.status.success(), "git ls-files failed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

/// Runs of at least `min` digits that stand alone: a run inside a longer
/// alphanumeric token (a hex hash, a base32 id) is not a number.
fn digit_runs(s: &str, min: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let before = start.checked_sub(1).map(|j| chars[j]);
            let after = chars.get(i).copied();
            let in_token = before.is_some_and(|c| c.is_ascii_alphabetic())
                || after.is_some_and(|c| c.is_ascii_alphabetic());
            if i - start >= min && !in_token {
                out.push(chars[start..i].iter().collect());
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Digits joined across `()-. ` separators (`(212) 867-5309`), for runs
/// that have at least one separator; a plain run is `digit_runs`' business.
fn separated_runs(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let sep = |c: char| "()-. ".contains(c);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() || (i > 0 && chars[i - 1].is_ascii_alphanumeric()) {
            i += 1;
            continue;
        }
        let mut digits = String::new();
        let mut separated = false;
        let mut first_group = 0;
        let mut j = i;
        while j < chars.len() {
            if chars[j].is_ascii_digit() {
                digits.push(chars[j]);
                if !separated {
                    first_group += 1;
                }
                j += 1;
            } else if sep(chars[j]) {
                let mut k = j;
                while k < chars.len() && sep(chars[k]) {
                    k += 1;
                }
                if k < chars.len() && chars[k].is_ascii_digit() && k - j <= 2 {
                    separated = true;
                    j = k;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        let token_after = chars.get(j).is_some_and(|c| c.is_ascii_alphabetic());
        // A phone starts with a group of 1 to 3 digits; `2026-03-04 10` is
        // a date and an hour.
        if separated && !token_after && first_group <= 3 {
            out.push(digits);
        }
        i = j.max(i + 1);
    }
    out
}

/// A synthetic number: one repeated digit, zeros then a few digits, a 555
/// number, or a compact date (`20260304`, the shape scanner file names take).
fn is_dummy_number(n: &str) -> bool {
    let first = n.chars().next().unwrap();
    let zeros = n.chars().take_while(|c| *c == '0').count();
    let compact_date = n.len() == 8
        && (n.starts_with("19") || n.starts_with("20"))
        && n[4..6].parse::<u32>().is_ok_and(|m| (1..=12).contains(&m))
        && n[6..8].parse::<u32>().is_ok_and(|d| (1..=31).contains(&d));
    n.chars().all(|c| c == first)
        || zeros + 3 >= n.len()
        || n.starts_with("555")
        || n.starts_with("1555")
        || compact_date
}

/// A clinical code: the whole value of a FHIR `"code"` key, such as a SNOMED
/// CT concept (`"code":"91935009"`). Public vocabularies identify no one.
fn is_code_value(line: &str, n: &str) -> bool {
    line.contains(&format!("\"code\":\"{n}\""))
}

fn scan_line(line: &str) -> Vec<String> {
    let mut findings = Vec::new();
    for n in digit_runs(line, 7) {
        if !is_dummy_number(&n) && !is_code_value(line, &n) {
            findings.push(format!("number {n}"));
        }
    }
    // Phone numbers written with separators: 3-3-4.
    for n in separated_runs(line)
        .into_iter()
        .filter(|n| (10..=11).contains(&n.len()))
    {
        if !is_dummy_number(&n) {
            findings.push(format!("phone-like {n}"));
        }
    }
    for word in line.split(|c: char| c.is_whitespace() || "\"'<>(),;`".contains(c)) {
        if let Some((local, domain)) = word.split_once('@') {
            let domain =
                domain.trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
            let ok = local.is_empty()
                || !domain.contains('.')
                || domain == "example.com"
                || domain == "example.org"
                || domain.ends_with(".example")
                || domain.ends_with("users.noreply.github.com");
            if !ok {
                findings.push(format!("email {word}"));
            }
        }
    }
    let lower = line.to_ascii_lowercase();
    for suffix in [" way", " st", " ave", " blvd", " dr", " ln", " ct", " rd"] {
        if let Some(pos) = lower.find(suffix) {
            let has_number = lower[..pos]
                .split_whitespace()
                .rev()
                .take(4)
                .any(|w| w.len() >= 3 && w.chars().all(|c| c.is_ascii_digit()));
            if has_number && !lower.contains("example st") {
                findings.push(format!("address-like `{}`", line.trim()));
            }
        }
    }
    // A DICOM UID is a dotted digit string; ours sit under the synthetic
    // root 2.25.0 (`2.25` is the UUID-derived arc).
    for word in line.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let dots = word.matches('.').count();
        if dots >= 4
            && word.len() >= 16
            && !word.starts_with("2.25.0.")
            && !word.starts_with("1.2.840.10008.")
        {
            findings.push(format!("dicom-uid-like {word}"));
        }
    }
    findings
}

#[test]
fn tracked_files_carry_no_personal_data() {
    // A packaged crate has no git checkout; there is nothing to publish
    // from it.
    if !std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.git")).exists() {
        return;
    }
    let files = tracked_files();
    assert!(
        files.iter().any(|f| f == "Cargo.toml") && files.len() > 20,
        "the file list is incomplete: {files:?}"
    );
    let mut findings = Vec::new();
    for file in files {
        // This file holds the scanner's own synthetic positives.
        if file.ends_with(".lock")
            || file.starts_with("LICENSE")
            || file == "tests/no_personal_data.rs"
        {
            continue;
        }
        let path = format!("{}/{file}", env!("CARGO_MANIFEST_DIR"));
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            for f in scan_line(line) {
                findings.push(format!("{file}:{}: {f}", i + 1));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "personal-data shapes in tracked files:\n{}",
        findings.join("\n")
    );
}

#[test]
fn the_scanner_finds_what_it_exists_to_find() {
    assert!(!scan_line("call 212 867 5309").is_empty());
    assert!(!scan_line("member 9817263544").is_empty());
    assert!(!scan_line("mail someone@gmail.com").is_empty());
    assert!(!scan_line("lives at 1234 Elm St").is_empty());
    assert!(!scan_line("study 1.2.826.0.1.3680043.8.498.1").is_empty());
    assert!(scan_line("call 555 010 0199, md5 900150983cd24fb0d6963f7d28e17f72").is_empty());
    assert!(scan_line("scan_20260304_1.pdf, id 000000001, uid 2.25.0.1.2.3.4").is_empty());
    assert!(scan_line("a@example.com, transfer syntax 1.2.840.10008.1.2.1").is_empty());
    assert!(scan_line("visit 2026-03-04 10 am, (555) 010-0199").is_empty());
    assert!(scan_line(r#"{"system":"http://snomed.info/sct","code":"91935009"}"#).is_empty());
    assert!(
        !scan_line(r#"{"value":"91935009"}"#).is_empty(),
        "only a code key is a code"
    );
}
