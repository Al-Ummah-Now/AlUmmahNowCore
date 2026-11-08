//! OAuth 2 + PKCE S256, device authorization (RFC 8628), multi-account, token storage adapter.
//!
//! Scaffolding only; see `docs/PLAN.md` §9 for the build order.

#[cfg(test)]
mod tests {
    #[test]
    fn links_common() {
        assert!(!aun_common::VERSION.is_empty());
    }
}
