//! The background runtime for work that outlives a single FFI call (upload workers, best-effort
//! token revocation after sign-out).

use std::sync::OnceLock;

use crate::AunError;

pub(crate) fn background() -> Result<&'static tokio::runtime::Runtime, AunError> {
    static RT: OnceLock<std::io::Result<tokio::runtime::Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("aun-background")
            .enable_all()
            .build()
    })
    .as_ref()
    .map_err(|e| AunError::Internal {
        detail: format!("background runtime: {e}"),
    })
}
