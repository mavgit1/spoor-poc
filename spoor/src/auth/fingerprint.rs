//! Shape of a credential value — enough to recognise a replacement, not to reconstruct it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Charset {
    Hex,
    Base64url,
    Alphanumeric,
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// Character length of the observed value (not bytes).
    pub length: usize,
    pub charset: Charset,
    /// At most 4 characters, split on char boundaries.
    pub prefix: String,
}

const PREFIX_CHARS: usize = 4;

pub fn fingerprint(value: &str) -> Fingerprint {
    Fingerprint {
        length: value.chars().count(),
        charset: classify_charset(value),
        prefix: prefix(value),
    }
}

pub fn prefix(value: &str) -> String {
    value.chars().take(PREFIX_CHARS).collect()
}

fn classify_charset(value: &str) -> Charset {
    if value.is_empty() {
        return Charset::Opaque;
    }
    if value.chars().all(|c| c.is_ascii_hexdigit()) {
        return Charset::Hex;
    }
    if value.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Charset::Alphanumeric;
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Charset::Base64url;
    }
    Charset::Opaque
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_is_char_safe() {
        let fp = fingerprint("éééésecret");
        assert_eq!(fp.prefix, "éééé");
        assert_eq!(fp.length, 10);
    }

    #[test]
    fn jwt_prefix_and_opaque_charset() {
        let fp = fingerprint("eyJhbGciOiJIUzI1NiJ9.aaaa.bbbb");
        assert_eq!(fp.prefix, "eyJh");
        assert_eq!(fp.charset, Charset::Opaque);
    }

    #[test]
    fn hex_and_base64url() {
        assert_eq!(fingerprint("deadbeefcafebabe").charset, Charset::Hex);
        assert_eq!(fingerprint("sk_live_abCD01-_").charset, Charset::Base64url);
        assert_eq!(fingerprint("Abc123xyz").charset, Charset::Alphanumeric);
    }

    #[test]
    fn prefix_is_not_the_secret() {
        let secret = "tok_live_abcdefghijklmnopqrstuvwxyz012345";
        let fp = fingerprint(secret);
        assert_eq!(fp.prefix.chars().count(), 4);
        assert!(!secret.starts_with(&format!("{}{}", fp.prefix, fp.prefix)));
        assert!(secret.len() > fp.prefix.len());
    }
}
