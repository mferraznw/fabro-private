use crate::error::{Error, ProviderErrorKind};

#[derive(Debug, Clone, Default)]
pub struct FallbackStrategy;

impl FallbackStrategy {
    #[must_use]
    pub const fn should_fallback(error: &Error) -> bool {
        error.failover_eligible()
    }

    #[must_use]
    pub fn reason(error: &Error) -> &'static str {
        match error.provider_kind() {
            Some(ProviderErrorKind::RateLimit) => "rate_limit",
            Some(ProviderErrorKind::Server) => "server_error",
            Some(ProviderErrorKind::ContextLength) => "context_length",
            Some(ProviderErrorKind::QuotaExceeded) => "quota_exceeded",
            Some(ProviderErrorKind::Authentication) => "authentication",
            Some(ProviderErrorKind::AccessDenied) => "access_denied",
            Some(ProviderErrorKind::NotFound) => "not_found",
            Some(ProviderErrorKind::InvalidRequest) => "invalid_request",
            Some(ProviderErrorKind::ContentFilter) => "content_filter",
            None => {
                if matches!(error, Error::RequestTimeout { .. } | Error::Network { .. }) {
                    "transport"
                } else {
                    "deterministic"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProviderErrorDetail;

    fn provider_error(kind: ProviderErrorKind, status_code: u16) -> Error {
        Error::Provider {
            kind,
            detail: Box::new(ProviderErrorDetail {
                status_code: Some(status_code),
                ..ProviderErrorDetail::new("test", "litellm")
            }),
        }
    }

    #[test]
    fn fallback_decision_tree() {
        assert!(FallbackStrategy::should_fallback(&provider_error(
            ProviderErrorKind::RateLimit,
            429,
        )));
        assert!(FallbackStrategy::should_fallback(&provider_error(
            ProviderErrorKind::Server,
            500,
        )));
        assert!(!FallbackStrategy::should_fallback(&provider_error(
            ProviderErrorKind::ContextLength,
            413,
        )));
        assert!(!FallbackStrategy::should_fallback(&provider_error(
            ProviderErrorKind::InvalidRequest,
            400,
        )));
        assert!(!FallbackStrategy::should_fallback(&provider_error(
            ProviderErrorKind::Authentication,
            401,
        )));
        assert!(!FallbackStrategy::should_fallback(&provider_error(
            ProviderErrorKind::AccessDenied,
            403,
        )));
        assert!(!FallbackStrategy::should_fallback(&Error::Interrupt {
            message: "cancelled".to_string(),
        }));
    }
}
