//! PII scrubbing. Every string that enters the buffer passes through here: property values,
//! breadcrumb messages, crash messages and stacks.
//!
//! - Properties whose **key** names a secret or contact detail (`email`, `password`, `token`,
//!   `authorization`, `cookie`, `phone`, `api_key`, `ip`, …) are replaced by `[redacted]`.
//! - In free text: e-mail addresses → `[email]`, JWTs and `Bearer …` values → `[token]`, URL query
//!   strings → `?[redacted]`, long opaque secrets (≥ 32 chars of base64/hex, not a UUID) →
//!   `[redacted]`, phone-like digit runs (≥ 9 digits) → `[phone]`.
//!
//! The rules are deliberately conservative: losing a debugging detail is acceptable, while leaking
//! an e-mail address or a token is not.

use std::collections::HashMap;

use crate::event::PropValue;

const REDACTED: &str = "[redacted]";

const SENSITIVE_SEGMENTS: &[&str] = &[
    "email",
    "mail",
    "password",
    "passwd",
    "pwd",
    "pass",
    "token",
    "secret",
    "authorization",
    "auth",
    "bearer",
    "cookie",
    "session",
    "phone",
    "msisdn",
    "mobile",
    "ssn",
    "ip",
    "apikey",
    "jwt",
    "otp",
    "pin",
    "address",
];

/// Whether a property key names sensitive data.
pub fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    if lower.contains("apikey") || lower.contains("api_key") || lower.contains("accesskey") {
        return true;
    }
    // Split snake_case, kebab-case, dotted and camelCase keys into segments.
    let mut segments: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in key.chars() {
        if c == '_' || c == '-' || c == '.' || c == ' ' {
            if !cur.is_empty() {
                segments.push(std::mem::take(&mut cur));
            }
        } else if c.is_uppercase() && !cur.is_empty() && !cur.chars().all(|x| x.is_uppercase()) {
            segments.push(std::mem::take(&mut cur));
            cur.extend(c.to_lowercase());
        } else {
            cur.extend(c.to_lowercase());
        }
    }
    if !cur.is_empty() {
        segments.push(cur);
    }
    segments
        .iter()
        .any(|s| SENSITIVE_SEGMENTS.contains(&s.to_ascii_lowercase().as_str()))
}

fn is_email(t: &str) -> bool {
    let Some((local, domain)) = t.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-')
}

fn is_jwt(t: &str) -> bool {
    t.starts_with("eyJ")
        && t.matches('.').count() == 2
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '=')
}

fn is_uuid(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

fn is_opaque_secret(t: &str) -> bool {
    t.len() >= 32
        && !is_uuid(t)
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '/' | '='))
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars().any(|c| c.is_ascii_alphabetic())
}

fn is_phone(t: &str) -> bool {
    let digits = t.chars().filter(char::is_ascii_digit).count();
    digits >= 9
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '(' | ')' | '.'))
}

/// Scrubs free text.
pub fn scrub_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_word_bearer = false;
    for (i, token) in text.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        // Keep surrounding punctuation outside the inspected core.
        let core_start = token
            .find(|c: char| !matches!(c, '(' | '[' | '<' | '"' | '\'' | '{'))
            .unwrap_or(token.len());
        let core_end = token
            .rfind(|c: char| {
                !matches!(
                    c,
                    ')' | ']' | '>' | '"' | '\'' | '}' | ',' | ';' | ':' | '.' | '!' | '?'
                )
            })
            .map(|i| i + token[i..].chars().next().map_or(1, char::len_utf8))
            .unwrap_or(core_start)
            .max(core_start);
        let (pre, core, post) = (
            &token[..core_start],
            &token[core_start..core_end],
            &token[core_end..],
        );
        let replaced: Option<String> = if core.is_empty() {
            None
        } else if prev_word_bearer {
            Some("[token]".into())
        } else if is_email(core) {
            Some("[email]".into())
        } else if is_jwt(core) {
            Some("[token]".into())
        } else if (core.starts_with("http://") || core.starts_with("https://")) && core.contains('?') {
            let base = core.split('?').next().unwrap_or("");
            Some(format!("{}?{REDACTED}", scrub_url_path(base)))
        } else if core.starts_with("http://") || core.starts_with("https://") {
            Some(scrub_url_path(core))
        } else if is_opaque_secret(core) {
            Some(REDACTED.into())
        } else if is_phone(core) {
            Some("[phone]".into())
        } else {
            None
        };
        prev_word_bearer = core.eq_ignore_ascii_case("bearer") || core.eq_ignore_ascii_case("token");
        out.push_str(pre);
        out.push_str(replaced.as_deref().unwrap_or(core));
        out.push_str(post);
    }
    out
}

