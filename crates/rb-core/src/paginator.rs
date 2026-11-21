//! Cursor pagination. Cursors are opaque base64url JSON envelopes carrying a version, so a format
//! change is rejected instead of silently misread.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum CursorError {
    Encoding,
    Json,
    Version(u32),
}

impl std::fmt::Display for CursorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CursorError::Encoding => f.write_str("cursor is not valid base64url"),
            CursorError::Json => f.write_str("cursor payload is malformed"),
            CursorError::Version(v) => write!(f, "cursor version {v} is not supported"),
        }
    }
}

impl std::error::Error for CursorError {}

#[derive(Serialize, Deserialize)]
struct Envelope<T> {
    v: u32,
    d: T,
}

pub fn encode<T: Serialize>(value: &T) -> String {
    // Serializing plain data to a Vec can't fail; fall back to an empty cursor defensively.
    let json = serde_json::to_vec(&Envelope { v: VERSION, d: value }).unwrap_or_default();
    URL_SAFE_NO_PAD.encode(json)
}

pub fn decode<T: DeserializeOwned>(cursor: &str) -> Result<T, CursorError> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| CursorError::Encoding)?;
    let head: Envelope<serde_json::Value> = serde_json::from_slice(&bytes).map_err(|_| CursorError::Json)?;
    if head.v != VERSION {
        return Err(CursorError::Version(head.v));
    }
    serde_json::from_value(head.d).map_err(|_| CursorError::Json)
}

/// Response envelope for list endpoints.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

impl<T> Page<T> {
    /// Builds a page from a fetch-N+1 result: `items` may hold one extra row beyond `page_size`.
    pub fn from_fetch(mut items: Vec<T>, page_size: usize, cursor_of: impl Fn(&T) -> String) -> Self {
        let has_more = items.len() > page_size;
        items.truncate(page_size);
        let next_cursor = if has_more { items.last().map(cursor_of) } else { None };
        Page { items, next_cursor, has_more }
    }
}

/// Parses a `?pageSize=` value, clamping to `[1, max]` and defaulting when absent or invalid.
pub fn clamp_page_size(raw: Option<&str>, default: usize, max: usize) -> usize {
    raw.and_then(|s| s.parse::<usize>().ok()).filter(|n| *n > 0).unwrap_or(default).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_garbage_and_other_versions() {
        let c = encode(&42u64);
        assert_eq!(decode::<u64>(&c), Ok(42));
        assert_eq!(decode::<u64>("!!!"), Err(CursorError::Encoding));
        assert_eq!(decode::<u64>(&URL_SAFE_NO_PAD.encode("nope")), Err(CursorError::Json));
        let v2 = URL_SAFE_NO_PAD.encode(r#"{"v":2,"d":1}"#);
        assert_eq!(decode::<u64>(&v2), Err(CursorError::Version(2)));
    }

    #[test]
    fn page_from_fetch_n_plus_one() {
        let p = Page::from_fetch(vec![1, 2, 3], 2, encode);
        assert_eq!(p.items, vec![1, 2]);
        assert!(p.has_more);
        assert_eq!(decode::<i32>(p.next_cursor.as_deref().unwrap_or_default()), Ok(2));
        let last = Page::from_fetch(vec![1], 2, encode);
        assert!(!last.has_more && last.next_cursor.is_none());
    }

    #[test]
    fn clamps_page_size() {
        assert_eq!(clamp_page_size(None, 20, 100), 20);
        assert_eq!(clamp_page_size(Some("0"), 20, 100), 20);
        assert_eq!(clamp_page_size(Some("x"), 20, 100), 20);
        assert_eq!(clamp_page_size(Some("500"), 20, 100), 100);
        assert_eq!(clamp_page_size(Some("7"), 20, 100), 7);
    }
}
