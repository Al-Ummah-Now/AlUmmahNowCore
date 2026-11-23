//! UniFFI bindings for short share links (`aun_common::short_links`, Epic 22 `E22-S7`).

use aun_common::short_links as core;

/// Kind of entity addressed by a short link on `alummahnow.link`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ShortLinkTarget {
    Video { id: String },
    Short { id: String },
    Channel { id: String },
    Handle { handle: String },
    Post { id: String },
    Playlist { id: String },
    Live { id: String },
    Home,
}

impl From<ShortLinkTarget> for core::ShortLinkTarget {
    fn from(t: ShortLinkTarget) -> Self {
        match t {
            ShortLinkTarget::Video { id } => Self::Video(id),
            ShortLinkTarget::Short { id } => Self::Short(id),
            ShortLinkTarget::Channel { id } => Self::Channel(id),
            ShortLinkTarget::Handle { handle } => Self::Handle(handle),
            ShortLinkTarget::Post { id } => Self::Post(id),
            ShortLinkTarget::Playlist { id } => Self::Playlist(id),
            ShortLinkTarget::Live { id } => Self::Live(id),
            ShortLinkTarget::Home => Self::Home,
        }
    }
}

impl From<core::ShortLinkTarget> for ShortLinkTarget {
    fn from(t: core::ShortLinkTarget) -> Self {
        match t {
            core::ShortLinkTarget::Video(id) => Self::Video { id },
            core::ShortLinkTarget::Short(id) => Self::Short { id },
            core::ShortLinkTarget::Channel(id) => Self::Channel { id },
            core::ShortLinkTarget::Handle(handle) => Self::Handle { handle },
            core::ShortLinkTarget::Post(id) => Self::Post { id },
            core::ShortLinkTarget::Playlist(id) => Self::Playlist { id },
            core::ShortLinkTarget::Live(id) => Self::Live { id },
            core::ShortLinkTarget::Home => Self::Home,
        }
    }
}

/// Optional query parameters for [`short_link_for`] and [`share_short_link`].
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct ShortLinkOptions {
    #[uniffi(default = None)]
    pub t_seconds: Option<u64>,
    #[uniffi(default = None)]
    pub playlist_id: Option<String>,
    #[uniffi(default = None)]
    pub playlist_index: Option<u32>,
    #[uniffi(default = None)]
    pub lang: Option<String>,
}

impl From<ShortLinkOptions> for core::ShortLinkOptions {
    fn from(o: ShortLinkOptions) -> Self {
        Self {
            t_seconds: o.t_seconds,
            playlist_id: o.playlist_id,
            playlist_index: o.playlist_index,
            lang: o.lang,
        }
    }
}

/// Resolves `https://[qa.|stage.]alummahnow.link` for an environment's site or API URL, or `None`.
#[uniffi::export]
pub fn short_link_origin_for(site_or_api_url: String) -> Option<String> {
    core::short_link_origin_for(&site_or_api_url)
}

/// Builds a short link (`https://[qa.|stage.]alummahnow.link/...`) for `target`, or `None` when
/// `site_or_api_url` has no short-link domain or `target` carries an invalid ID/handle.
#[uniffi::export]
pub fn short_link_for(
    site_or_api_url: String,
    target: ShortLinkTarget,
    options: ShortLinkOptions,
) -> Option<String> {
    core::short_link_url(&site_or_api_url, &target.into(), &options.into())
}

/// Builds a short link for a share sheet, falling back to `fallback_url` when unavailable.
#[uniffi::export]
pub fn share_short_link(
    site_or_api_url: String,
    fallback_url: String,
    target: ShortLinkTarget,
    options: ShortLinkOptions,
) -> String {
    core::share_short_link(&site_or_api_url, &fallback_url, &target.into(), &options.into())
}

/// Parses the path of an `alummahnow.link` URL (`/v/…`, `/s/…`, `/e/…`, `/@…`, `/c/…`, `/p/…`, `/l/…`).
#[uniffi::export]
pub fn parse_short_link_path(pathname: String) -> Option<ShortLinkTarget> {
    core::parse_short_link_path(&pathname).map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_short_links_roundtrip() {
        let link = short_link_for(
            "https://videos.api.qa.alummahnow.net".into(),
            ShortLinkTarget::Live {
                id: "str_halqa_1".into(),
            },
            ShortLinkOptions {
                lang: Some("ar".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            link.as_deref(),
            Some("https://qa.alummahnow.link/e/str_halqa_1?lang=ar")
        );
        assert_eq!(
            parse_short_link_path("/e/str_halqa_1".into()),
            Some(ShortLinkTarget::Live {
                id: "str_halqa_1".into()
            })
        );
    }
}
