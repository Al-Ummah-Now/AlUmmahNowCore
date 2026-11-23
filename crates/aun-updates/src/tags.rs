//! Release tags (wiki AUN-APP-UPDATES §2.1): `v` for any app, `admin-v` for AlUmmahNowApple's staff Admin app,
//! `admin-windows-v` for AlUmmahNowDesktop's Admin for Windows. The same rule as `@al-ummah/shared-types`
//! `parseAppReleaseTag` and the release tool; shared cases in `spec/app-updates/tags.json`.

use crate::version;

/// Longest first, so `admin-windows-v` wins over `admin-v`.
pub const TAG_PREFIXES: [&str; 3] = ["admin-windows-v", "admin-v", "v"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTag {
    pub prefix: &'static str,
    /// The tag's version: a trailing `-N` of one or two digits is the 4th part (`admin-v1.2.3-2` → `1.2.3.2`)
    pub version: String,
    /// An `admin-` tag releases only the Admin app
    pub admin_only: bool,
}

pub fn parse_release_tag(tag: &str) -> Option<ReleaseTag> {
    let prefix = TAG_PREFIXES.into_iter().find(|p| tag.starts_with(p))?;
    let bare = &tag[prefix.len()..];
    if bare.is_empty()
        || bare.len() > 64
        || !bare
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".+-".contains(c))
    {
        return None;
    }
    let version = match bare.rsplit_once('-') {
        Some((core, n))
            if !n.is_empty()
                && n.len() <= 2
                && n.chars().all(|c| c.is_ascii_digit())
                && core.split('.').count() == 3
                && core
                    .split('.')
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) =>
        {
            format!("{core}.{n}")
        }
        _ => bare.to_owned(),
    };
    version::is_valid(&version).then_some(ReleaseTag {
        prefix,
        version,
        admin_only: prefix != "v",
    })
}
