use std::fmt;

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::{Map, Value, value::RawValue};

use super::ContractError;

const MAX_NESTING: usize = 128;
const MAX_INTEGER_DIGITS: usize = 4300;

pub(crate) fn strict_json(bytes: &[u8]) -> Result<Value, ContractError> {
    let input = std::str::from_utf8(bytes).map_err(|_| ContractError::Parse("invalid_utf8"))?;
    let raw = serde_json::from_str::<&RawValue>(input).map_err(|_| {
        if has_nonfinite_literal(input.as_bytes()) {
            ContractError::Parse("non_finite_number")
        } else {
            ContractError::Parse("invalid_json")
        }
    })?;
    parse_value(raw, 0)
}

// Borrow raw children so JSON numbers remain distinct from standard JSON objects.
fn parse_value(raw: &RawValue, depth: usize) -> Result<Value, ContractError> {
    if depth == MAX_NESTING {
        return Err(ContractError::Parse("nesting_too_deep"));
    }
    let encoded = raw.get();
    match encoded.as_bytes()[0] {
        b'{' => {
            let RawObject(entries) = serde_json::from_str(encoded).map_err(invalid_json)?;
            let mut object = Map::new();
            for (key, child) in entries {
                if object.contains_key(&key) {
                    return Err(ContractError::Parse("duplicate_key"));
                }
                object.insert(key, parse_value(child, depth + 1)?);
            }
            Ok(Value::Object(object))
        }
        b'[' => {
            let elements: Vec<&RawValue> = serde_json::from_str(encoded).map_err(invalid_json)?;
            elements
                .into_iter()
                .map(|child| parse_value(child, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        }
        b'-' | b'0'..=b'9' => {
            if encoded.contains(['.', 'e', 'E']) {
                if encoded
                    .parse::<f64>()
                    .is_ok_and(|number| !number.is_finite())
                {
                    return Err(ContractError::Parse("non_finite_number"));
                }
            } else if encoded.trim_start_matches('-').len() > MAX_INTEGER_DIGITS {
                return Err(ContractError::Parse("integer_too_long"));
            }
            serde_json::from_str(encoded).map_err(invalid_json)
        }
        _ => serde_json::from_str(encoded).map_err(invalid_json),
    }
}

fn invalid_json(_: serde_json::Error) -> ContractError {
    ContractError::Parse("invalid_json")
}

struct RawObject<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for RawObject<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = RawObject<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut fields: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = fields.next_entry::<String, &'de RawValue>()? {
                    entries.push(entry);
                }
                Ok(RawObject(entries))
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

// Python recognizes these three non-JSON numeric tokens before rejecting them.
fn has_nonfinite_literal(bytes: &[u8]) -> bool {
    let mut in_string = false;
    let mut escaped = false;
    let mut previous = None;
    for (index, &byte) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        if byte == b'"' {
            in_string = true;
        } else if matches!(previous, None | Some(b'[' | b':' | b',')) {
            let token: &[u8] = match byte {
                b'N' => b"NaN",
                b'I' => b"Infinity",
                b'-' => b"-Infinity",
                _ => b"",
            };
            if !token.is_empty() && bytes[index..].starts_with(token) {
                let after = bytes.get(index + token.len()).copied();
                if after.is_none_or(|next| {
                    next.is_ascii_whitespace() || matches!(next, b',' | b']' | b'}')
                }) {
                    return true;
                }
            }
        }
        if !byte.is_ascii_whitespace() {
            previous = Some(byte);
        }
    }
    false
}
