use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::fmt;

struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Any;
        impl<'de> Visitor<'de> for Any {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(Unique(v)) = a.next_element()? {
                    values.push(v);
                }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(key) = a.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom("duplicate key"));
                    }
                    let Unique(v) = a.next_value()?;
                    values.insert(key, v);
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        d.deserialize_any(Any)
    }
}

/// Strip UTF-8 BOM and `//` / `/* ... */` JSON comments while preserving string literals.
pub fn strip_json_comments(input: &str) -> String {
    let text = input.strip_prefix('\u{feff}').unwrap_or(input);
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut in_string = false;

    while i < bytes.len() {
        if in_string {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                let ch = text[i..].chars().next().unwrap_or('\\');
                out.push(ch);
                i += ch.len_utf8();
                if i < bytes.len() {
                    let next_ch = text[i..].chars().next().unwrap_or(' ');
                    out.push(next_ch);
                    i += next_ch.len_utf8();
                }
                continue;
            }
            if bytes[i] == b'"' {
                in_string = false;
            }
            let ch = text[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        } else {
            if bytes[i] == b'"' {
                in_string = true;
                out.push('"');
                i += 1;
                continue;
            }
            if bytes[i] == b'/' && i + 1 < bytes.len() {
                if bytes[i + 1] == b'/' {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                if bytes[i + 1] == b'*' {
                    let comment_start = i;
                    out.push(' ');
                    i += 2;
                    while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                        if bytes[i] == b'\n' {
                            out.push('\n');
                        }
                        i += 1;
                    }
                    if i + 1 < bytes.len() {
                        i += 2;
                    } else {
                        out.push_str(&text[comment_start..]);
                        i = bytes.len();
                    }
                    continue;
                }
            }
            let ch = text[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Local settings/model configuration follows the runtime's comment syntax.
/// Release metadata and all other configuration remain strict JSON.
pub fn parse_local_json(path: &str, bytes: &[u8]) -> Result<Value, String> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    if matches!(path, "agent/home/settings.json" | "agent/home/models.json") {
        let text = std::str::from_utf8(bytes).map_err(|_| "本机配置编码无效。")?;
        return parse_strict_json(strip_json_comments(text).as_bytes());
    }
    parse_strict_json(bytes)
}

/// Strict JSON parser that rejects duplicate object keys.
pub fn parse_strict_json(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice::<Unique>(bytes)
        .map(|v| v.0)
        .map_err(|_| "升级 JSON 含重复字段或格式无效。".into())
}
