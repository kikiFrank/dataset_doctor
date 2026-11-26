//! Freeze one canonical Core InputManifest and distinguish its two digests.

use std::error::Error;
use std::fmt::{self, Display};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::json::{ContractError, JsonContracts, JsonProfile};

#[derive(Debug)]
pub enum InputFingerprintError {
    ExpectedObject,
    DuplicateAsset,
    NumberRange,
    Contract(ContractError),
    Encoding(serde_json::Error),
}

impl Display for InputFingerprintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExpectedObject => formatter.write_str("InputManifest must be a JSON object"),
            Self::DuplicateAsset => {
                formatter.write_str("InputManifest contains a duplicate asset ID")
            }
            Self::NumberRange => {
                formatter.write_str("InputManifest number exponent cannot be represented")
            }
            Self::Contract(error) => Display::fmt(error, formatter),
            Self::Encoding(error) => write!(formatter, "Encode the InputManifest: {error}"),
        }
    }
}

impl Error for InputFingerprintError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Contract(error) => Some(error),
            Self::Encoding(error) => Some(error),
            _ => None,
        }
    }
}

impl From<serde_json::Error> for InputFingerprintError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encoding(error)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct FrozenInputManifest {
    pub bytes: Vec<u8>,
    pub input_fingerprint: [u8; 32],
    pub sha256: [u8; 32],
}

/// Fingerprint the schema-validated manifest without its fingerprint field.
/// Objects sort by UTF-8 key; asset membership sorts by lowercase UUID while
/// other arrays keep their order. JSON numbers normalize as exact decimals.
pub fn freeze_input_manifest(
    contracts: &JsonContracts,
    mut manifest: Value,
) -> Result<FrozenInputManifest, InputFingerprintError> {
    let draft = manifest
        .as_object_mut()
        .ok_or(InputFingerprintError::ExpectedObject)?;
    draft.insert("input_fingerprint".into(), Value::String("0".repeat(64)));
    let mut validated = contracts
        .validate(JsonProfile::InputManifest, &serde_json::to_vec(&manifest)?)
        .map_err(InputFingerprintError::Contract)?;
    let fields = validated
        .as_object_mut()
        .expect("validated InputManifest object");
    fields.remove("input_fingerprint");
    let dataset = fields
        .get_mut("dataset_id")
        .expect("validated Dataset ID field");
    if let Value::String(dataset) = dataset {
        dataset.make_ascii_lowercase();
    }
    let assets = fields
        .get_mut("assets")
        .and_then(Value::as_array_mut)
        .expect("validated assets array");
    for asset in assets.iter_mut() {
        asset["asset_id"] = Value::String(asset_id(asset).to_ascii_lowercase());
    }
    assets.sort_unstable_by(|left, right| asset_id(left).cmp(asset_id(right)));
    if assets
        .windows(2)
        .any(|pair| asset_id(&pair[0]) == asset_id(&pair[1]))
    {
        return Err(InputFingerprintError::DuplicateAsset);
    }

    let mut projection = Vec::new();
    canonical_json(&mut projection, &validated)?;
    let digest = Sha256::digest(&projection);
    validated["input_fingerprint"] = Value::String(format!("{digest:x}"));
    let mut bytes = Vec::new();
    canonical_json(&mut bytes, &validated)?;
    let sha256 = Sha256::digest(&bytes).into();
    Ok(FrozenInputManifest {
        bytes,
        input_fingerprint: digest.into(),
        sha256,
    })
}

fn asset_id(asset: &Value) -> &str {
    asset["asset_id"].as_str().expect("validated Asset ID")
}

fn canonical_json(output: &mut Vec<u8>, value: &Value) -> Result<(), InputFingerprintError> {
    match value {
        Value::Object(fields) => {
            let mut fields: Vec<_> = fields.iter().collect();
            fields.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            output.push(b'{');
            for (index, (key, value)) in fields.into_iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                serde_json::to_writer(&mut *output, key)?;
                output.push(b':');
                canonical_json(output, value)?;
            }
            output.push(b'}');
        }
        Value::Array(items) => {
            output.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                canonical_json(output, item)?;
            }
            output.push(b']');
        }
        Value::Number(number) => {
            output.extend_from_slice(canonical_number(&number.to_string())?.as_bytes())
        }
        other => serde_json::to_writer(output, other)?,
    }
    Ok(())
}

fn canonical_number(raw: &str) -> Result<String, InputFingerprintError> {
    let (sign, magnitude) = raw
        .strip_prefix('-')
        .map_or(("", raw), |magnitude| ("-", magnitude));
    let (coefficient, exponent) = magnitude.split_once(['e', 'E']).unwrap_or((magnitude, "0"));
    let (integer, fraction) = coefficient.split_once('.').unwrap_or((coefficient, ""));
    let digits = [integer, fraction].concat();
    let nonzero = digits.trim_start_matches('0');
    if nonzero.is_empty() {
        return Ok("0".into());
    }
    let significant = nonzero.trim_end_matches('0');
    let fraction = i64::try_from(fraction.len()).map_err(|_| InputFingerprintError::NumberRange)?;
    let trailing = i64::try_from(nonzero.len() - significant.len())
        .map_err(|_| InputFingerprintError::NumberRange)?;
    let exponent = exponent
        .parse::<i64>()
        .ok()
        .and_then(|value| value.checked_sub(fraction))
        .and_then(|value| value.checked_add(trailing))
        .ok_or(InputFingerprintError::NumberRange)?;
    if exponent == 0 {
        Ok(format!("{sign}{significant}"))
    } else {
        Ok(format!("{sign}{significant}e{exponent}"))
    }
}