/// Removes userinfo (`user:pass@`) and secret-looking path segments from a URL without a query.
fn scrub_url_path(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let rest = match rest.split_once('@') {
        Some((userinfo, host)) if !userinfo.contains('/') => host,
        _ => rest,
    };
    let scrubbed: Vec<String> = rest
        .split('/')
        .map(|seg| {
            if is_opaque_secret(seg) || is_jwt(seg) || is_email(seg) {
                REDACTED.to_owned()
            } else {
                seg.to_owned()
            }
        })
        .collect();
    format!("{scheme}://{}", scrubbed.join("/"))
}

/// Truncates to at most `max_chars` characters (on a char boundary), adding `…`.
pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut s: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// Scrubs and truncates a property map in place; drops non-finite floats.
pub fn scrub_properties(props: &mut HashMap<String, PropValue>, max_chars: usize) {
    props.retain(|_, v| !matches!(v, PropValue::Float(f) if !f.is_finite()));
    for (k, v) in props.iter_mut() {
        if is_sensitive_key(k) {
            *v = PropValue::Text(REDACTED.into());
        } else if let PropValue::Text(s) = v {
            *s = truncate(&scrub_text(s), max_chars);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_keys() {
        for k in [
            "email",
            "user_email",
            "userEmail",
            "password",
            "access_token",
            "refreshToken",
            "Authorization",
            "api_key",
            "apiKey",
            "phone_number",
            "client-ip",
            "session_token",
        ] {
            assert!(is_sensitive_key(k), "{k}");
        }
        for k in [
            "video_id",
            "screen",
            "author_channel",
            "bitrate_bps",
            "duration_ms",
            "error_code",
        ] {
            assert!(!is_sensitive_key(k), "{k}");
        }
    }

    #[test]
    fn scrubs_free_text() {
        assert_eq!(
            scrub_text("contact me at sara@example.com."),
            "contact me at [email]."
        );
        assert_eq!(
            scrub_text("Authorization: Bearer abc.def-ghi"),
            "Authorization: Bearer [token]"
        );
        assert_eq!(
            scrub_text("jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig_part-1"),
            "jwt [token]"
        );
        assert_eq!(
            scrub_text("GET https://api.example.net/v1/videos?token=s3cr3t&x=1 failed"),
            "GET https://api.example.net/v1/videos?[redacted] failed"
        );
        assert_eq!(
            scrub_text("https://user:pw@host.net/a/0123456789abcdef0123456789abcdef"),
            "https://host.net/a/[redacted]"
        );
        assert_eq!(
            scrub_text("call +44 7700900123 or (020)7946-0958"),
            "call +44 [phone] or ([phone]"
        );
        assert_eq!(
            scrub_text("key AKIA0123456789ABCDEFGHIJKLMNOPQRSTU"),
            "key [redacted]"
        );
        let uuid = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(scrub_text(&format!("video {uuid}")), format!("video {uuid}"));
        assert_eq!(scrub_text("مرحبا بالعالم 42"), "مرحبا بالعالم 42");
        assert_eq!(scrub_text(""), "");
    }

    #[test]
    fn truncates_and_scrubs_maps() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("عربي", 10), "عربي");
        let mut p: HashMap<String, PropValue> = HashMap::new();
        p.insert("email".into(), "a@b.co".into());
        p.insert("note".into(), "mail a@b.co now".into());
        p.insert("nan".into(), PropValue::Float(f64::NAN));
        p.insert("n".into(), PropValue::Int(5));
        scrub_properties(&mut p, 100);
        assert_eq!(p["email"], PropValue::Text("[redacted]".into()));
        assert_eq!(p["note"], PropValue::Text("mail [email] now".into()));
        assert!(!p.contains_key("nan"));
        assert_eq!(p["n"], PropValue::Int(5));
    }
}
