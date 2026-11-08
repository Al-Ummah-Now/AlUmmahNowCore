//! Consent-gated analytics, QoE events and crash breadcrumbs.
//!
//! Scaffolding only; see `docs/PLAN.md` §9 for the build order.

#[cfg(test)]
mod tests {
    #[test]
    fn links_common() {
        assert!(!aun_common::VERSION.is_empty());
    }
}
