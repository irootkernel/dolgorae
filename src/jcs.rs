use serde::de::{Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use sha2::{Digest as _, Sha256};
use std::collections::HashSet;
use std::fmt::Write as _;

pub const RAW_PAYLOAD_LIMIT: usize = 2 * 1024 * 1024;
pub const REPRESENTED_PAYLOAD_LIMIT: usize = 3 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum LosslessJson {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

#[derive(Debug)]
pub enum JcsError {
    Json(serde_json::Error),
    DuplicateKey(String),
    InvalidNumber(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnrepresentablePayload {
    pub observed_byte_length: u64,
    pub raw_sha256: String,
    pub json_pointer: Option<String>,
    pub reason: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PayloadRepresentation {
    Represented {
        value: LosslessJson,
        canonical_bytes: Vec<u8>,
        observed_byte_length: u64,
        raw_sha256: String,
    },
    Unrepresentable(UnrepresentablePayload),
}

impl std::fmt::Display for JcsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => error.fmt(formatter),
            Self::DuplicateKey(key) => write!(formatter, "duplicate object key: {key}"),
            Self::InvalidNumber(number) => write!(formatter, "invalid JSON number: {number}"),
        }
    }
}

impl std::error::Error for JcsError {}

impl From<serde_json::Error> for JcsError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub fn parse(input: &str) -> Result<LosslessJson, JcsError> {
    let raw = RawValue::from_string(input.to_owned())?;
    parse_raw(&raw)
}

fn parse_raw(raw: &RawValue) -> Result<LosslessJson, JcsError> {
    let source = raw.get().trim_start();
    match source.as_bytes().first().copied() {
        Some(b'{') => Ok(LosslessJson::Object(
            serde_json::from_str::<RawObject>(source)
                .map_err(classify_json_error)?
                .0,
        )),
        Some(b'[') => {
            let values = serde_json::from_str::<Vec<Box<RawValue>>>(source)?;
            Ok(LosslessJson::Array(
                values
                    .iter()
                    .map(|value| parse_raw(value))
                    .collect::<Result<_, _>>()?,
            ))
        }
        Some(b'"') => Ok(LosslessJson::String(serde_json::from_str(source)?)),
        Some(b't' | b'f') => Ok(LosslessJson::Bool(serde_json::from_str(source)?)),
        Some(b'n') => {
            let _: Option<()> = serde_json::from_str(source)?;
            Ok(LosslessJson::Null)
        }
        Some(_) => {
            if !is_json_number(source) {
                return Err(JcsError::InvalidNumber(source.to_owned()));
            }
            Ok(LosslessJson::Number(source.to_owned()))
        }
        None => Err(JcsError::Json(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "empty JSON",
        )))),
    }
}

fn classify_json_error(error: serde_json::Error) -> JcsError {
    let message = error.to_string();
    let prefix = "duplicate object key: ";
    if let Some(rest) = message.strip_prefix(prefix) {
        let key = rest.split(" at line ").next().unwrap_or(rest);
        JcsError::DuplicateKey(key.to_owned())
    } else {
        JcsError::Json(error)
    }
}

fn is_json_number(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut index = usize::from(bytes.first() == Some(&b'-'));
    if index >= bytes.len() {
        return false;
    }
    if bytes[index] == b'0' {
        index += 1;
        if bytes.get(index).is_some_and(u8::is_ascii_digit) {
            return false;
        }
    } else if bytes[index].is_ascii_digit() && bytes[index] != b'0' {
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    } else {
        return false;
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        let fraction_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == fraction_start {
            return false;
        }
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return false;
        }
    }
    index == bytes.len()
}

struct RawObject(Vec<(String, LosslessJson)>);

impl<'de> Deserialize<'de> for RawObject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RawObjectVisitor;

        impl<'de> Visitor<'de> for RawObjectVisitor {
            type Value = RawObject;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON object with unique decoded keys")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut keys = HashSet::new();
                let mut entries = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key.clone()) {
                        return Err(A::Error::custom(format!("duplicate object key: {key}")));
                    }
                    let raw = map.next_value::<Box<RawValue>>()?;
                    let value = parse_raw(&raw).map_err(A::Error::custom)?;
                    entries.push((key, value));
                }
                Ok(RawObject(entries))
            }
        }

        deserializer.deserialize_map(RawObjectVisitor)
    }
}

