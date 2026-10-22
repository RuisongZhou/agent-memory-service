//! SurrealDB's CBOR encoding of values, mapped to and from JSON the way SurrealDB's SDK maps its
//! values: strings stay strings, NONE and NULL read back as null, and the tagged types (datetimes,
//! UUIDs, record IDs, ...) read back as their string forms.
use ciborium::Value as Cbor;
use serde_json::{Map, Number, Value};

const TAG_NONE: u64 = 6;
const TAG_RECORD_ID: u64 = 8;
const TAG_DATETIME: u64 = 12;
const TAG_UUID: u64 = 37;

pub fn text(s: &str) -> Cbor {
    Cbor::Text(s.to_string())
}

pub fn from_json(value: Value) -> Cbor {
    match value {
        Value::Null => Cbor::Null,
        Value::Bool(b) => Cbor::Bool(b),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Cbor::Integer(i.into()),
            (None, Some(f)) => Cbor::Float(f),
            (None, None) => Cbor::Text(n.to_string()),
        },
        Value::String(s) => Cbor::Text(s),
        Value::Array(items) => Cbor::Array(items.into_iter().map(from_json).collect()),
        Value::Object(map) => Cbor::Map(
            map.into_iter()
                .map(|(k, v)| (Cbor::Text(k), from_json(v)))
                .collect(),
        ),
    }
}

pub fn to_json(value: Cbor) -> Value {
    match value {
        Cbor::Null => Value::Null,
        Cbor::Bool(b) => Value::Bool(b),
        Cbor::Integer(i) => {
            let i = i128::from(i);
            i64::try_from(i)
                .map(Value::from)
                .or_else(|_| u64::try_from(i).map(Value::from))
                .unwrap_or(Value::Null)
        }
        Cbor::Float(f) => Number::from_f64(f).map_or(Value::Null, Value::Number),
        Cbor::Text(s) => Value::String(s),
        Cbor::Bytes(bytes) => Value::Array(bytes.into_iter().map(Value::from).collect()),
        Cbor::Array(items) => Value::Array(items.into_iter().map(to_json).collect()),
        Cbor::Map(entries) => Value::Object(
            entries
                .into_iter()
                .map(|(k, v)| (key(k), to_json(v)))
                .collect::<Map<_, _>>(),
        ),
        Cbor::Tag(tag, inner) => tagged(tag, *inner),
        _ => Value::Null,
    }
}

/// A tagged value. Tags this store never reads (tables, decimals, durations, sets, geometries)
/// keep their inner value.
fn tagged(tag: u64, inner: Cbor) -> Value {
    match (tag, inner) {
        (TAG_NONE, _) => Value::Null,
        (TAG_DATETIME, Cbor::Array(parts)) => match parts.as_slice() {
            [Cbor::Integer(secs), Cbor::Integer(nanos)] => {
                let (secs, nanos) = (i128::from(*secs), i128::from(*nanos));
                i64::try_from(secs)
                    .ok()
                    .zip(u32::try_from(nanos).ok())
                    .and_then(|(s, n)| chrono::DateTime::from_timestamp(s, n))
                    .map_or(Value::Null, |t| {
                        Value::String(t.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
                    })
            }
            _ => Value::Null,
        },
        (TAG_UUID, Cbor::Bytes(b)) if b.len() == 16 => {
            let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
            Value::String(format!(
                "{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            ))
        }
        (TAG_RECORD_ID, Cbor::Array(mut parts)) if parts.len() == 2 => {
            let id = match to_json(parts.pop().expect("two parts")) {
                Value::String(s) => s,
                other => other.to_string(),
            };
            Value::String(format!("{}:{id}", key(parts.pop().expect("two parts"))))
        }
        (_, inner) => to_json(inner),
    }
}

fn key(value: Cbor) -> String {
    match value {
        Cbor::Text(s) => s,
        other => match to_json(other) {
            Value::String(s) => s,
            other => other.to_string(),
        },
    }
}

/// Remove and return the field `name` of a CBOR map.
pub fn take(entries: &mut Vec<(Cbor, Cbor)>, name: &str) -> Option<Cbor> {
    let at = entries
        .iter()
        .position(|(k, _)| matches!(k, Cbor::Text(t) if t == name))?;
    Some(entries.swap_remove(at).1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_round_trips_and_strings_stay_strings() {
        let doc = json!({"mid": "m1", "at": "2026-10-07T10:00:00+08:00", "key": "TODO:fix",
                         "n": 3, "big": u64::MAX, "f": 0.25, "list": [1, null, true], "nested": {"a": []}});
        let back = to_json(from_json(doc.clone()));
        assert_eq!(back["at"], "2026-10-07T10:00:00+08:00");
        assert_eq!(back["key"], "TODO:fix");
        assert_eq!(back["big"], json!(u64::MAX as f64));
        let mut expected = doc;
        expected["big"] = json!(u64::MAX as f64);
        assert_eq!(back, expected);
    }

    #[test]
    fn tagged_values_read_back_as_the_sdk_reads_them() {
        let tag = |t, v| Cbor::Tag(t, Box::new(v));
        assert_eq!(to_json(tag(TAG_NONE, Cbor::Null)), Value::Null);
        let datetime = tag(
            TAG_DATETIME,
            Cbor::Array(vec![
                Cbor::Integer(1_791_367_200.into()),
                Cbor::Integer(5.into()),
            ]),
        );
        assert_eq!(to_json(datetime), "2026-10-07T10:00:00.000000005Z");
        let record = tag(
            TAG_RECORD_ID,
            Cbor::Array(vec![text("memories"), text("m1")]),
        );
        assert_eq!(to_json(record), "memories:m1");
        let uuid = tag(TAG_UUID, Cbor::Bytes((0u8..16).collect()));
        assert_eq!(to_json(uuid), "00010203-0405-0607-0809-0a0b0c0d0e0f");
        assert_eq!(to_json(tag(7, text("memories"))), "memories");
        assert_eq!(to_json(Cbor::Float(f64::NAN)), Value::Null);
    }
}
