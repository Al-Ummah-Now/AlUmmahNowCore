//! PII scrubbing. Every string that enters the buffer passes through here: property values,
//! breadcrumb messages, crash messages and stacks.
//!
//! - Properties whose **key** names a secret or contact detail (`email`, `password`, `token`,
//!   `authorization`, `cookie`, `phone`, `api_key`, `ip`, …) are replaced by `[redacted]`.
//! - In free text, any whitespace separates words. Words are further split on `= & : , ;`, so
//!   `token=…`, `a=1&password=…` and `Authorization:Bearer …` all redact the value after a sensitive
//!   key or a `Bearer`/`token` word. E-mail addresses → `[email]`; JWTs → `[token]`; URLs anywhere in
//!   a word lose their query/fragment (`?[redacted]`), userinfo and secret-looking path segments;
//!   long opaque secrets (≥ 32 chars of base64/hex, not a UUID) → `[redacted]`; phone-like digit
//!   runs (≥ 9 digits) → `[phone]`.
//!
//! The rules are deliberately conservative: losing a debugging detail is acceptable, while leaking
//! an e-mail address or a token is not.

use std::collections::HashMap;

use crate::event::PropValue;

const REDACTED: &str = "[redacted]";

/// Key segments (after splitting snake/kebab/dot/camel case) that name sensitive data. Adjacent
/// pairs are also joined, so `api_key`, `apiKey` and `APIKey` all match `apikey`.
const SENSITIVE_SEGMENTS: &[&str] = &[
    "email",
    "mail",
    "password",
    "passwd",
    "pwd",
    "pass",
    "passphrase",
    "token",
    "secret",
    "authorization",
    "auth",
    "bearer",
    "cookie",
    "session",
    "sessionid",
    "phone",
    "msisdn",
    "mobile",
    "tel",
    "ssn",
    "ip",
    "ipaddress",
    "apikey",
    "accesskey",
    "key",
    "jwt",
    "otp",
    "pin",
    "address",
    "credential",
    "credentials",
    "signature",
    "sig",
    "card",
    "creditcard",
    "cvv",
    "cvc",
    "iban",
    "pan",
    "dob",
    "birthdate",
    "firstname",
    "lastname",
    "fullname",
    "username",
    "login",
];