pub fn canonicalize(value: &LosslessJson) -> Result<Vec<u8>, JcsError> {
    let mut output = String::new();
    write_canonical(value, &mut output)?;
    Ok(output.into_bytes())
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn represent_payload(input: &[u8]) -> PayloadRepresentation {
    let observed_byte_length = u64::try_from(input.len()).unwrap_or(u64::MAX);
    let raw_sha256 = sha256_hex(input);
    if input.len() > RAW_PAYLOAD_LIMIT {
        return PayloadRepresentation::Unrepresentable(UnrepresentablePayload {
            observed_byte_length,
            raw_sha256,
            json_pointer: None,
            reason: "raw_payload_too_large",
        });
    }
    let text = match std::str::from_utf8(input) {
        Ok(text) => text,
        Err(_) => {
            return PayloadRepresentation::Unrepresentable(UnrepresentablePayload {
                observed_byte_length,
                raw_sha256,
                json_pointer: None,
                reason: "payload_not_utf8",
            });
        }
    };
    let parsed = match parse(text) {
        Ok(value) => value,
        Err(JcsError::DuplicateKey(_)) => {
            return PayloadRepresentation::Unrepresentable(UnrepresentablePayload {
                observed_byte_length,
                raw_sha256,
                json_pointer: None,
                reason: "duplicate_object_member",
            });
        }
        Err(_) => {
            return PayloadRepresentation::Unrepresentable(UnrepresentablePayload {
                observed_byte_length,
                raw_sha256,
                json_pointer: None,
                reason: "invalid_json",
            });
        }
    };
    let value = prepare_payload(parsed);
    let canonical_bytes = match canonicalize(&value) {
        Ok(bytes) => bytes,
        Err(_) => {
            return PayloadRepresentation::Unrepresentable(UnrepresentablePayload {
                observed_byte_length,
                raw_sha256,
                json_pointer: None,
                reason: "canonicalization_failed",
            });
        }
    };
    if canonical_bytes.len() > REPRESENTED_PAYLOAD_LIMIT {
        return PayloadRepresentation::Unrepresentable(UnrepresentablePayload {
            observed_byte_length,
            raw_sha256,
            json_pointer: None,
            reason: "represented_payload_too_large",
        });
    }
    PayloadRepresentation::Represented {
        value,
        canonical_bytes,
        observed_byte_length,
        raw_sha256,
    }
}

#[must_use]
pub fn prepare_payload(value: LosslessJson) -> LosslessJson {
    adapt_numbers(redact(escape_marker_keys(value)))
}

fn escape_marker_keys(value: LosslessJson) -> LosslessJson {
    match value {
        LosslessJson::Array(values) => {
            LosslessJson::Array(values.into_iter().map(escape_marker_keys).collect())
        }
        LosslessJson::Object(entries) => LosslessJson::Object(
            entries
                .into_iter()
                .map(|(key, value)| {
                    let key = if is_inbound_marker_key(&key) {
                        format!("${key}")
                    } else {
                        key
                    };
                    (key, escape_marker_keys(value))
                })
                .collect(),
        ),
        value => value,
    }
}

fn is_inbound_marker_key(key: &str) -> bool {
    let dollars = key.bytes().take_while(|byte| *byte == b'$').count();
    dollars > 0 && key[dollars..].starts_with("dolgorae_")
}

fn redact(value: LosslessJson) -> LosslessJson {
    match value {
        LosslessJson::Array(values) => {
            LosslessJson::Array(values.into_iter().map(redact).collect())
        }
        LosslessJson::Object(entries) => LosslessJson::Object(
            entries
                .into_iter()
                .map(|(key, value)| {
                    if is_secret_key(&key) {
                        let original_type = json_type(&value).to_owned();
                        (key, redacted_marker(original_type))
                    } else {
                        (key, redact(value))
                    }
                })
                .collect(),
        ),
        value => value,
    }
}

fn redacted_marker(original_type: String) -> LosslessJson {
    LosslessJson::Object(vec![(
        "$dolgorae_redacted".to_owned(),
        LosslessJson::Object(vec![
            (
                "reason".to_owned(),
                LosslessJson::String("secret_key".to_owned()),
            ),
            (
                "original_type".to_owned(),
                LosslessJson::String(original_type),
            ),
        ]),
    )])
}

const SECRET_SEQUENCES: &[&[&str]] = &[
    &["authorization"],
    &["proxy", "authorization"],
    &["cookie"],
    &["set", "cookie"],
    &["password"],
    &["secret"],
    &["client", "secret"],
    &["api", "key"],
    &["access", "token"],
    &["refresh", "token"],
    &["id", "token"],
    &["session", "token"],
    &["session", "key"],
    &["bearer", "token"],
    &["auth", "token"],
    &["api", "token"],
    &["oauth", "token"],
    &["security", "token"],
    &["private", "key"],
    &["secret", "key"],
    &["signing", "key"],
    &["signing", "secret"],
    &["encryption", "key"],
    &["api", "secret"],
    &["credential"],
    &["passphrase"],
    &["passwd"],
];

fn is_secret_key(key: &str) -> bool {
    if !key.is_ascii() {
        return false;
    }
    let tokens = matching_tokens(key);
    if tokens.is_empty() {
        return false;
    }
    SECRET_SEQUENCES.iter().any(|secret| {
        tokens.windows(secret.len()).any(|window| {
            window.iter().enumerate().all(|(index, candidate)| {
                candidate == secret[index]
                    || (index + 1 == window.len()
                        && candidate.strip_suffix('s') == Some(secret[index]))
            })
        }) || (tokens.concat() == secret.concat())
    })
}

fn matching_tokens(key: &str) -> Vec<String> {
    let bytes = key.as_bytes();
    let mut raw = Vec::new();
    let mut start = 0;
    for index in 0..=bytes.len() {
        let separator = index == bytes.len() || matches!(bytes[index], b'-' | b'_');
        let camel_boundary = index > start
            && index < bytes.len()
            && bytes[index].is_ascii_uppercase()
            && (bytes[index - 1].is_ascii_lowercase()
                || bytes[index - 1].is_ascii_digit()
                || (bytes[index - 1].is_ascii_uppercase()
                    && bytes.get(index + 1).is_some_and(u8::is_ascii_lowercase)));
        if separator || camel_boundary {
            if start < index {
                raw.push(key[start..index].to_ascii_lowercase());
            }
            start = index + usize::from(separator);
        }
    }
    raw.into_iter()
        .filter_map(|mut token| {
            while token.ends_with(|character: char| character.is_ascii_digit()) {
                token.pop();
            }
            (!token.is_empty()).then_some(token)
        })
        .collect()
}

fn json_type(value: &LosslessJson) -> &'static str {
    match value {
        LosslessJson::Null => "null",
        LosslessJson::Bool(_) => "boolean",
        LosslessJson::Number(_) => "number",
        LosslessJson::String(_) => "string",
        LosslessJson::Array(_) => "array",
        LosslessJson::Object(_) => "object",
    }
}

