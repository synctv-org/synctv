//! Shared validation context for model-level settings.

/// Runtime inputs needed by settings validators.
#[derive(Clone, Copy)]
pub struct SettingsValidationContext<'a> {
    pub ssrf_guard: &'a synctv_common::ssrf::SsrfGuard,
}

impl<'a> SettingsValidationContext<'a> {
    #[must_use]
    pub const fn new(ssrf_guard: &'a synctv_common::ssrf::SsrfGuard) -> Self {
        Self { ssrf_guard }
    }
}

impl SettingsValidationContext<'_> {
    pub fn with_strict_policy<R>(f: impl FnOnce(&SettingsValidationContext<'_>) -> R) -> R {
        let ssrf_guard = synctv_common::ssrf::SsrfGuard::strict_policy();
        f(&SettingsValidationContext::new(&ssrf_guard))
    }
}
