//! Self-test entry points the binding smoke tests (Swift, Kotlin, C#) call to prove that sync
//! calls, async calls, errors and callbacks all work across the boundary on every target.
//! Apps may also call them from a hidden diagnostics screen.

use std::sync::Arc;
use std::time::Duration;

use crate::AunError;
use crate::platform::{Connectivity, ForeignConnectivity, ForeignSecureStorage, NetworkState, SecureStorage};

/// Receives events from a self-test run (exercises Rust → native callbacks).
#[uniffi::export(with_foreign)]
pub trait SelfTestListener: Send + Sync {
    fn on_event(&self, index: u32, message: String);
}

/// Waits `delay_ms` on the core's tokio runtime, then returns `message` reversed.
#[uniffi::export(async_runtime = "tokio")]
pub async fn selftest_async_echo(message: String, delay_ms: u64) -> String {
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    message.chars().rev().collect()
}

/// Always throws the error named by `kind` (`"network"`, `"unauthorized"`, `"rate_limited"`,
/// anything else → `Internal`). Used to check error mapping in each language.
#[uniffi::export]
pub fn selftest_throw(kind: String) -> Result<(), AunError> {
    Err(match kind.as_str() {
        "network" => AunError::Network {
            detail: "connection reset".into(),
        },
        "unauthorized" => AunError::Unauthorized {
            detail: "sign in again".into(),
        },
        "rate_limited" => AunError::RateLimited {
            retry_after_ms: Some(1_500),
        },
        other => AunError::Internal {
            detail: format!("selftest: {other}"),
        },
    })
}

/// Calls `listener.on_event` `count` times from a tokio worker thread, then returns `count`.
#[uniffi::export(async_runtime = "tokio")]
pub async fn selftest_callbacks(listener: Arc<dyn SelfTestListener>, count: u32) -> u32 {
    let handle = tokio::spawn(async move {
        for i in 0..count {
            listener.on_event(i, format!("event {i}"));
        }
        count
    });
    handle.await.unwrap_or(0)
}

/// Writes, reads, lists and deletes a probe key through a native [`SecureStorage`] using the core
/// trait, returning the bytes it read back. Proves the adapter works end to end.
#[uniffi::export]
pub fn selftest_secure_storage(storage: Arc<dyn SecureStorage>) -> Result<Vec<u8>, AunError> {
    use aun_common::platform::SecureStorage as _;
    let s = ForeignSecureStorage(storage);
    let key = "aun-core/selftest/probe";
    let value = "سلام".as_bytes();
    s.set(key, value)?;
    let read = s.get(key)?.ok_or_else(|| AunError::Storage {
        detail: "probe key missing after set".into(),
    })?;
    if !s.list("aun-core/selftest/")?.iter().any(|k| k == key) {
        return Err(AunError::Storage {
            detail: "probe key not listed".into(),
        });
    }
    s.delete(key)?;
    Ok(read)
}

/// Reads the state from a native [`Connectivity`] through the core trait.
#[uniffi::export]
pub fn selftest_connectivity(connectivity: Arc<dyn Connectivity>) -> NetworkState {
    use aun_common::platform::Connectivity as _;
    let s = ForeignConnectivity(connectivity).current();
    NetworkState {
        online: s.online,
        metered: s.metered,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[tokio::test]
    async fn async_echo() {
        assert_eq!(selftest_async_echo("abc".into(), 1).await, "cba");
    }

    #[test]
    fn throws() {
        assert!(matches!(
            selftest_throw("rate_limited".into()),
            Err(AunError::RateLimited {
                retry_after_ms: Some(1500)
            })
        ));
    }

    struct Collect(Mutex<Vec<u32>>);
    impl SelfTestListener for Collect {
        fn on_event(&self, index: u32, _message: String) {
            self.0.lock().unwrap().push(index);
        }
    }

    #[tokio::test]
    async fn callbacks() {
        let l = Arc::new(Collect(Mutex::new(vec![])));
        assert_eq!(selftest_callbacks(l.clone(), 3).await, 3);
        assert_eq!(*l.0.lock().unwrap(), vec![0, 1, 2]);
    }

    struct Mem(aun_common::platform::MemorySecureStorage);
    impl SecureStorage for Mem {
        fn get(&self, key: String) -> Result<Option<Vec<u8>>, AunError> {
            Ok(aun_common::platform::SecureStorage::get(&self.0, &key)?)
        }
        fn set(&self, key: String, value: Vec<u8>) -> Result<(), AunError> {
            Ok(aun_common::platform::SecureStorage::set(&self.0, &key, &value)?)
        }
        fn delete(&self, key: String) -> Result<(), AunError> {
            Ok(aun_common::platform::SecureStorage::delete(&self.0, &key)?)
        }
        fn list(&self, prefix: String) -> Result<Vec<String>, AunError> {
            Ok(aun_common::platform::SecureStorage::list(&self.0, &prefix)?)
        }
    }

    #[test]
    fn secure_storage_probe() {
        let m = Arc::new(Mem(Default::default()));
        assert_eq!(selftest_secure_storage(m).unwrap(), "سلام".as_bytes());
    }
}
