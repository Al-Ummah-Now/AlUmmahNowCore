//! What to show after an update check (wiki AUN-APP-UPDATES §3.7, §5, §12): nothing, a dismissible
//! notice, a bar that can't be closed ("will stop working soon"), or the forced screen. The forced
//! screen needs the app's own source to confirm an update exists, so no one is stranded.

use serde::Deserialize;

use crate::state::UpdateState;
use crate::verify::Notes;
use crate::version;

/// The `update` block of `GET /api/v1/app-config` (the fields this crate reads).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAnswer {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub minimum_version: String,
    #[serde(default)]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub notes: Option<Notes>,
    #[serde(default)]
    pub message: Option<Notes>,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub manifest_url: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub available: Option<bool>,
    #[serde(default)]
    pub check_hours: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    None,
    /// The small bar; closing it hides this version
    Notice,
    /// Below the minimum, but this build's source has no update yet: a bar that can't be closed
    RequiredBar,
    /// Below the minimum and an update is there: the full-window screen
    Forced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    pub version: Option<String>,
    pub notes: Option<Notes>,
    pub message: Option<Notes>,
    pub action: String,
    pub url: Option<String>,
    pub manifest_url: Option<String>,
    pub tag: Option<String>,
}

impl Prompt {
    fn none() -> Self {
        Self {
            kind: PromptKind::None,
            version: None,
            notes: None,
            message: None,
            action: "none".into(),
            url: None,
            manifest_url: None,
            tag: None,
        }
    }
}

/// `device_has_update`: what Play In-App Updates or the Microsoft Store said (None when this build
/// has no device store). Updates `state.last_required_minimum`.
pub fn decide(
    answer: &UpdateAnswer,
    installed: &str,
    state: &mut UpdateState,
    device_has_update: Option<bool>,
) -> Prompt {
    if !answer.enabled {
        state.last_required_minimum = None;
        return Prompt::none();
    }
    let below = version::compare(installed, &answer.minimum_version) == Some(std::cmp::Ordering::Less);
    state.last_required_minimum = below.then(|| answer.minimum_version.clone());
    let has_action = answer.action != "none";
    // Can this build get the update now? The server knows (true/false) or the device does (null)
    let obtainable = has_action
        && answer.latest_version.is_some()
        && match answer.available {
            Some(a) => a && device_has_update != Some(false),
            None => device_has_update == Some(true),
        };
    let base = Prompt {
        kind: PromptKind::None,
        version: answer.latest_version.clone(),
        notes: answer.notes.clone(),
        message: answer.message.clone(),
        action: answer.action.clone(),
        url: answer.url.clone(),
        manifest_url: answer.manifest_url.clone(),
        tag: answer.tag.clone(),
    };
    if below {
        return Prompt {
            kind: if obtainable {
                PromptKind::Forced
            } else {
                PromptKind::RequiredBar
            },
            ..base
        };
    }
    let behind = answer
        .latest_version
        .as_deref()
        .is_some_and(|l| version::compare(installed, l) == Some(std::cmp::Ordering::Less));
    if answer.status != "recommended" || !behind || !obtainable {
        return Prompt::none();
    }
    if answer
        .latest_version
        .as_deref()
        .is_some_and(|l| state.is_dismissed(l))
    {
        return Prompt::none();
    }
    Prompt {
        kind: PromptKind::Notice,
        ..base
    }
}

/// Offline: the forced screen holds only for a minimum the app has seen (never one it hasn't).
pub fn offline_kind(installed: &str, state: &UpdateState) -> PromptKind {
    match &state.last_required_minimum {
        Some(min) if version::compare(installed, min) == Some(std::cmp::Ordering::Less) => {
            PromptKind::RequiredBar
        }
        _ => PromptKind::None,
    }
}
