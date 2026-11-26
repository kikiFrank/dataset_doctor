use dd_infra::input_fingerprint::{InputFingerprintError, freeze_input_manifest};
use dd_infra::json::JsonContracts;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const GOLDEN_PROJECTION: &str = include_str!(
    "../../../testdata/contracts/json-schema/v1/input-fingerprint/canonical-projection.json"
);
const GOLDEN_SHA256: &str = "1afcdc024ee99590e8282e66bf4942e55339e54740c926ee72b8aadcc73d8ea0";

#[test]
fn equivalent_input_values_freeze_to_the_independently_hashed_golden_vector() {
    let contracts = JsonContracts::new().unwrap();
    let ordered: Value = serde_json::from_str(GOLDEN_PROJECTION).unwrap();
    let forward = freeze_input_manifest(&contracts, ordered.clone()).unwrap();
    let mut reordered = ordered.clone();
    reordered["dataset_id"] = json!(ordered["dataset_id"].as_str().unwrap().to_ascii_uppercase());
    reordered["assets"].as_array_mut().unwrap().reverse();
    reordered["resolved_config"]["threshold"] = serde_json::from_str("0.500").unwrap();
    reordered["resolved_config"]["zero"] = serde_json::from_str("-0.00").unwrap();
    reordered["input_fingerprint"] = json!("untrusted prior value");
    let reverse = freeze_input_manifest(&contracts, reordered).unwrap();
    assert_eq!(forward, reverse);
    let document: Value = serde_json::from_slice(&forward.bytes).unwrap();
    assert_eq!(document["input_fingerprint"], GOLDEN_SHA256);
    assert_eq!(
        document["resolved_config"]["wide"].as_u64(),
        Some(9_007_199_254_740_993)
    );
    let actual_bytes: [u8; 32] = Sha256::digest(&forward.bytes).into();
    assert_eq!(forward.sha256, actual_bytes);
    assert_ne!(forward.sha256, forward.input_fingerprint);

    let mut changed = ordered;
    changed["resolved_config"]["detections"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert_ne!(
        freeze_input_manifest(&contracts, changed)
            .unwrap()
            .input_fingerprint,
        forward.input_fingerprint
    );
}

#[test]
fn rejects_duplicate_asset_id_and_missing_contract_fields() {
    let contracts = JsonContracts::new().unwrap();
    let mut duplicate: Value = serde_json::from_str(GOLDEN_PROJECTION).unwrap();
    let first_id = duplicate["assets"][0]["asset_id"]
        .as_str()
        .unwrap()
        .to_ascii_uppercase();
    duplicate["assets"][1]["asset_id"] = json!(first_id);
    assert!(matches!(
        freeze_input_manifest(&contracts, duplicate),
        Err(InputFingerprintError::DuplicateAsset)
    ));
    let mut invalid: Value = serde_json::from_str(GOLDEN_PROJECTION).unwrap();
    invalid.as_object_mut().unwrap().remove("capability_id");
    assert!(matches!(
        freeze_input_manifest(&contracts, invalid),
        Err(InputFingerprintError::Contract(_))
    ));
}
