//! Minimal JSON: a value type, a parser and a serializer. Enough for
//! JSON-RPC and MCP. Objects keep insertion order.

use std::fmt::{self, Write};
use std::ops::Index;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Value {
    #[default]
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

static NULL: Value = Value::Null;

impl Value {
    /// Builds an object from `(key, value)` pairs.
    pub fn object<K: Into<String>>(pairs: impl IntoIterator<Item = (K, Value)>) -> Self {
        Value::Object(pairs.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Sets `key` on an object, replacing an existing value. No-op on non-objects.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Value>) {
        if let Value::Object(fields) = self {
            let key = key.into();
            let value = value.into();
            match fields.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => *v = value,
                None => fields.push((key, value)),
            }
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Non-negative integers only.
    pub fn as_u64(&self) -> Option<u64> {
        self.as_f64().filter(|n| n.fract() == 0.0 && *n >= 0.0 && *n <= u64::MAX as f64).map(|n| n as u64)
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.as_f64().filter(|n| n.fract() == 0.0 && n.abs() <= i64::MAX as f64).map(|n| n as i64)
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }
}

/// `value["key"]` yields `Null` for missing keys and non-objects.
impl Index<&str> for Value {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }
}

macro_rules! from_number {
    ($($t:ty),*) => {$(
        impl From<$t> for Value {
            fn from(n: $t) -> Self {
                Value::Number(n as f64)
            }
        }
    )*};
}
from_number!(i32, i64, u16, u32, u64, usize, f64);

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_owned())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<Vec<Value>> for Value {
    fn from(items: Vec<Value>) -> Self {
        Value::Array(items)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

// ---- serializing ----

impl fmt::Display for Value {
    /// Compact JSON.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) if !n.is_finite() => f.write_str("null"),
            Value::Number(n) if n.fract() == 0.0 && n.abs() < 1e15 => write!(f, "{}", *n as i64),
            Value::Number(n) => write!(f, "{n}"),
            Value::String(s) => write_string(f, s),
            Value::Array(items) => {
                f.write_char('[')?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_char(',')?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_char(']')
            }
            Value::Object(fields) => {
                f.write_char('{')?;
                for (i, (key, value)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_char(',')?;
                    }
                    write_string(f, key)?;
                    write!(f, ":{value}")?;
                }
                f.write_char('}')
            }
        }
    }
}

fn write_string(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    f.write_char('"')?;
    for c in s.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            // DEL and C1 too, so printed JSON can't carry terminal controls.
            c if c.is_control() => write!(f, "\\u{:04x}", c as u32)?,
            c => f.write_char(c)?,
        }
    }
    f.write_char('"')
}

// ---- parsing ----

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub offset: usize,
    pub message: &'static str,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

impl std::error::Error for Error {}

const MAX_DEPTH: usize = 128;

