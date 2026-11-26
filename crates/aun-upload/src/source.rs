//! Reopenable, random-access upload streams.

use aun_common::{Error, Result};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// A platform stream with a known length and a stable content identity.
/// Callbacks may run on background threads.
pub trait UploadSource: Send + Sync {
    fn size(&self) -> Result<u64>;

    /// An opaque identity that stays stable across reopens and changes when the bytes change.
    fn fingerprint(&self) -> Result<String>;

    /// Read at most `len` bytes from `offset`. Short reads are allowed; empty means EOF.
    fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>>;
}

/// Reopens a source by its persisted ID. The app is responsible for retaining platform access.
pub trait UploadSourceOpener: Send + Sync {
    fn reopen(&self, source_id: String) -> Result<Arc<dyn UploadSource>>;
}

pub(crate) const PIECE: u64 = 64 * 1024;

pub(crate) fn read_piece(source: &dyn UploadSource, offset: u64, len: u64) -> Result<Vec<u8>> {
    let data = source.read_at(offset, len)?;
    if data.is_empty() || data.len() as u64 > len {
        return Err(Error::invalid_input(
            "upload source returned an invalid read length".into(),
        ));
    }
    Ok(data)
}

pub(crate) fn sha256_source(
    source: &dyn UploadSource,
    size: u64,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<String> {
    let mut hash = Sha256::new();
    let mut offset = 0;
    while offset < size {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let data = read_piece(source, offset, (size - offset).min(PIECE))?;
        offset += data.len() as u64;
        hash.update(data);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OversizedRead;

    impl UploadSource for OversizedRead {
        fn size(&self) -> Result<u64> {
            Ok(2)
        }

        fn fingerprint(&self) -> Result<String> {
            Ok("revision-1".into())
        }

        fn read_at(&self, _offset: u64, len: u64) -> Result<Vec<u8>> {
            Ok(vec![0; len as usize + 1])
        }
    }

    #[test]
    fn rejects_reads_larger_than_requested() {
        assert!(matches!(
            read_piece(&OversizedRead, 0, 1),
            Err(Error::InvalidInput { .. })
        ));
    }
}
