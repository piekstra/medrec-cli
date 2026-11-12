//! Synthetic data for tests and dogfooding (`cargo run --example synth --
//! <DIR>`). Everything here is invented: the people are `Testperson`s, the
//! organizations `Example …`, every DICOM UID is under the synthetic root
//! `2.25.0`, and the records stream is `tests/fixtures/stream/`.
//!
//! ```text
//! <DIR>/scans/        PDFs: two lab reports (one a re-scan), an exact copy,
//!                     a visit summary, an EOB, an ambiguous page, and a PNG
//!                     "scan" (rendered with pdftoppm when it is installed)
//! <DIR>/dicom/cd1/    an ultrasound study (DICOMDIR + 3 images)
//! <DIR>/dicom/cd2/    the same study with a second series
//! <DIR>/dicom/ct.zip  a CT study for the adult, zipped (needs bsdtar)
//! <DIR>/provider/     a fake provider CLI (`fakeprov`) and its stream
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::dicom::synthetic::SyntheticStudy;
use crate::pdfgen::text_pdf;

/// The synthetic records stream, as a provider CLI writes it.
pub const STREAM: &str = include_str!("../tests/fixtures/stream/synthetic.ndjson");
/// The fake provider's `info --json`.
pub const PROVIDER_INFO: &str = include_str!("../tests/fixtures/stream/fakeprov-info.json");

pub fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Write everything above under `out`.
pub fn write_all(out: &Path) {
    write_scans(&out.join("scans"));
    write_dicom(&out.join("dicom"));
    write_provider(&out.join("provider"), STREAM);
}

pub fn write_scans(scans: &Path) {
    let lab = text_pdf(&[
        "Example Health Laboratory",
        "Laboratory report",
        "Patient: Robin Testperson",
        "DOB: 02/02/2020",
        "Collected: 03/04/2026",
        "Hemoglobin 12.5 g/dL (11.5 - 15.5)",
        "White blood cells 7.1 K/uL",
        "Platelets 250 K/uL",
        "Reviewed by the Example Health laboratory team",
    ]);
    write(&scans.join("lab_robin.pdf"), &lab);
    write(&scans.join("lab_robin_copy.pdf"), &lab);
    write(
        &scans.join("lab_robin_rescan.pdf"),
        &text_pdf(&[
            "Example Health Laboratory",
            "Laboratory report",
            "Patient: Robin Testperson",
            "DOB: 02/02/2020",
            "Collected: 03/04/2026",
            "Hemoglobin 12.5 g/dL (11.5 - 15.5)",
            "White blood cells 7.1 K/uL",
            "Platelets 250 K/uL",
            "Reviewed by the Example Health laboratory team.",
            "Page 1 of 1",
        ]),
    );
    write(
        &scans.join("visit_alex.pdf"),
        &text_pdf(&[
            "Example Clinic",
            "After visit summary",
            "Alex Testperson",
            "Date of birth: 1990-01-01",
            "Visit date: February 10, 2026",
            "Reason for visit: annual physical",
            "Follow up in one year.",
        ]),
    );
    write(
        &scans.join("eob_alex.pdf"),
        &text_pdf(&[
            "Example Health Plan",
            "Explanation of Benefits",
            "Member: Alex Testperson",
            "Date of service: 02/10/2026",
            "Amount billed $150.00  Plan paid $110.00  You owe $40.00",
            "This is not a bill.",
        ]),
    );
    write(
        &scans.join("ambiguous.pdf"),
        &text_pdf(&[
            "Example Clinic",
            "Family consent form",
            "Alex Testperson",
            "Robin Testperson",
            "Please sign and return.",
        ]),
    );
    // A raster "scan" for the OCR path.
    if Command::new("pdftoppm").arg("-v").output().is_ok() {
        let pdf = scans.join("visit_alex.pdf");
        let stem = scans.join("tmp-scan");
        let ok = Command::new("pdftoppm")
            .args(["-png", "-r", "150", "-singlefile"])
            .arg(&pdf)
            .arg(&stem)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            std::fs::rename(
                stem.with_extension("png"),
                scans.join("scan_visit_alex.png"),
            )
            .unwrap();
        }
    }
}

/// Two copies of an ultrasound study (the second adds a series) and a
/// zipped CT.
pub fn write_dicom(dicom: &Path) {
    let us = SyntheticStudy::new("US").seed(101).series(1).instances(3);
    us.write_to(&dicom.join("cd1")).unwrap();
    SyntheticStudy::new("US")
        .seed(101)
        .series(2)
        .instances(3)
        .write_to(&dicom.join("cd2"))
        .unwrap();
    let ct_dir = dicom.join("ct-src");
    SyntheticStudy::new("CT")
        .seed(202)
        .series(1)
        .instances(4)
        .study_date("20260210")
        .body_part("CHEST")
        .patient_name("Testperson^Alex")
        .patient_birth_date("19900101")
        .write_to(&ct_dir)
        .unwrap();
    let tar = if cfg!(target_os = "macos") {
        "/usr/bin/tar"
    } else {
        "bsdtar"
    };
    let zipped = Command::new(tar)
        .arg("-a")
        .arg("-cf")
        .arg(dicom.join("ct.zip"))
        .arg("-C")
        .arg(&ct_dir)
        .arg(".")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if zipped {
        std::fs::remove_dir_all(&ct_dir).unwrap();
    }
}

/// A fake `health-records/v1` provider, `fakeprov`, that prints `stream`
/// for `records export`. Returns the directory to put on `PATH`.
pub fn write_provider(provider: &Path, stream: &str) -> PathBuf {
    std::fs::create_dir_all(provider).unwrap();
    std::fs::write(provider.join("synthetic.ndjson"), stream).unwrap();
    std::fs::write(provider.join("fakeprov-info.json"), PROVIDER_INFO).unwrap();
    let script = provider.join("fakeprov");
    write(
        &script,
        format!(
            "#!/bin/sh\n# A fake health-records/v1 provider for medrec's tests.\ncase \"$1\" in\n  info) cat '{0}/fakeprov-info.json' ;;\n  records) cat '{0}/synthetic.ndjson'; exit \"${{FAKEPROV_EXIT:-0}}\" ;;\n  *) echo \"fakeprov: unknown command $1\" >&2; exit 2 ;;\nesac\n",
            provider.display()
        )
        .as_bytes(),
    );
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    provider.to_path_buf()
}
