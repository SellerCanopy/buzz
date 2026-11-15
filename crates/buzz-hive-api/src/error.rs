//! One error type, one mapping to the Connect JSON body and HTTP status.

use buzz_hive_core::orchestrator::intake::IntakeError;
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::ports::code_host::HostError;
use buzz_hive_core::ports::store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum HiveError {
    #[error("{0}")]
    Unauthenticated(String),
    #[error("{0}")]
    PermissionDenied(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    InvalidArgument(String),
    #[error("{0}")]
    FailedPrecondition(String),
    #[error("{0}")]
    AlreadyExists(String),
    #[error("{0}")]
    Unavailable(String),
    /// The caller is over a rate limit. Trying again later works.
    #[error("{0}")]
    ResourceExhausted(String),
    #[error("{0}")]
    Internal(String),
}

impl HiveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthenticated(_) => "unauthenticated",
            Self::PermissionDenied(_) => "permission_denied",
            Self::NotFound(_) => "not_found",
            Self::InvalidArgument(_) => "invalid_argument",
            Self::FailedPrecondition(_) => "failed_precondition",
            Self::AlreadyExists(_) => "already_exists",
            Self::Unavailable(_) => "unavailable",
            Self::ResourceExhausted(_) => "resource_exhausted",
            Self::Internal(_) => "internal",
        }
    }

    pub fn status(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode as S;
        match self {
            Self::Unauthenticated(_) => S::UNAUTHORIZED,
            Self::PermissionDenied(_) => S::FORBIDDEN,
            Self::NotFound(_) => S::NOT_FOUND,
            Self::InvalidArgument(_) => S::BAD_REQUEST,
            Self::FailedPrecondition(_) | Self::AlreadyExists(_) => S::CONFLICT,
            Self::Unavailable(_) => S::SERVICE_UNAVAILABLE,
            Self::ResourceExhausted(_) => S::TOO_MANY_REQUESTS,
            Self::Internal(_) => S::INTERNAL_SERVER_ERROR,
        }
    }

    /// An `Internal` error whose cause is logged and not sent to the caller.
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        tracing::error!(%cause, "internal error");
        Self::Internal("internal error".into())
    }
}

impl From<StoreError> for HiveError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Unavailable(cause) => {
                tracing::error!(%cause, "store unavailable");
                Self::Unavailable("storage is unavailable".into())
            }
            StoreError::Conflict(message) => Self::FailedPrecondition(message),
            StoreError::NotFound(message) => Self::NotFound(message),
            StoreError::Internal(cause) => Self::internal(cause),
        }
    }
}

impl From<OpError> for HiveError {
    fn from(error: OpError) -> Self {
        match error {
            OpError::InvalidArgument(message) => Self::InvalidArgument(message),
            OpError::NotFound(message) => Self::NotFound(message),
            OpError::FailedPrecondition(message) => Self::FailedPrecondition(message),
            OpError::PermissionDenied(message) => Self::PermissionDenied(message),
            OpError::Store(error) => error.into(),
        }
    }
}

impl From<HostError> for HiveError {
    fn from(error: HostError) -> Self {
        match error {
            HostError::NotFound(message) => Self::NotFound(message),
            HostError::RateLimited { retry_after } => Self::ResourceExhausted(format!(
                "GitHub is rate limiting the team token; try again in {} seconds",
                retry_after.as_secs().max(1)
            )),
            HostError::Unauthorized(message) => {
                Self::FailedPrecondition(format!("GitHub refused the team token: {message}"))
            }
            HostError::Rejected(message) => {
                Self::FailedPrecondition(format!("GitHub refused the request: {message}"))
            }
            HostError::Other(message) => Self::Unavailable(format!("GitHub failed: {message}")),
        }
    }
}

impl From<IntakeError> for HiveError {
    fn from(error: IntakeError) -> Self {
        match error {
            IntakeError::Op(error) => error.into(),
            IntakeError::Host(error) => error.into(),
        }
    }
}

impl axum::response::IntoResponse for HiveError {
    fn into_response(self) -> axum::response::Response {
        // A refusal the caller caused is not an error here, but it should be findable: the
        // request span names the route and the request id.
        if !self.status().is_server_error() {
            tracing::info!(code = self.code(), message = %self, "request refused");
        }
        let body = serde_json::json!({ "code": self.code(), "message": self.to_string() });
        (self.status(), axum::Json(body)).into_response()
    }
}
