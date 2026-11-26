use dd_infra::json::{ContractError, JsonContracts, JsonProfile};
use serde_json::Value;

const INPUT_MANIFEST: &str =
    include_str!("../../../testdata/contracts/json-schema/v1/input-manifest/cases.json");
const WORKSPACE_CONFIG: &str =
    include_str!("../../../testdata/contracts/json-schema/v1/workspace-config/cases.json");
const COCO_SINGLE_IMAGE: &str =
    include_str!("../../../testdata/contracts/json-schema/v1/coco-single-image/cases.json");

#[test]
fn rust_matches_all_shared_python_json_schema_vectors() {
    let contracts = JsonContracts::new().expect("embedded schemas validate offline");
    for (profile, fixtures) in [
        (JsonProfile::InputManifest, INPUT_MANIFEST),
        (JsonProfile::WorkspaceConfig, WORKSPACE_CONFIG),
        (JsonProfile::CocoSingleImage, COCO_SINGLE_IMAGE),
    ] {
        let cases: Vec<Value> = serde_json::from_str(fixtures).unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let id = case["id"].as_str().unwrap();
            let input = case["raw"].as_str().unwrap().as_bytes();
            let outcome = contracts.validate(profile, input);
            let expected_stage = case["expected"]["stage"].as_str().unwrap();
            let category = case["expected"]["category"].as_str().unwrap();
            match (expected_stage, outcome) {
                ("ok", Ok(_)) => assert_eq!(category, "accepted", "{id}"),
                ("parse", Err(ContractError::Parse(actual))) => {
                    assert_eq!(actual, category, "{profile:?}/{id}");
                }
                ("schema", Err(ContractError::Schema(issues))) => assert!(
                    issues.iter().any(|issue| issue.keyword == category),
                    "{profile:?}/{id}: expected {category}, got {issues:?}",
                ),
                (stage, result) => panic!("{profile:?}/{id}: expected {stage}, got {result:?}"),
            }
        }
    }
}

#[test]
fn strict_parse_rejects_invalid_utf8_and_oversized_integer() {
    let contracts = JsonContracts::new().unwrap();
    let input = JsonProfile::InputManifest;
    assert!(matches!(
        contracts.validate(input, b"{\"input\":\"\xff\"}"),
        Err(ContractError::Parse("invalid_utf8"))
    ));
    let data = format!("{{\"input\":{}}}", "9".repeat(4301));
    assert!(matches!(
        contracts.validate(input, data.as_bytes()),
        Err(ContractError::Parse("integer_too_long"))
    ));
}

#[test]
fn preserves_exact_numbers_and_keys_in_extensible_config() {
    let contracts = JsonContracts::new().unwrap();
    let input: Vec<Value> = serde_json::from_str(INPUT_MANIFEST).unwrap();
    let mut valid: Value = serde_json::from_str(input[0]["raw"].as_str().unwrap()).unwrap();
    let special = serde_json::json!({ "$serde_json::private::Number": "100" });
    valid["resolved_config"]["special"] = special.clone();
    // Supply a value larger than u64 inside the extensible JSON object.
    let encoded = valid.to_string().replace(
        "\"special\":",
        "\"large_integer\":18446744073709551617,\"special\":",
    );
    let checked = contracts
        .validate(JsonProfile::InputManifest, encoded.as_bytes())
        .unwrap();
    assert_eq!(
        checked["resolved_config"]["large_integer"].to_string(),
        "18446744073709551617"
    );
    assert_eq!(checked["resolved_config"]["special"], special);
}