pub fn parse(input: &str) -> Result<Value, Error> {
    let mut p = Parser { bytes: input.as_bytes(), pos: 0, depth: 0 };
    let value = p.value()?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return Err(p.error("trailing characters"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn error(&self, message: &'static str) -> Error {
        Error { offset: self.pos, message }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, literal: &str, value: Value) -> Result<Value, Error> {
        if self.bytes[self.pos..].starts_with(literal.as_bytes()) {
            self.pos += literal.len();
            Ok(value)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn value(&mut self) -> Result<Value, Error> {
        self.skip_ws();
        match self.peek() {
            Some(b'n') => self.expect("null", Value::Null),
            Some(b't') => self.expect("true", Value::Bool(true)),
            Some(b'f') => self.expect("false", Value::Bool(false)),
            Some(b'"') => self.string().map(Value::String),
            Some(b'[') => self.nested(Self::array),
            Some(b'{') => self.nested(Self::object),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn nested(&mut self, f: fn(&mut Self) -> Result<Value, Error>) -> Result<Value, Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        let value = f(self);
        self.depth -= 1;
        value
    }

    fn array(&mut self) -> Result<Value, Error> {
        self.pos += 1; // [
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn object(&mut self) -> Result<Value, Error> {
        self.pos += 1; // {
        let mut fields = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(fields));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a key"));
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(self.error("expected ':'"));
            }
            self.pos += 1;
            fields.push((key, self.value()?));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(fields));
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn string(&mut self) -> Result<String, Error> {
        self.pos += 1; // "
        let mut out = Vec::new();
        loop {
            let Some(b) = self.peek() else { return Err(self.error("unterminated string")) };
            self.pos += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let Some(e) = self.peek() else { return Err(self.error("unterminated escape")) };
                    self.pos += 1;
                    let c = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => return Err(self.error("invalid escape")),
                    };
                    out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                }
                0x00..=0x1F => return Err(self.error("control character in string")),
                _ => out.push(b),
            }
        }
        // The input was a &str and escapes produce valid UTF-8.
        String::from_utf8(out).map_err(|_| self.error("invalid UTF-8"))
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        let digits = self.bytes.get(self.pos..self.pos + 4).ok_or_else(|| self.error("short \\u escape"))?;
        let s = std::str::from_utf8(digits).map_err(|_| self.error("invalid \\u escape"))?;
        let n = u32::from_str_radix(s, 16).map_err(|_| self.error("invalid \\u escape"))?;
        self.pos += 4;
        Ok(n)
    }

    fn unicode_escape(&mut self) -> Result<char, Error> {
        let hi = self.hex4()?;
        let cp = if (0xD800..0xDC00).contains(&hi) {
            // Surrogate pair.
            if !self.bytes[self.pos..].starts_with(b"\\u") {
                return Ok('\u{FFFD}');
            }
            self.pos += 2;
            let lo = self.hex4()?;
            if !(0xDC00..0xE000).contains(&lo) {
                return Ok('\u{FFFD}');
            }
            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
        } else {
            hi
        };
        Ok(char::from_u32(cp).unwrap_or('\u{FFFD}'))
    }

    fn number(&mut self) -> Result<Value, Error> {
        let start = self.pos;
        let digits = |p: &mut Self| {
            let s = p.pos;
            while matches!(p.peek(), Some(b'0'..=b'9')) {
                p.pos += 1;
            }
            p.pos > s
        };
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        if !digits(self) {
            return Err(self.error("invalid number"));
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !digits(self) {
                return Err(self.error("invalid number"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !digits(self) {
                return Err(self.error("invalid number"));
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or_default();
        text.parse().map(Value::Number).map_err(|_| self.error("invalid number"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let src = r#"{"a":1,"b":[true,false,null],"c":"x\"y\\z\n","d":-2.5,"e":{}}"#;
        let v = parse(src).unwrap();
        assert_eq!(v.to_string(), src);
        assert_eq!(v["a"].as_u64(), Some(1));
        assert_eq!(v["d"].as_f64(), Some(-2.5));
        assert!(v["missing"].is_null());
    }

    #[test]
    fn unicode() {
        let v = parse(r#""é 😀 中""#).unwrap();
        assert_eq!(v.as_str(), Some("é 😀 中"));
        assert_eq!(Value::from("\u{1}").to_string(), r#""\u0001""#);
        assert_eq!(Value::from("\u{7f}\u{9b}é").to_string(), r#""\u007f\u009bé""#);
    }

    #[test]
    fn numbers() {
        assert_eq!(parse("1e3").unwrap().as_u64(), Some(1000));
        assert_eq!(parse("-0.5").unwrap().as_u64(), None);
        assert_eq!(Value::from(42u64).to_string(), "42");
        assert_eq!(Value::from(0.25).to_string(), "0.25");
    }

    #[test]
    fn errors() {
        for bad in ["", "{", "[1,]", r#"{"a" 1}"#, "01x", "\"\u{1}\"", "tru", "1 2"] {
            assert!(parse(bad).is_err(), "{bad:?} should fail");
        }
        let deep = "[".repeat(200) + &"]".repeat(200);
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn building() {
        let mut v = Value::object([("id", Value::from(1u64))]);
        v.insert("ok", true);
        v.insert("id", 2u64);
        assert_eq!(v.to_string(), r#"{"id":2,"ok":true}"#);
    }
}
