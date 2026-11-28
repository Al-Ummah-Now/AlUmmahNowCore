//! Short share links on `alummahnow.link` (`packages/shared-types/src/short-links.ts`, Epic 22).
//!
//! Supported paths:
//! - `/v/<videoId>`      → watch page (`?t=<seconds>`, `?list=<playlistId>[&index=<n>]`)
//! - `/s/<videoId>`      → Short
//! - `/@<handle>`        → channel by `@handle`
//! - `/c/<channelId>`    → channel by ID
//! - `/p/<postId>`       → community post
//! - `/l/<playlistId>`   → playlist
//! - `/e/<streamId>`     → live stream / scheduled online event
//! - `/`                 → home
//!
//! Host rule (`short_link_origin_for`):
//! - `qa` hosts → `https://qa.alummahnow.link`
//! - `stage` hosts → `https://stage.alummahnow.link`
//! - production `alummahnow.net` / `alummahnow.com` hosts (excluding `pr-*`, `preview`, `dev`) → `https://alummahnow.link`
//! - localhost / preview hosts → `None` (callers fall back to the full page URL).

/// Target addressed by a short link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortLinkTarget {
    Video(String),
    Short(String),
    Channel(String),
    Handle(String),
    Post(String),
    Playlist(String),
    Live(String),
    Home,
}

/// Optional query parameters carried by a short link.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortLinkOptions {
    /// Start position in seconds (`>= 1` only).
    pub t_seconds: Option<u64>,
    /// Public playlist ID playing alongside the video.
    pub playlist_id: Option<String>,
    /// Zero-based index inside `playlist_id`.
    pub playlist_index: Option<u32>,
    /// Language hint (`"ar"` appends `lang=ar`; `"en"` is omitted by [`share_short_link`]).
    pub lang: Option<String>,
}

/// Validates `[A-Za-z0-9][A-Za-z0-9_-]{0,63}` (matching `ID_PATTERN` in `short-links.ts`).
pub fn is_valid_short_link_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

/// Validates channel handle `[A-Za-z0-9](?:[A-Za-z0-9._-]{1,28})[A-Za-z0-9]` (3..=30 ASCII chars).
pub fn is_valid_short_link_handle(handle: &str) -> bool {
    let bytes = handle.as_bytes();
    if !(3..=30).contains(&bytes.len()) {
        return false;
    }
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
        return false;
    }
    bytes[1..bytes.len() - 1]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'.' || *b == b'_' || *b == b'-')
}

