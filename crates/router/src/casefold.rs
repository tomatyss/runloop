use hashbrown::Equivalent;
use std::borrow::Borrow;
use std::hash::{Hash, Hasher};

/// ASCII case-insensitive owned string for fast HashSet membership.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CiString(String);

impl CiString {
    pub fn new<S: AsRef<str>>(value: S) -> Self {
        // Store a pre-folded lowercase version so hashing and equality are cheap.
        Self(value.as_ref().to_ascii_lowercase())
    }

    /// Case-insensitive comparison against a borrowed &str.
    pub fn eq_str(&self, other: &str) -> bool {
        ascii_eq_nocase(self.0.as_bytes(), other.as_bytes())
    }
}

impl Hash for CiString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_ascii_lower_with_len(self.0.as_bytes(), state);
    }
}

impl Borrow<str> for CiString {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl Equivalent<CiBorrow<'_>> for CiString {
    fn equivalent(&self, key: &CiBorrow<'_>) -> bool {
        self.eq_str(key.0)
    }
}

pub fn hash_ascii_lower_with_len<H: Hasher>(bytes: &[u8], state: &mut H) {
    state.write_usize(bytes.len());
    for b in bytes {
        state.write_u8(b.to_ascii_lowercase());
    }
}

/// Borrowed, case-insensitive view for hashing without allocation.
pub struct CiBorrow<'a>(pub &'a str);

impl Hash for CiBorrow<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        hash_ascii_lower_with_len(self.0.as_bytes(), state);
    }
}

impl Equivalent<CiString> for CiBorrow<'_> {
    fn equivalent(&self, key: &CiString) -> bool {
        key.eq_str(self.0)
    }
}

/// Case-insensitive ASCII comparison.
pub fn ascii_eq_nocase(a: &[u8], b: &[u8]) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Case-insensitive ASCII substring check without allocating `prompt_lower`.
pub fn ascii_contains_nocase(haystack: &str, needle_lower: &str) -> bool {
    let needle_bytes = needle_lower.as_bytes();
    if needle_bytes.is_empty() {
        return false;
    }
    let hbytes = haystack.as_bytes();
    if needle_bytes.len() > hbytes.len() {
        return false;
    }
    let first = needle_bytes[0];
    let nlen = needle_bytes.len();
    for (idx, byte) in hbytes.iter().enumerate() {
        if idx + nlen > hbytes.len() {
            break;
        }
        if byte.to_ascii_lowercase() != first {
            continue;
        }
        if ascii_eq_nocase(&hbytes[idx..idx + nlen], needle_bytes) {
            return true;
        }
    }
    false
}
