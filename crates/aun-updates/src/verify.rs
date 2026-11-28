//! Update key lists and release manifests (wiki AUN-APP-UPDATES §3.1, §3.5, §5.3). The rules match
//! `@al-ummah/shared-types` `app-updates.ts`: a signature is checked before anything in the file is
//! trusted, any one valid signature under a trusted, non-revoked key passes, and a key list counts
//! only when the repo's root key signed it with a higher serial.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::version;

pub const REPOS: [&str; 4] = [
    "AlUmmahNowDesktop",
    "AlUmmahNowTools",
    "AlUmmahNowApple",
    "AlUmmahNowAndroid",
];
pub const MANIFEST_MAX_BYTES: usize = 256 * 1024;
const NOTES_MAX: usize = 1500;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    #[error("too large")]
    TooLarge,
    #[error("bad signature file")]
    BadSignatureFile,
    #[error("no trusted key signed it")]
    UnknownKey,
    #[error("bad signature")]
    BadSignature,
    #[error("not JSON")]
    BadJson,
    #[error("bad contents: {0}")]
    BadContents(String),
    #[error("wrong repo")]
    WrongRepo,
    #[error("wrong tag")]
    WrongTag,
    #[error("wrong channel")]
    WrongChannel,
    #[error("not newer")]
    NotNewer,
    #[error("below the highest version already verified")]
    BelowFloor,
    #[error("yanked")]
    Yanked,
}

