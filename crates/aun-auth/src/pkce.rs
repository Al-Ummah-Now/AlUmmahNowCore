//! PKCE (RFC 7636) with the S256 method only (api-videos' OAuth clients require S256), plus the
//! random `state` value for CSRF protection.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

use aun_common::{Error, Result};

/// A PKCE verifier/challenge pair.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

impl Pkce {
    /// 32 random bytes → a 43-character base64url verifier (the RFC's recommended size).
    pub fn generate() -> Result<Self> {
        Ok(Self::from_verifier(random_token(32)?))
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = s256(&verifier);
        Self { verifier, challenge }
    }

    pub const METHOD: &'static str = "S256";
}

/// `BASE64URL(SHA256(ASCII(verifier)))`.
pub fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// `n` random bytes from the OS CSPRNG, base64url-encoded.
pub fn random_token(n: usize) -> Result<String> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).map_err(|e| Error::Internal(format!("OS random source: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_matches_independent_computation() {
        // Cross-checked with Python: base64.urlsafe_b64encode(hashlib.sha256(v).digest()).rstrip(b"=")
        let p = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r0wW1gFWFOEjXk".into());
        assert_eq!(p.challenge, "RCtbqyFn6Dv6GL6UC4xrWL6mHZBJowIOtGzu8O97GYg");
        assert_eq!(s256(""), "47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU");
    }

    #[test]
    fn generated_verifier_is_valid() {
        let p = Pkce::generate().unwrap();
        assert_eq!(p.verifier.len(), 43);
        assert!(
            p.verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_ne!(Pkce::generate().unwrap().verifier, p.verifier);
        assert!(!format!("{p:?}").contains(&p.verifier));
    }
}