fn is_valid_hostname(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn extract_host(site_host_or_origin: &str) -> Option<String> {
    let trimmed = site_host_or_origin.trim();
    if trimmed.is_empty() {
        return None;
    }
    let after_scheme = if let Some(idx) = trimmed.find("://") {
        &trimmed[idx + 3..]
    } else {
        trimmed
    };
    let authority = after_scheme.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = authority.split(':').next()?.to_ascii_lowercase();
    if is_valid_hostname(&host) {
        Some(host)
    } else {
        None
    }
}

fn is_preview_label(label: &str) -> bool {
    if label == "preview" || label == "dev" {
        return true;
    }
    if let Some(rest) = label.strip_prefix("pr") {
        let digits = rest.strip_prefix('-').unwrap_or(rest);
        return !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    }
    false
}

/// Resolves the short-link origin (`https://[qa.|stage.]alummahnow.link`) for an environment's site
/// or API URL, or `None` where short links are not used (e.g. `localhost` or PR previews).
pub fn short_link_origin_for(site_host_or_origin: &str) -> Option<String> {
    let host = extract_host(site_host_or_origin)?;
    if host == "alummahnow.link" || host.ends_with(".alummahnow.link") {
        return Some(format!("https://{host}"));
    }
    let is_platform_domain = host == "alummahnow.net"
        || host.ends_with(".alummahnow.net")
        || host == "alummahnow.com"
        || host.ends_with(".alummahnow.com");
    if !is_platform_domain {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.contains(&"qa") {
        return Some("https://qa.alummahnow.link".to_owned());
    }
    if labels.contains(&"stage") {
        return Some("https://stage.alummahnow.link".to_owned());
    }
    if labels.iter().copied().any(is_preview_label) {
        return None;
    }
    Some("https://alummahnow.link".to_owned())
}

/// Builds the path segment of a short link (without host), or `None` if the target ID/handle is invalid.
pub fn short_link_path(target: &ShortLinkTarget) -> Option<String> {
    match target {
        ShortLinkTarget::Home => Some("/".to_owned()),
        ShortLinkTarget::Handle(raw) => {
            let handle = raw.strip_prefix('@').unwrap_or(raw);
            is_valid_short_link_handle(handle).then(|| format!("/@{handle}"))
        }
        ShortLinkTarget::Video(id) => is_valid_short_link_id(id).then(|| format!("/v/{id}")),
        ShortLinkTarget::Short(id) => is_valid_short_link_id(id).then(|| format!("/s/{id}")),
        ShortLinkTarget::Channel(id) => is_valid_short_link_id(id).then(|| format!("/c/{id}")),
        ShortLinkTarget::Post(id) => is_valid_short_link_id(id).then(|| format!("/p/{id}")),
        ShortLinkTarget::Playlist(id) => is_valid_short_link_id(id).then(|| format!("/l/{id}")),
        ShortLinkTarget::Live(id) => is_valid_short_link_id(id).then(|| format!("/e/{id}")),
    }
}

/// Builds a full short link for `origin_or_site`, or `None` when the host has no short-link domain
/// or the target ID is invalid.
///
/// Mirrors `shareLink` on web (`Apple#31`, `Android#20`, `Desktop#39`): appends `lang=ar` when
/// `options.lang` is Arabic (`"ar"` / `"ar-*"`), and omits `lang` for English so links stay short.
pub fn short_link_url(
    origin_or_site: &str,
    target: &ShortLinkTarget,
    options: &ShortLinkOptions,
) -> Option<String> {
    let origin = short_link_origin_for(origin_or_site)?;
    let path = short_link_path(target)?;
    let mut params: Vec<(String, String)> = Vec::new();

    if let Some(list_id) = options.playlist_id.as_deref()
        && is_valid_short_link_id(list_id)
    {
        params.push(("list".to_owned(), list_id.to_owned()));
        if let Some(idx) = options.playlist_index {
            params.push(("index".to_owned(), idx.to_string()));
        }
    }
    if let Some(t) = options.t_seconds
        && t >= 1
    {
        params.push(("t".to_owned(), t.to_string()));
    }
    if let Some(lang) = options.lang.as_deref() {
        let base = lang.split(['-', '_']).next().unwrap_or(lang);
        if base.eq_ignore_ascii_case("ar") {
            params.push(("lang".to_owned(), "ar".to_owned()));
        }
    }

    let mut url = format!("{origin}{path}");
    if !params.is_empty() {
        url.push('?');
        for (i, (k, v)) in params.iter().enumerate() {
            if i > 0 {
                url.push('&');
            }
            url.push_str(k);
            url.push('=');
            url.push_str(v);
        }
    }
    Some(url)
}

/// Returns the short link when available for `site_or_api_url` and `target`, else `fallback_url`.
pub fn share_short_link(
    site_or_api_url: &str,
    fallback_url: &str,
    target: &ShortLinkTarget,
    options: &ShortLinkOptions,
) -> String {
    short_link_url(site_or_api_url, target, options).unwrap_or_else(|| fallback_url.to_owned())
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = (bytes[i + 1] as char).to_digit(16)?;
            let lo = (bytes[i + 2] as char).to_digit(16)?;
            out.push(((hi << 4) | lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Parses the path of an `alummahnow.link` URL into its [`ShortLinkTarget`], or `None` if unknown.
pub fn parse_short_link_path(pathname: &str) -> Option<ShortLinkTarget> {
    let before_query = pathname.split(['?', '#']).next().unwrap_or(pathname);
    let mut path = percent_decode(before_query)?;
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    if path.is_empty() || path == "/" {
        return Some(ShortLinkTarget::Home);
    }
    if let Some(handle) = path.strip_prefix("/@") {
        if !handle.contains('/') && is_valid_short_link_handle(handle) {
            return Some(ShortLinkTarget::Handle(handle.to_owned()));
        }
        return None;
    }
    let mut segs = path.strip_prefix('/')?.split('/');
    let prefix = segs.next()?;
    let id = segs.next()?;
    if segs.next().is_some() || !is_valid_short_link_id(id) {
        return None;
    }
    match prefix {
        "v" => Some(ShortLinkTarget::Video(id.to_owned())),
        "s" => Some(ShortLinkTarget::Short(id.to_owned())),
        "c" => Some(ShortLinkTarget::Channel(id.to_owned())),
        "p" => Some(ShortLinkTarget::Post(id.to_owned())),
        "l" => Some(ShortLinkTarget::Playlist(id.to_owned())),
        "e" => Some(ShortLinkTarget::Live(id.to_owned())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_origins_across_environments() {
        assert_eq!(
            short_link_origin_for("https://videos.api.alummahnow.net").as_deref(),
            Some("https://alummahnow.link")
        );
        assert_eq!(
            short_link_origin_for("https://videos.api.qa.alummahnow.net").as_deref(),
            Some("https://qa.alummahnow.link")
        );
        assert_eq!(
            short_link_origin_for("https://videos.stage.alummahnow.net").as_deref(),
            Some("https://stage.alummahnow.link")
        );
        assert_eq!(short_link_origin_for("http://127.0.0.1:8787"), None);
        assert_eq!(
            short_link_origin_for("https://pr-1813.videos.alummahnow.net"),
            None
        );
        for bad in [
            "https://evil\r\n.alummahnow.link",
            "https://a b.alummahnow.link",
            "https://foo..alummahnow.link",
            "https://.alummahnow.link",
            "https://-bad.alummahnow.link",
        ] {
            assert_eq!(short_link_origin_for(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn builds_and_parses_all_short_link_kinds() {
        let opts_ar = ShortLinkOptions {
            t_seconds: Some(95),
            playlist_id: Some("pl_ramadan_1".into()),
            playlist_index: Some(2),
            lang: Some("ar".into()),
        };
        assert_eq!(
            short_link_url(
                "https://videos.api.qa.alummahnow.net",
                &ShortLinkTarget::Video("vid_123".into()),
                &opts_ar
            )
            .as_deref(),
            Some("https://qa.alummahnow.link/v/vid_123?list=pl_ramadan_1&index=2&t=95&lang=ar")
        );

        let opts_en = ShortLinkOptions {
            lang: Some("en".into()),
            ..Default::default()
        };
        assert_eq!(
            short_link_url(
                "https://videos.alummahnow.net",
                &ShortLinkTarget::Live("str_halqa_9".into()),
                &opts_en
            )
            .as_deref(),
            Some("https://alummahnow.link/e/str_halqa_9")
        );
        assert_eq!(
            short_link_url(
                "https://videos.alummahnow.net",
                &ShortLinkTarget::Handle("@almanar".into()),
                &opts_en
            )
            .as_deref(),
            Some("https://alummahnow.link/@almanar")
        );

        assert_eq!(
            parse_short_link_path("/e/str_halqa_9/"),
            Some(ShortLinkTarget::Live("str_halqa_9".into()))
        );
        assert_eq!(
            parse_short_link_path("/@almanar"),
            Some(ShortLinkTarget::Handle("almanar".into()))
        );
        assert_eq!(
            parse_short_link_path("/l/pl_42"),
            Some(ShortLinkTarget::Playlist("pl_42".into()))
        );
        assert_eq!(parse_short_link_path("/x/bad"), None);
        assert_eq!(
            share_short_link(
                "http://localhost:3001",
                "http://localhost:3001/en/video/vid_1",
                &ShortLinkTarget::Video("vid_1".into()),
                &opts_en
            ),
            "http://localhost:3001/en/video/vid_1"
        );
    }
}
