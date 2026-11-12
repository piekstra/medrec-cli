//! The synthetic stream fixture is a records-stream/v1 stream medrec
//! accepts, carries the fields the views read, and names only synthetic
//! people (tests/fixtures/README.md).

use std::io::BufReader;

use medrec::stream::{Item, RecordsReader};
use serde_json::Value;

const STREAM: &str = include_str!("fixtures/stream/synthetic.ndjson");

fn resources() -> Vec<Value> {
    let mut r = RecordsReader::new(BufReader::new(STREAM.as_bytes()));
    r.header().expect("a header");
    let mut out = Vec::new();
    loop {
        match r.next() {
            Some(Ok(Item::Resource(raw))) => out.push(raw.value().unwrap()),
            Some(Ok(Item::Trailer(t))) => {
                assert!(t.complete);
                break;
            }
            Some(Err(e)) => panic!("the fixture is refused: {e}"),
            None => panic!("no trailer"),
        }
    }
    assert!(r.next().is_none());
    out
}

#[test]
fn the_stream_is_complete_and_its_counts_agree() {
    let rs = resources();
    assert_eq!(rs.len(), 14);
    // A Binary follows the DocumentReference that names it.
    let pos = |id: &str| rs.iter().position(|r| r["id"] == id).unwrap();
    assert!(pos("doc-0001") < pos("bin-0001"));
    assert!(pos("doc-0002") < pos("bin-0002"));
}

#[test]
fn the_fields_the_views_read_are_present() {
    let rs = resources();
    let get = |t: &str| rs.iter().find(|r| r["resourceType"] == t).unwrap().clone();
    let obs = get("Observation");
    assert!(obs["code"]["coding"][0]["code"].is_string());
    assert!(obs["valueQuantity"]["value"].is_number());
    assert!(obs["effectiveDateTime"].is_string());
    let eob = get("ExplanationOfBenefit");
    assert!(eob["total"][0]["amount"]["value"].is_number());
    assert!(eob["payment"]["amount"]["value"].is_number());
    assert!(eob["benefitBalance"][0]["financial"][0]["usedMoney"]["value"].is_number());
    assert_eq!(
        eob["supportingInfo"][0]["valueReference"]["reference"],
        "DocumentReference/doc-0002"
    );
    let doc = get("DocumentReference");
    assert_eq!(doc["content"][0]["attachment"]["url"], "Binary/bin-0001");
    assert!(
        get("Binary")["data"].as_str().unwrap().starts_with("JVBER"),
        "a PDF"
    );
}

#[test]
fn identities_in_the_fixture_are_synthetic() {
    let rs = resources();
    for r in &rs {
        for name in r["name"].as_array().into_iter().flatten() {
            assert_eq!(name["family"], "Testperson", "{}", r["id"]);
        }
        if let Some(b) = r["birthDate"].as_str() {
            assert!(["2020-02-02", "1990-01-01"].contains(&b), "{b}");
        }
        if let Some(v) = r["subscriberId"].as_str() {
            assert!(v.starts_with("MEMBER-000000000"), "{v}");
        }
        // Every id in the stream is a dummy: `<word>-000n`.
        let id = r["id"].as_str().unwrap();
        assert!(
            id.split_once('-')
                .is_some_and(|(_, n)| n.len() == 4 && n.starts_with("000")),
            "{id}"
        );
    }
    let header: Value = serde_json::from_str(STREAM.lines().next().unwrap()).unwrap();
    assert!(header["organization"]["fhir_base"]
        .as_str()
        .unwrap()
        .contains(".example."));
    assert_eq!(header["patient"]["label"], "Robin Testperson");
}
