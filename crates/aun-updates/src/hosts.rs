//! Where an update may be downloaded from (wiki AUN-APP-UPDATES §5.3 step 4). Every redirect hop is
//! checked, not just the first URL. The list is compiled in: moving files elsewhere takes one app
//! release that adds the new host first.

use url::Url;

/// GitHub's file hosts that release downloads redirect to.
pub const ASSET_HOSTS: [&str; 2] = [
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

/// Whether `url` may be fetched while downloading an update of `repo`: HTTPS on the default port,
/// no user info, and either this repo's GitHub release downloads or GitHub's asset hosts.
pub fn allowed_download_url(url: &str, repo: &str) -> bool {
    let Ok(u) = Url::parse(url) else { return false };
    if u.scheme() != "https" || u.port().is_some() || !u.username().is_empty() || u.password().is_some() {
        return false;
    }
    match u.host_str() {
        Some("github.com") => u
            .path()
            .starts_with(&format!("/Al-Ummah-Now/{repo}/releases/download/")),
        Some(h) => ASSET_HOSTS.contains(&h),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hops() {
        let repo = "AlUmmahNowTools";
        assert!(allowed_download_url(
            "https://github.com/Al-Ummah-Now/AlUmmahNowTools/releases/download/v1/x.nupkg",
            repo
        ));
        assert!(allowed_download_url(
            "https://release-assets.githubusercontent.com/github-production-release-asset/1?sig=x",
            repo
        ));
        assert!(allowed_download_url(
            "https://objects.githubusercontent.com/github-production-release-asset-2e65be/1",
            repo
        ));
        for bad in [
            "http://github.com/Al-Ummah-Now/AlUmmahNowTools/releases/download/v1/x",
            "https://github.com/Al-Ummah-Now/AlUmmahNowDesktop/releases/download/v1/x",
            "https://github.com/evil/AlUmmahNowTools/releases/download/v1/x",
            "https://github.com:8443/Al-Ummah-Now/AlUmmahNowTools/releases/download/v1/x",
            "https://user@github.com/Al-Ummah-Now/AlUmmahNowTools/releases/download/v1/x",
            "https://evil.example/x",
            "https://release-assets.githubusercontent.com.evil.example/x",
            "r2://app-releases/AlUmmahNowTools/v1/x",
            "not a url",
        ] {
            assert!(!allowed_download_url(bad, repo), "{bad}");
        }
    }
}