fn adapt_numbers(value: LosslessJson) -> LosslessJson {
    match value {
        LosslessJson::Number(number) => match adapt_number(&number) {
            Ok(rendered) if rendered.starts_with('{') => LosslessJson::Object(vec![(
                "$dolgorae_number".to_owned(),
                LosslessJson::String(number),
            )]),
            Ok(rendered) => LosslessJson::Number(rendered),
            Err(_) => LosslessJson::Object(vec![(
                "$dolgorae_number".to_owned(),
                LosslessJson::String(number),
            )]),
        },
        LosslessJson::Array(values) => {
            LosslessJson::Array(values.into_iter().map(adapt_numbers).collect())
        }
        LosslessJson::Object(entries) => LosslessJson::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key, adapt_numbers(value)))
                .collect(),
        ),
        value => value,
    }
}

fn write_canonical(value: &LosslessJson, output: &mut String) -> Result<(), JcsError> {
    match value {
        LosslessJson::Null => output.push_str("null"),
        LosslessJson::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        LosslessJson::Number(number) => output.push_str(&adapt_number(number)?),
        LosslessJson::String(value) => output.push_str(&serde_json::to_string(value)?),
        LosslessJson::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical(value, output)?;
            }
            output.push(']');
        }
        LosslessJson::Object(entries) => {
            let mut entries = entries.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.encode_utf16().cmp(right.0.encode_utf16()));
            if let Some(duplicate) = entries
                .windows(2)
                .find(|pair| pair[0].0 == pair[1].0)
                .map(|pair| pair[0].0.clone())
            {
                return Err(JcsError::DuplicateKey(duplicate));
            }
            output.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key)?);
                output.push(':');
                write_canonical(value, output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn adapt_number(original: &str) -> Result<String, JcsError> {
    let value = original
        .parse::<f64>()
        .map_err(|_| JcsError::InvalidNumber(original.to_owned()))?;
    if !value.is_finite() {
        return Ok(marker_number(original));
    }
    let rendered = ecmascript_number(value)?;
    if decimal_identity(original) == decimal_identity(&rendered) {
        Ok(rendered)
    } else {
        Ok(marker_number(original))
    }
}

fn marker_number(original: &str) -> String {
    format!(
        "{{\"$dolgorae_number\":{}}}",
        serde_json::to_string(original).expect("string serialization")
    )
}

fn ecmascript_number(value: f64) -> Result<String, JcsError> {
    if value == 0.0 {
        return Ok("0".to_owned());
    }
    let raw = serde_json::to_string(&value)?;
    let (negative, unsigned) = raw
        .strip_prefix('-')
        .map_or((false, raw.as_str()), |rest| (true, rest));
    let (mantissa, exponent) = unsigned
        .split_once(['e', 'E'])
        .map_or((unsigned, 0_i32), |(mantissa, exponent)| {
            (mantissa, exponent.parse::<i32>().expect("serde exponent"))
        });
    let dot = mantissa.find('.').unwrap_or(mantissa.len());
    let mut digits = mantissa.replace('.', "");
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    let decimal_point = i32::try_from(dot).expect("short float") + exponent;
    let mut output = String::new();
    if negative {
        output.push('-');
    }
    if decimal_point > 0 && decimal_point <= 21 {
        let point = usize::try_from(decimal_point).expect("positive");
        if point >= digits.len() {
            output.push_str(&digits);
            output.extend(std::iter::repeat_n('0', point - digits.len()));
        } else {
            output.push_str(&digits[..point]);
            output.push('.');
            output.push_str(&digits[point..]);
        }
    } else if decimal_point <= 0 && decimal_point > -6 {
        output.push_str("0.");
        output.extend(std::iter::repeat_n(
            '0',
            usize::try_from(-decimal_point).expect("bounded"),
        ));
        output.push_str(&digits);
    } else {
        output.push(digits.as_bytes()[0] as char);
        if digits.len() > 1 {
            output.push('.');
            output.push_str(&digits[1..]);
        }
        output.push('e');
        let scientific_exponent = decimal_point - 1;
        if scientific_exponent >= 0 {
            output.push('+');
        }
        write!(output, "{scientific_exponent}").expect("string write");
    }
    Ok(output)
}

fn decimal_identity(value: &str) -> (bool, String, i64) {
    let (negative, value) = value
        .strip_prefix('-')
        .map_or((false, value), |rest| (true, rest));
    let (mantissa, exponent) = value
        .split_once(['e', 'E'])
        .map_or((value, 0_i64), |(mantissa, exponent)| {
            (mantissa, exponent.parse().unwrap_or(0))
        });
    let fraction = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let mut digits = mantissa.replace('.', "");
    while digits.starts_with('0') && digits.len() > 1 {
        digits.remove(0);
    }
    let mut scale = exponent - i64::try_from(fraction).expect("fraction length");
    while digits.ends_with('0') && digits.len() > 1 {
        digits.pop();
        scale += 1;
    }
    if digits.chars().all(|character| character == '0') {
        return (false, "0".to_owned(), 0);
    }
    (negative, digits, scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(input: &str) -> String {
        String::from_utf8(canonicalize(&parse(input).unwrap()).unwrap()).unwrap()
    }

    #[test]
    fn rejects_duplicate_decoded_keys_at_every_depth() {
        assert!(parse(r#"{"a":1,"\u0061":2}"#).is_err());
        assert!(parse(r#"[{"a":1,"a":2}]"#).is_err());
        assert!(matches!(
            canonicalize(&LosslessJson::Object(vec![
                ("a".to_owned(), LosslessJson::Number("1".to_owned())),
                ("a".to_owned(), LosslessJson::Number("2".to_owned())),
            ])),
            Err(JcsError::DuplicateKey(key)) if key == "a"
        ));
    }

    #[test]
    fn sorts_keys_by_utf16_code_units() {
        assert_eq!(
            canonical(r#"{"\ud83d\ude00":1,"\ufffd":2,"a":3}"#),
            r#"{"a":3,"😀":1,"�":2}"#
        );
    }

    #[test]
    fn adapts_normative_numeric_edges() {
        assert_eq!(canonical("1.0"), "1");
        assert_eq!(canonical("1e2"), "100");
        assert_eq!(canonical("-0"), "0");
        assert_eq!(canonical("0.1"), "0.1");
        assert_eq!(canonical("1e21"), "1e+21");
        assert_eq!(
            canonical("9007199254740993"),
            r#"{"$dolgorae_number":"9007199254740993"}"#
        );
        assert_eq!(canonical("1e400"), r#"{"$dolgorae_number":"1e400"}"#);
    }

    #[test]
    fn canonicalizes_arrays_and_non_ascii_strings() {
        assert_eq!(
            canonical(r#"[true,null,"한글",{"b":2,"a":1}]"#),
            r#"[true,null,"한글",{"a":1,"b":2}]"#
        );
    }
}
