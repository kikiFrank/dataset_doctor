use dd_ui_contracts::{ArtifactOwner, DecimalI64, DecimalU64};
use serde_json::Value;

const CASES: &str = include_str!("../../../testdata/contracts/ui/v1/decimal/cases.json");

#[test]
fn rust_matches_shared_decimal_wire_cases() {
    let cases: Vec<Value> = serde_json::from_str(CASES).unwrap();
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let raw = case["raw"].as_str().unwrap();
        let accepted = case["accepted"].as_bool().unwrap();
        let result = match case["profile"].as_str() {
            Some("u64") => serde_json::from_str::<DecimalU64>(raw)
                .map(|value| serde_json::to_string(&value).unwrap()),
            Some("i64") => serde_json::from_str::<DecimalI64>(raw)
                .map(|value| serde_json::to_string(&value).unwrap()),
            profile => panic!("unknown decimal profile {profile:?}"),
        };
        assert_eq!(result.is_ok(), accepted, "{id}");
        if let Ok(encoded) = result {
            assert_eq!(encoded, raw, "{id}");
        }
    }
}

#[test]
fn artifact_owner_is_a_discriminated_kind_and_id() {
    for kind in ["TASK", "ASSET", "REVIEW"] {
        let raw = format!(r#"{{"kind":"{kind}","id":"2c823c8a-1a3a-4b0d-97b3-0982e5f2aca9"}}"#);
        let owner: ArtifactOwner = serde_json::from_str(&raw).unwrap();
        assert_eq!(serde_json::to_string(&owner).unwrap(), raw);
    }
    assert!(serde_json::from_str::<ArtifactOwner>(r#"{"kind":"UNKNOWN","id":"a"}"#).is_err());
    assert!(serde_json::from_str::<ArtifactOwner>(r#"{"kind":"TASK"}"#).is_err());
}
