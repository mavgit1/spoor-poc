//! Heuristics for dynamic path segment values (IDs, UUIDs, etc.).

pub(super) fn is_numeric_string(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let s = s.strip_prefix('-').unwrap_or(s);
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

pub(super) fn is_upper_case_slug(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let all_upper_digit_underscore = s
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if !all_upper_digit_underscore {
        return false;
    }
    let has_underscore = s.contains('_');
    let all_alpha = s.chars().all(|c| c.is_ascii_uppercase());
    if has_underscore {
        s.len() >= 3
    } else {
        all_alpha && s.len() >= 4
    }
}

pub(super) fn is_hex_string(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("0x") {
        rest.len() >= 8 && rest.chars().all(|c| c.is_ascii_hexdigit())
    } else {
        s.len() >= 16 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
    }
}

pub(super) fn is_base58(s: &str) -> bool {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    s.len() >= 20 && s.bytes().all(|b| ALPHABET.contains(&b))
}

pub(super) fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let expected_lens = [8, 4, 4, 4, 12];
    parts
        .iter()
        .zip(expected_lens.iter())
        .all(|(part, &len)| part.len() == len && part.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_string_valid() {
        assert!(is_numeric_string("123"));
        assert!(is_numeric_string("-1"));
    }

    #[test]
    fn uuid_valid() {
        assert!(is_uuid("550e8400-e29b-41d4-a716-446655440000"));
    }
}
