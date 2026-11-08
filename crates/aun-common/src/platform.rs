//! Platform-adapter traits shared by several crates. Native apps implement them through UniFFI
//! callback interfaces (see `aun-ffi`); the in-memory implementations here back the tests and
//! headless tools. Module-specific adapters live with their module:
//! `aun_upload::BackgroundTransfer`, `aun_media::HardwareEncoder`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Result;

/// Secret storage owned by the OS: Keychain, Android Keystore, Windows Credential Locker, libsecret.
/// Keys are namespaced by the caller (e.g. `auth/accounts/<id>`); values are opaque bytes.
pub trait SecureStorage: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    fn set(&self, key: &str, value: &[u8]) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
    /// Lists the keys that start with `prefix`.
    fn list(&self, prefix: &str) -> Result<Vec<String>>;
}

/// Volatile [`SecureStorage`] for tests and tools. Never use it in an app.
#[derive(Debug, Default)]
pub struct MemorySecureStorage {
    items: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemorySecureStorage {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, Vec<u8>>>> {
        self.items
            .lock()
            .map_err(|_| crate::Error::Internal("secure storage lock poisoned".into()))
    }
}

impl SecureStorage for MemorySecureStorage {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.lock()?.get(key).cloned())
    }
    fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        self.lock()?.insert(key.to_owned(), value.to_vec());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.lock()?.remove(key);
        Ok(())
    }
    fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys: Vec<String> = self
            .lock()?
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        keys.sort();
        Ok(keys)
    }
}

/// Network state reported by the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkState {
    pub online: bool,
    /// Cellular or a metered Wi-Fi: honour "Wi-Fi only" settings.
    pub metered: bool,
}

/// Connectivity reported by the OS (NWPathMonitor, ConnectivityManager, …).
pub trait Connectivity: Send + Sync {
    fn current(&self) -> NetworkState;
}

/// Always online and unmetered; the default when no adapter is installed.
#[derive(Debug, Default, Clone, Copy)]
pub struct AlwaysOnline;

impl Connectivity for AlwaysOnline {
    fn current(&self) -> NetworkState {
        NetworkState {
            online: true,
            metered: false,
        }
    }
}

/// Kinds of local notification the core asks the app to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationKind {
    UploadFinished,
    UploadFailed,
    EncodeFinished,
    TranscriptionFinished,
}

/// Local notifications (UserNotifications, NotificationManager, AppNotification, portal).
pub trait NotificationSink: Send + Sync {
    fn notify(&self, kind: NotificationKind, title: &str, body: &str);
}

/// Wall clock, injectable so sync/LWW and token-expiry logic can be tested deterministically.
pub trait Clock: Send + Sync {
    /// Unix time in milliseconds.
    fn now_ms(&self) -> i64;
}

/// The system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

/// A clock tests can move by hand.
#[derive(Debug, Default)]
pub struct ManualClock(std::sync::atomic::AtomicI64);

impl ManualClock {
    pub fn new(start_ms: i64) -> Self {
        Self(std::sync::atomic::AtomicI64::new(start_ms))
    }
    pub fn advance(&self, ms: i64) {
        self.0.fetch_add(ms, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn set(&self, ms: i64) {
        self.0.store(ms, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_storage_roundtrip() {
        let s = MemorySecureStorage::new();
        assert_eq!(s.get("a").unwrap(), None);
        s.set("auth/1", b"x").unwrap();
        s.set("auth/2", b"y").unwrap();
        s.set("other", b"z").unwrap();
        assert_eq!(s.get("auth/1").unwrap(), Some(b"x".to_vec()));
        assert_eq!(s.list("auth/").unwrap(), vec!["auth/1", "auth/2"]);
        s.delete("auth/1").unwrap();
        assert_eq!(s.list("auth/").unwrap(), vec!["auth/2"]);
    }

    #[test]
    fn manual_clock() {
        let c = ManualClock::new(1_000);
        c.advance(500);
        assert_eq!(c.now_ms(), 1_500);
        assert!(SystemClock.now_ms() > 1_700_000_000_000);
    }
}