impl VerifyError {
    /// The reason code apps report (`POST /api/v1/app-config/update-failure`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge | Self::BadContents(_) => "bad_manifest",
            Self::BadSignatureFile => "bad_signature_file",
            Self::UnknownKey => "unknown_key",
            Self::BadSignature => "bad_signature",
            Self::BadJson => "bad_json",
            Self::WrongRepo => "wrong_repo",
            Self::WrongTag => "wrong_tag",
            Self::WrongChannel => "wrong_channel",
            Self::NotNewer => "not_newer",
            Self::BelowFloor => "below_floor",
            Self::Yanked => "yanked",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignatureFile {
    signatures: Vec<SignatureEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SignatureEntry {
    key_id: String,
    sig: String,
}

/// A key's id: the first 8 hex characters of SHA-256 over the raw 32-byte public key.
pub fn key_id(public_key_b64: &str) -> Option<String> {
    let raw = B64.decode(public_key_b64).ok()?;
    if raw.len() != 32 {
        return None;
    }
    let digest = Sha256::digest(&raw);
    Some(digest.iter().take(4).map(|b| format!("{b:02x}")).collect())
}

/// The id of the first valid signature under `keys` (minus `revoked`).
fn check_signatures(
    bytes: &[u8],
    sig_text: &str,
    keys: &[String],
    revoked: &[String],
) -> Result<String, VerifyError> {
    if bytes.len() > MANIFEST_MAX_BYTES {
        return Err(VerifyError::TooLarge);
    }
    let file: SignatureFile = serde_json::from_str(sig_text).map_err(|_| VerifyError::BadSignatureFile)?;
    if file.signatures.is_empty() || file.signatures.len() > 4 {
        return Err(VerifyError::BadSignatureFile);
    }
    for s in &file.signatures {
        let id_ok = s.key_id.len() == 8
            && s.key_id
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        if !id_ok || s.sig.len() != 88 {
            return Err(VerifyError::BadSignatureFile);
        }
    }
    let mut candidate = false;
    for s in &file.signatures {
        if revoked.contains(&s.key_id) {
            continue;
        }
        let Some(key) = keys
            .iter()
            .find(|k| key_id(k).as_deref() == Some(s.key_id.as_str()))
        else {
            continue;
        };
        candidate = true;
        let Ok(raw) = B64.decode(key) else { continue };
        let Ok(raw): Result<[u8; 32], _> = raw.try_into() else {
            continue;
        };
        let Ok(vk) = VerifyingKey::from_bytes(&raw) else {
            continue;
        };
        let Ok(sig_raw) = B64.decode(&s.sig) else { continue };
        let Ok(sig) = Signature::from_slice(&sig_raw) else {
            continue;
        };
        if vk.verify(bytes, &sig).is_ok() {
            return Ok(s.key_id.clone());
        }
    }
    Err(if candidate {
        VerifyError::BadSignature
    } else {
        VerifyError::UnknownKey
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ListedKey {
    pub key_id: String,
    pub public_key: String,
}

/// `update-keys.json` (wiki §3.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyList {
    pub schema: u32,
    pub repo: String,
    pub serial: u64,
    pub keys: Vec<ListedKey>,
    #[serde(default)]
    pub revoked: Vec<String>,
    #[serde(default)]
    pub yanked: Vec<String>,
}

/// Checks a key list against the repo's root keys. `stored_serial`: the serial already accepted.
pub fn accept_key_list(
    bytes: &[u8],
    sig_text: &str,
    root_keys: &[String],
    repo: &str,
    stored_serial: Option<u64>,
) -> Result<KeyList, VerifyError> {
    check_signatures(bytes, sig_text, root_keys, &[])?;
    let list: KeyList = serde_json::from_slice(bytes).map_err(|e| {
        if e.is_syntax() || e.is_eof() {
            VerifyError::BadJson
        } else {
            VerifyError::BadContents(e.to_string())
        }
    })?;
    if list.schema != 1 || list.keys.is_empty() || list.keys.len() > 4 || list.serial == 0 {
        return Err(VerifyError::BadContents("schema, keys or serial".into()));
    }
    for k in &list.keys {
        if key_id(&k.public_key).as_deref() != Some(k.key_id.as_str()) {
            return Err(VerifyError::BadContents(format!(
                "keyId {} does not match its key",
                k.key_id
            )));
        }
    }
    if list.yanked.iter().any(|v| !version::is_valid(v)) {
        return Err(VerifyError::BadContents("yanked version".into()));
    }
    if list.repo != repo {
        return Err(VerifyError::WrongRepo);
    }
    if stored_serial.is_some_and(|s| list.serial <= s) {
        return Err(VerifyError::NotNewer);
    }
    Ok(list)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notes {
    pub en: String,
    pub ar: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub app: String,
    pub platform: String,
    #[serde(default)]
    pub arch: Option<String>,
    pub kind: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreUpload {
    pub app: String,
    pub platform: String,
    pub source: String,
}

/// `app-release.json` (wiki §3.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Manifest {
    pub schema: u32,
    pub repo: String,
    pub tag: String,
    pub version: String,
    pub channel: String,
    pub published_at: String,
    pub notes: Notes,
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub stores: Vec<StoreUpload>,
    #[serde(default)]
    pub yanked: Vec<String>,
}

/// What a manifest must match (wiki §5.3 step 3).
#[derive(Debug, Clone, Default)]
pub struct Expect<'a> {
    pub repo: &'a str,
    pub tag: Option<&'a str>,
    pub channels: Option<&'a [&'a str]>,
    /// The installed version: the manifest must be newer
    pub newer_than: Option<&'a str>,
    /// The highest version this app has verified: the manifest may not be below it
    pub floor: Option<&'a str>,
}

/// What a verifier trusts for one repo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trust {
    pub keys: Vec<String>,
    pub revoked: Vec<String>,
    pub yanked: Vec<String>,
}

fn download_prefix(repo: &str, tag: &str) -> String {
    format!("https://github.com/Al-Ummah-Now/{repo}/releases/download/{tag}/")
}

fn private_prefix(repo: &str, tag: &str) -> String {
    format!("r2://app-releases/{repo}/{tag}/")
}

fn check_manifest(m: &Manifest) -> Result<(), String> {
    if m.schema != 1 {
        return Err("schema".into());
    }
    if !REPOS.contains(&m.repo.as_str()) {
        return Err("repo".into());
    }
    let Some(tag) = crate::tags::parse_release_tag(&m.tag) else {
        return Err("tag".into());
    };
    if !version::is_valid(&m.version)
        || version::compare(&tag.version, &m.version) != Some(std::cmp::Ordering::Equal)
    {
        return Err("version must be the tag's".into());
    }
    if !(m.channel == "stable" || m.channel == "beta") {
        return Err("channel".into());
    }
    if tag.admin_only
        && (m.artifacts.iter().any(|a| a.app != "admin") || m.stores.iter().any(|s| s.app != "admin"))
    {
        return Err("an admin- tag releases only the Admin app".into());
    }
    for n in [&m.notes.en, &m.notes.ar] {
        let len = n.trim().chars().count();
        if len == 0 || len > NOTES_MAX {
            return Err("notes".into());
        }
    }
    let public = download_prefix(&m.repo, &m.tag);
    let private = private_prefix(&m.repo, &m.tag);
    for (i, a) in m.artifacts.iter().enumerate() {
        let rest_safe = |rest: &str| {
            !rest.is_empty()
                && !rest.contains(['?', '#', '\\'])
                && rest.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
        };
        let ok_public = a.url.strip_prefix(&public).is_some_and(rest_safe)
            && crate::hosts::allowed_download_url(&a.url, &m.repo);
        let ok_private = a.app == "admin" && a.url.strip_prefix(&private).is_some_and(rest_safe);
        let ok_url = ok_public || ok_private;
        let ok_hash = a.sha256.len() == 64
            && a.sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        if !ok_url || !ok_hash || a.size == 0 {
            return Err(format!("artifact {i}"));
        }
    }
    if m.yanked.iter().any(|v| !version::is_valid(v)) {
        return Err("yanked".into());
    }
    Ok(())
}

/// Checks a release manifest (wiki §5.3): the signature first, then everything else.
pub fn verify_manifest(
    bytes: &[u8],
    sig_text: &str,
    trust: &Trust,
    expect: &Expect<'_>,
) -> Result<(Manifest, String), VerifyError> {
    let key = check_signatures(bytes, sig_text, &trust.keys, &trust.revoked)?;
    let m: Manifest = serde_json::from_slice(bytes).map_err(|e| {
        if e.is_syntax() || e.is_eof() {
            VerifyError::BadJson
        } else {
            VerifyError::BadContents(e.to_string())
        }
    })?;
    check_manifest(&m).map_err(VerifyError::BadContents)?;
    if m.repo != expect.repo {
        return Err(VerifyError::WrongRepo);
    }
    if expect.tag.is_some_and(|t| t != m.tag) {
        return Err(VerifyError::WrongTag);
    }
    if expect.channels.is_some_and(|c| !c.contains(&m.channel.as_str())) {
        return Err(VerifyError::WrongChannel);
    }
    if expect
        .newer_than
        .is_some_and(|i| version::compare(&m.version, i) != Some(std::cmp::Ordering::Greater))
    {
        return Err(VerifyError::NotNewer);
    }
    if expect
        .floor
        .is_some_and(|f| version::compare(&m.version, f) == Some(std::cmp::Ordering::Less))
    {
        return Err(VerifyError::BelowFloor);
    }
    if trust
        .yanked
        .iter()
        .any(|v| version::compare(v, &m.version) == Some(std::cmp::Ordering::Equal))
    {
        return Err(VerifyError::Yanked);
    }
    Ok((m, key))
}

/// The SHA-256 a downloaded file must have (lowercase hex).
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