/// Splits a property key into lowercase segments: `userEmail` → `user`, `email`;
/// `APIKey` → `api`, `key`; `refresh-token` → `refresh`, `token`.
fn key_segments(key: &str) -> Vec<String> {
    let chars: Vec<char> = key.chars().collect();
    let mut segments = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                segments.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if !cur.is_empty() && i > 0 {
            let p = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let boundary = c.is_uppercase()
                && (p.is_lowercase() || p.is_ascii_digit() || (p.is_uppercase() && next_lower));
            if boundary {
                segments.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        segments.push(cur);
    }
    segments.into_iter().map(|s| s.to_lowercase()).collect()
}

/// Whether a property key (or the key of a `key=value` pair in text) names sensitive data.
pub fn is_sensitive_key(key: &str) -> bool {
    let segs = key_segments(key);
    segs.iter().any(|s| SENSITIVE_SEGMENTS.contains(&s.as_str()))
        || segs
            .windows(2)
            .any(|w| SENSITIVE_SEGMENTS.contains(&format!("{}{}", w[0], w[1]).as_str()))
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

/// Words after which the next value is a credential (`Bearer xyz`, `token xyz`).
fn introduces_secret(core: &str) -> bool {
    ["bearer", "token", "password", "passwd", "secret", "basic"]
        .iter()
        .any(|w| core.eq_ignore_ascii_case(w))
}

/// `[token]` after token-like words (`Bearer`, `token`), `[redacted]` after anything else.
fn secret_label(core: &str) -> &'static str {
    if ["bearer", "token", "basic", "jwt"]
        .iter()
        .any(|w| core.eq_ignore_ascii_case(w))
    {
        "[token]"
    } else {
        REDACTED
    }
}

/// Characters that wrap a value without being part of it.
fn trim_wrapping(seg: &str) -> (&str, &str, &str) {
    let start = seg
        .find(|c: char| !matches!(c, '(' | '[' | '<' | '"' | '\'' | '{' | '`'))
        .unwrap_or(seg.len());
    let end = seg[start..]
        .rfind(|c: char| !matches!(c, ')' | ']' | '>' | '"' | '\'' | '}' | '`' | '.' | '!' | '?'))
        .map(|i| start + i + seg[start + i..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(start);
    (&seg[..start], &seg[start..end], &seg[end..])
}

/// Scrubber state carried across words: whether the next value must be redacted, and with which
/// label.
#[derive(Default)]
struct Pending(Option<&'static str>);

fn classify(core: &str) -> Option<String> {
    if is_email(core) {
        Some("[email]".into())
    } else if is_jwt(core) {
        Some("[token]".into())
    } else if is_opaque_secret(core) {
        Some(REDACTED.into())
    } else if is_phone(core) {
        Some("[phone]".into())
    } else {
        None
    }
}

/// Scrubs one segment (no whitespace, no `=&:,;` delimiters).
fn scrub_segment(seg: &str, pending: &mut Pending, out: &mut String) -> String {
    let (pre, core, post) = trim_wrapping(seg);
    out.push_str(pre);
    if !core.is_empty() {
        match pending.0 {
            // "Authorization: Bearer xyz": keep the scheme word, redact what follows it.
            Some(_) if core.eq_ignore_ascii_case("bearer") || core.eq_ignore_ascii_case("basic") => {
                pending.0 = Some("[token]");
                out.push_str(core);
            }
            Some(label) => {
                pending.0 = None;
                out.push_str(label);
            }
            None => match classify(core) {
                Some(r) => out.push_str(&r),
                None => out.push_str(core),
            },
        }
    }
    out.push_str(post);
    core.to_owned()
}

/// Scrubs a whitespace-free word, splitting it on `=`, `&`, `:`, `,` and `;` so that
/// `token=…`, `a=1&password=…` and `Authorization:Bearer…` are all caught.
fn scrub_word(word: &str, pending: &mut Pending, out: &mut String) {
    if let Some(idx) = word.find("://") {
        let start = word[..idx]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
            .map(|i| i + 1)
            .unwrap_or(0);
        let (prefix, url) = word.split_at(start);
        if !prefix.is_empty() {
            scrub_word(prefix, pending, out);
        }
        let end = url
            .rfind(|c: char| !matches!(c, ')' | ']' | '>' | '"' | '\'' | ',' | ';' | '.' | '!'))
            .map(|i| i + url[i..].chars().next().map_or(1, char::len_utf8))
            .unwrap_or(url.len());
        if pending.0.take().is_some() {
            out.push_str(REDACTED);
        } else {
            out.push_str(&scrub_url(&url[..end]));
        }
        out.push_str(&url[end..]);
        return;
    }
    let mut seg_start = 0;
    for (i, c) in word.char_indices() {
        if matches!(c, '=' | '&' | ':' | ',' | ';') {
            let core = scrub_segment(&word[seg_start..i], pending, out);
            out.push(c);
            if matches!(c, '=' | ':') && (is_sensitive_key(&core) || introduces_secret(&core)) {
                pending.0 = Some(secret_label(&core));
            }
            seg_start = i + c.len_utf8();
        }
    }
    let core = scrub_segment(&word[seg_start..], pending, out);
    if introduces_secret(&core) {
        pending.0 = Some(secret_label(&core));
    }
}

/// Scrubs free text (any whitespace separates words).
pub fn scrub_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending = Pending::default();
    let mut word_start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = word_start.take() {
                scrub_word(&text[s..i], &mut pending, &mut out);
            }
            out.push(c);
        } else if word_start.is_none() {
            word_start = Some(i);
        }
    }
    if let Some(s) = word_start {
        scrub_word(&text[s..], &mut pending, &mut out);
    }
    out
}

/// Drops the query and fragment, userinfo (`user:pass@`) and secret-looking path segments.
fn scrub_url(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let (rest, had_query) = match rest.find(['?', '#']) {
        Some(i) => (&rest[..i], true),
        None => (rest, false),
    };
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
    let mut s = format!("{scheme}://{}", scrubbed.join("/"));
    if had_query {
        s.push('?');
        s.push_str(REDACTED);
    }
    s
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
        assert_eq!(scrub_text("Bearer abc.def-ghi"), "Bearer [token]");
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
        assert_eq!(
            scrub_text("key AKIA0123456789ABCDEFGHIJKLMNOPQRSTU"),
            "key [redacted]"
        );
        assert_eq!(scrub_text(""), "");
    }

    #[test]
    fn delimiter_and_whitespace_bypasses_are_caught() {
        assert_eq!(scrub_text("token=abc123"), "token=[token]");
        assert_eq!(
            scrub_text("a=1&password=hunter2&b=2"),
            "a=1&password=[redacted]&b=2"
        );
        assert_eq!(
            scrub_text("Authorization:Bearer\txyz"),
            "Authorization:Bearer\t[token]"
        );
        assert_eq!(scrub_text("auth:Bearer xyz"), "auth:Bearer [token]");
        assert_eq!(scrub_text("email:sara@example.com"), "email:[redacted]");
        assert_eq!(scrub_text("user=sara@example.com"), "user=[email]");
        assert_eq!(scrub_text("line1\nsara@example.com"), "line1\n[email]");
        assert_eq!(
            scrub_text("url=https://cdn.example.net/v.m3u8?sig=abc&exp=1"),
            "url=https://cdn.example.net/v.m3u8?[redacted]"
        );
        assert_eq!(
            scrub_text("(see https://x.net/a#access_token=zzz)."),
            "(see https://x.net/a?[redacted])."
        );
        assert_eq!(scrub_text("api_key=https://x.net/a"), "api_key=[redacted]");
        assert_eq!(scrub_text("code=42 status:ok"), "code=42 status:ok");
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
