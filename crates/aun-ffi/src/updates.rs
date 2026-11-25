//! FFI facade for app update checks (`aun-updates`; wiki AUN-APP-UPDATES §5, §6, §7). The apps fetch
//! `GET /api/v1/app-config` (with `ApiClient::request_path`), the manifest and the files with their
//! own stack; this object decides what is trusted and what to show, and keeps the state the app
//! stores between runs.

use std::sync::Mutex;

use aun_updates::decide::{self, UpdateAnswer};
use aun_updates::verify::{Expect, verify_manifest as verify};
use aun_updates::{PromptKind, UpdateState, hosts, version};

use crate::error::AunError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum UpdatePromptKind {
    /// Show nothing
    None,
    /// The small bar; closing it hides this version
    Notice,
    /// Below the minimum but no update is available to this build yet: a bar that can't be closed
    RequiredBar,
    /// Below the minimum and an update is available: the full-window screen
    Forced,
}

impl From<PromptKind> for UpdatePromptKind {
    fn from(k: PromptKind) -> Self {
        match k {
            PromptKind::None => Self::None,
            PromptKind::Notice => Self::Notice,
            PromptKind::RequiredBar => Self::RequiredBar,
            PromptKind::Forced => Self::Forced,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct UpdatePrompt {
    pub kind: UpdatePromptKind,
    pub version: Option<String>,
    pub notes_en: Option<String>,
    pub notes_ar: Option<String>,
    pub message_en: Option<String>,
    pub message_ar: Option<String>,
    /// `store`, `install`, `download`, `notice` or `none`. `notice`: say a new version is out and open
    /// the app's own compiled-in page; url, manifest_url and tag are always None. A build without root
    /// keys gets `notice` instead of `install` and `download`; a build with them never gets `notice`.
    pub action: String,
    pub url: Option<String>,
    pub manifest_url: Option<String>,
    pub tag: Option<String>,
    /// Hours until the next check (the server's `checkHours`, default 6)
    pub check_hours: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReleaseArtifact {
    pub app: String,
    pub platform: String,
    pub arch: Option<String>,
    pub kind: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// [`UpdateChecker::verify_manifest`]'s answer: the release, or why it was refused.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ManifestCheck {
    pub release: Option<VerifiedRelease>,
    /// The reason code to report (`POST /api/v1/app-config/update-failure`) when refused
    pub refused: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VerifiedRelease {
    pub version: String,
    pub tag: String,
    pub channel: String,
    pub notes_en: String,
    pub notes_ar: String,
    pub artifacts: Vec<ReleaseArtifact>,
}

/// `-1`, `0` or `1` by app-version precedence (`2026.11.22` < `2026.11.22.2`, `1.5.0-preview.2` <
/// `1.5.0`); `None` when either is malformed.
#[uniffi::export]
pub fn compare_app_versions(a: String, b: String) -> Option<i8> {
    version::compare(&a, &b).map(|o| o as i8)
}

/// One app's update state and rules. Not `Send` across awaits: every method is quick and sync.
#[derive(Debug, uniffi::Object)]
pub struct UpdateChecker {
    repo: String,
    root_keys: Vec<String>,
    installed: String,
    beta: bool,
    state: Mutex<UpdateState>,
}

#[uniffi::export]
impl UpdateChecker {
    /// `repo`: this app's repo (`AlUmmahNowTools`, ...). `root_keys`: the repo's root public keys
    /// compiled into the build. `bundled_key_list` / `_sig`: the key list the build carries, so a
    /// first run trusts the release keys of its time. `state_json`: what [`Self::state_json`] gave last run.
    #[uniffi::constructor]
    pub fn new(
        repo: String,
        root_keys: Vec<String>,
        installed_version: String,
        beta_channel: bool,
        state_json: Option<String>,
        bundled_key_list: Option<String>,
        bundled_key_list_sig: Option<String>,
    ) -> Result<std::sync::Arc<Self>, AunError> {
        if !version::is_valid(&installed_version) {
            return Err(AunError::InvalidInput {
                detail: format!("installed version {installed_version}"),
            });
        }
        let mut state = state_json
            .as_deref()
            .map(UpdateState::from_json)
            .unwrap_or_default();
        if let (Some(list), Some(sig)) = (bundled_key_list, bundled_key_list_sig) {
            // A newer serial from the build wins; an invalid bundled list is a build error worth surfacing
            state
                .accept_key_list(list.as_bytes(), &sig, &root_keys, &repo)
                .map_err(|e| AunError::InvalidInput {
                    detail: format!("bundled key list: {e}"),
                })?;
        }
        Ok(std::sync::Arc::new(Self {
            repo,
            root_keys,
            installed: installed_version,
            beta: beta_channel,
            state: Mutex::new(state),
        }))
    }

    /// What to save for the next run.
    pub fn state_json(&self) -> String {
        self.lock().to_json()
    }

    /// Reads the `update` block (JSON text of `result.update` or the whole answer's `update`), takes
    /// a newer key list from it, and says what to show. `device_has_update`: Play In-App Updates or
    /// the Microsoft Store's answer, None elsewhere.
    pub fn apply_answer(
        &self,
        update_json: String,
        device_has_update: Option<bool>,
    ) -> Result<UpdatePrompt, AunError> {
        let value: serde_json::Value =
            serde_json::from_str(&update_json).map_err(|e| AunError::InvalidInput {
                detail: e.to_string(),
            })?;
        let update = value.get("update").cloned().unwrap_or(value);
        let answer: UpdateAnswer =
            serde_json::from_value(update.clone()).map_err(|e| AunError::InvalidInput {
                detail: e.to_string(),
            })?;
        let mut state = self.lock();
        if let (Some(list), Some(sig)) = (
            update.pointer("/keys/list").and_then(|v| v.as_str()),
            update.pointer("/keys/sig").and_then(|v| v.as_str()),
        ) {
            // A key list that fails is ignored (the one already trusted stays); the server reports bad lists
            let _ = state.accept_key_list(list.as_bytes(), sig, &self.root_keys, &self.repo);
        }
        let prompt = decide::decide(
            &answer,
            &self.installed,
            &mut state,
            device_has_update,
            !self.root_keys.is_empty(),
        );
        Ok(UpdatePrompt {
            kind: prompt.kind.into(),
            version: prompt.version,
            notes_en: prompt.notes.as_ref().map(|n| n.en.clone()),
            notes_ar: prompt.notes.map(|n| n.ar),
            message_en: prompt.message.as_ref().map(|n| n.en.clone()),
            message_ar: prompt.message.map(|n| n.ar),
            action: prompt.action,
            url: prompt.url,
            manifest_url: prompt.manifest_url,
            tag: prompt.tag,
            check_hours: answer.check_hours.unwrap_or(6).clamp(1, 48),
        })
    }

    /// What to show without a network: a bar for a minimum already seen, else nothing.
    pub fn offline_prompt_kind(&self) -> UpdatePromptKind {
        decide::offline_kind(&self.installed, &self.lock()).into()
    }

    /// Checks a downloaded `app-release.json` and its `.sig` for the tag the API named. On success
    /// the replay floor rises; nothing is trusted when `refused` is set.
    pub fn verify_manifest(&self, manifest: Vec<u8>, signature: String, tag: String) -> ManifestCheck {
        if self.root_keys.is_empty() {
            // Nothing can be trusted without a root key, whatever a stored state claims
            return ManifestCheck {
                release: None,
                refused: Some("unknown_key".to_owned()),
            };
        }
        let mut state = self.lock();
        let channels: &[&str] = if self.beta {
            &["stable", "beta"]
        } else {
            &["stable"]
        };
        let floor = state.floor.clone();
        let expect = Expect {
            repo: &self.repo,
            tag: Some(&tag),
            channels: Some(channels),
            newer_than: Some(&self.installed),
            floor: floor.as_deref(),
        };
        match verify(&manifest, &signature, &state.trust(), &expect) {
            Err(e) => ManifestCheck {
                release: None,
                refused: Some(e.code().to_owned()),
            },
            Ok((m, _)) => {
                state.remember_verified(&m);
                let release = VerifiedRelease {
                    version: m.version,
                    tag: m.tag,
                    channel: m.channel,
                    notes_en: m.notes.en,
                    notes_ar: m.notes.ar,
                    artifacts: m
                        .artifacts
                        .into_iter()
                        .map(|a| ReleaseArtifact {
                            app: a.app,
                            platform: a.platform,
                            arch: a.arch,
                            kind: a.kind,
                            url: a.url,
                            sha256: a.sha256,
                            size: a.size,
                        })
                        .collect(),
                };
                ManifestCheck {
                    release: Some(release),
                    refused: None,
                }
            }
        }
    }

    /// Whether a download URL or redirect hop is allowed for this repo (check every hop).
    pub fn allowed_download_url(&self, url: String) -> bool {
        !self.root_keys.is_empty() && hosts::allowed_download_url(&url, &self.repo)
    }

    /// The user closed the notice for this version.
    pub fn dismiss(&self, version: String) {
        self.lock().dismiss(&version);
    }

    /// Desktop "Install updates automatically": the user's choice, None for the default.
    pub fn auto_install(&self) -> Option<bool> {
        self.lock().auto_install
    }

    pub fn set_auto_install(&self, value: Option<bool>) {
        self.lock().auto_install = value;
    }

    /// Record a successful check (Unix ms).
    pub fn mark_checked(&self, at_ms: i64) {
        self.lock().last_check_ms = Some(at_ms);
    }

    pub fn last_check_ms(&self) -> Option<i64> {
        self.lock().last_check_ms
    }
}

impl UpdateChecker {
    fn lock(&self) -> std::sync::MutexGuard<'_, UpdateState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
