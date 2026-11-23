//! What an app remembers between update checks (wiki AUN-APP-UPDATES §3.5, §5.3). Stored by the app
//! as the JSON string [`UpdateState::to_json`] gives; anything unreadable starts fresh, except that
//! a fresh state still trusts the key list the build carries.

use serde::{Deserialize, Serialize};

use crate::verify::{self, KeyList, Trust, VerifyError};
use crate::version;

const DISMISSED_KEPT: usize = 10;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    /// Serial of the accepted key list
    #[serde(default)]
    pub serial: u64,
    /// Release public keys of the accepted list (base64)
    #[serde(default)]
    pub keys: Vec<String>,
    /// Every key id ever revoked, from any list (never forgotten)
    #[serde(default)]
    pub revoked: Vec<String>,
    /// Every version ever yanked, from key lists and verified manifests (never forgotten)
    #[serde(default)]
    pub yanked: Vec<String>,
    /// The highest version this app has verified in a manifest (replay floor)
    #[serde(default)]
    pub floor: Option<String>,
    /// Versions whose notice the user closed (newest last)
    #[serde(default)]
    pub dismissed: Vec<String>,
    /// Desktop setting "Install updates automatically"; None: the admin default
    #[serde(default)]
    pub auto_install: Option<bool>,
    /// Unix ms of the last successful check
    #[serde(default)]
    pub last_check_ms: Option<i64>,
    /// The last `required` the app saw, so it holds offline (AUN-FEATURES `lastKnownElseHidden`)
    #[serde(default)]
    pub last_required_minimum: Option<String>,
}

impl UpdateState {
    pub fn from_json(text: &str) -> Self {
        serde_json::from_str(text).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned())
    }

    pub fn trust(&self) -> Trust {
        Trust {
            keys: self
                .keys
                .iter()
                .filter(|k| verify::key_id(k).is_none_or(|id| !self.revoked.contains(&id)))
                .cloned()
                .collect(),
            revoked: self.revoked.clone(),
            yanked: self.yanked.clone(),
        }
    }

    fn absorb(&mut self, list: KeyList) {
        for id in list.revoked {
            if !self.revoked.contains(&id) {
                self.revoked.push(id);
            }
        }
        self.add_yanked(list.yanked);
        self.serial = list.serial;
        self.keys = list
            .keys
            .into_iter()
            .filter(|k| !self.revoked.contains(&k.key_id))
            .map(|k| k.public_key)
            .collect();
    }

    pub(crate) fn add_yanked(&mut self, versions: Vec<String>) {
        for v in versions {
            if !self.yanked.contains(&v) {
                self.yanked.push(v);
            }
        }
    }

    /// Takes a newer root-signed key list. `Ok(false)`: the same or an older list (nothing to do).
    pub fn accept_key_list(
        &mut self,
        bytes: &[u8],
        sig_text: &str,
        root_keys: &[String],
        repo: &str,
    ) -> Result<bool, VerifyError> {
        let stored = (self.serial > 0).then_some(self.serial);
        match verify::accept_key_list(bytes, sig_text, root_keys, repo, stored) {
            Ok(list) => {
                self.absorb(list);
                Ok(true)
            }
            Err(VerifyError::NotNewer) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// After a manifest verified: raise the floor and remember its yanked versions.
    pub fn remember_verified(&mut self, manifest: &verify::Manifest) {
        let higher = self
            .floor
            .as_deref()
            .is_none_or(|f| version::compare(&manifest.version, f) == Some(std::cmp::Ordering::Greater));
        if higher {
            self.floor = Some(manifest.version.clone());
        }
        self.add_yanked(manifest.yanked.clone());
    }

    pub fn dismiss(&mut self, version: &str) {
        self.dismissed.retain(|v| v != version);
        self.dismissed.push(version.to_owned());
        let extra = self.dismissed.len().saturating_sub(DISMISSED_KEPT);
        self.dismissed.drain(..extra);
    }

    pub fn is_dismissed(&self, version: &str) -> bool {
        self.dismissed.iter().any(|v| v == version)
    }
}
