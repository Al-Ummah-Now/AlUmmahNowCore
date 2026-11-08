//! Platform adapters native apps implement (docs/PLAN.md §6), bridged to the `aun_common` traits.

use std::sync::Arc;

use crate::AunError;

/// OS-owned secret storage: Keychain (Apple), Android Keystore, Windows Credential Locker,
/// libsecret (Linux). Implemented natively and passed to the core.
#[uniffi::export(with_foreign)]
pub trait SecureStorage: Send + Sync {
    fn get(&self, key: String) -> Result<Option<Vec<u8>>, AunError>;
    fn set(&self, key: String, value: Vec<u8>) -> Result<(), AunError>;
    fn delete(&self, key: String) -> Result<(), AunError>;
    /// Keys that start with `prefix`.
    fn list(&self, prefix: String) -> Result<Vec<String>, AunError>;
}

/// Adapts a native [`SecureStorage`] to the core trait.
pub(crate) struct ForeignSecureStorage(pub Arc<dyn SecureStorage>);

impl std::fmt::Debug for ForeignSecureStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ForeignSecureStorage")
    }
}

impl aun_common::platform::SecureStorage for ForeignSecureStorage {
    fn get(&self, key: &str) -> aun_common::Result<Option<Vec<u8>>> {
        Ok(self.0.get(key.to_owned())?)
    }
    fn set(&self, key: &str, value: &[u8]) -> aun_common::Result<()> {
        Ok(self.0.set(key.to_owned(), value.to_vec())?)
    }
    fn delete(&self, key: &str) -> aun_common::Result<()> {
        Ok(self.0.delete(key.to_owned())?)
    }
    fn list(&self, prefix: &str) -> aun_common::Result<Vec<String>> {
        Ok(self.0.list(prefix.to_owned())?)
    }
}

/// Network state from the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct NetworkState {
    pub online: bool,
    pub metered: bool,
}

/// Connectivity reported by NWPathMonitor / ConnectivityManager / NetworkInformation.
#[uniffi::export(with_foreign)]
pub trait Connectivity: Send + Sync {
    fn current(&self) -> NetworkState;
}

pub(crate) struct ForeignConnectivity(pub Arc<dyn Connectivity>);

impl std::fmt::Debug for ForeignConnectivity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ForeignConnectivity")
    }
}

impl aun_common::platform::Connectivity for ForeignConnectivity {
    fn current(&self) -> aun_common::platform::NetworkState {
        let s = self.0.current();
        aun_common::platform::NetworkState {
            online: s.online,
            metered: s.metered,
        }
    }
}
