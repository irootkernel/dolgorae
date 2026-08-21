use serde::de::{Error as _, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use std::collections::HashSet;
use std::fmt::Write as _;

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
            serde_json::from_str::<RawObject>(source)?.0,
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
