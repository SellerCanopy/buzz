//! How an engine error becomes an API error: its code, its HTTP status and what the caller is
//! told. Ported from the Go engine's `TestConnectErrorMapping` (`api/errors_test.go`).

use std::time::Duration;

use axum::http::StatusCode;
use buzz_hive_api::HiveError;
use buzz_hive_core::orchestrator::intake::IntakeError;
use buzz_hive_core::orchestrator::OpError;
use buzz_hive_core::ports::code_host::HostError;
use buzz_hive_core::ports::store::StoreError;

/// The code and status an error maps to.
fn mapped(error: impl Into<HiveError>) -> (&'static str, StatusCode) {
    let error = error.into();
    (error.code(), error.status())
}

#[test]
fn an_operation_error_keeps_its_kind_and_message() {
    let cases = [
        (
            OpError::InvalidArgument("title is required".into()),
            "invalid_argument",
            StatusCode::BAD_REQUEST,
        ),
        (
            OpError::NotFound("card 3 does not exist".into()),
            "not_found",
            StatusCode::NOT_FOUND,
        ),
        (
            OpError::FailedPrecondition("card 3 is done".into()),
            "failed_precondition",
            StatusCode::CONFLICT,
        ),
        (
            OpError::PermissionDenied("an agent may not do this".into()),
            "permission_denied",
            StatusCode::FORBIDDEN,
        ),
        (
            OpError::AlreadyExists("the name is taken".into()),
            "already_exists",
            StatusCode::CONFLICT,
        ),
    ];
    for (error, code, status) in cases {
        let message = error.to_string();
        let got: HiveError = error.into();
        assert_eq!((got.code(), got.status()), (code, status), "{message}");
        assert_eq!(got.to_string(), message, "the message is passed on");
    }
}

#[test]
fn a_workflow_that_failed_its_checks_is_an_invalid_argument() {
    assert_eq!(
        mapped(OpError::InvalidWorkflow(Vec::new())),
        ("invalid_argument", StatusCode::BAD_REQUEST)
    );
}

#[test]
fn a_store_error_inside_an_operation_maps_as_the_store_error_does() {
    assert_eq!(
        mapped(OpError::Store(StoreError::NotFound("card 9".into()))),
        ("not_found", StatusCode::NOT_FOUND)
    );
    assert_eq!(
        mapped(OpError::Store(StoreError::Conflict("changed".into()))),
        ("failed_precondition", StatusCode::CONFLICT)
    );
}

#[test]
fn a_store_fault_is_not_sent_to_the_caller() {
    assert_eq!(
        mapped(StoreError::Unavailable(
            "connection refused at 10.0.0.1".into()
        )),
        ("unavailable", StatusCode::SERVICE_UNAVAILABLE)
    );
    let internal: HiveError = StoreError::Internal("bad column secret_col".into()).into();
    assert_eq!(
        (internal.code(), internal.status()),
        ("internal", StatusCode::INTERNAL_SERVER_ERROR)
    );
    assert_eq!(internal.to_string(), "internal error");
    let unavailable: HiveError = StoreError::Unavailable("connection refused".into()).into();
    assert_eq!(unavailable.to_string(), "storage is unavailable");
}

#[test]
fn a_code_host_error_maps_by_what_the_caller_can_do() {
    assert_eq!(
        mapped(HostError::NotFound("issue 4".into())),
        ("not_found", StatusCode::NOT_FOUND)
    );
    assert_eq!(
        mapped(HostError::RateLimited {
            retry_after: Duration::from_secs(30)
        }),
        ("resource_exhausted", StatusCode::TOO_MANY_REQUESTS)
    );
    assert_eq!(
        mapped(HostError::Unauthorized("bad credentials".into())),
        ("failed_precondition", StatusCode::CONFLICT)
    );
    assert_eq!(
        mapped(HostError::Rejected("branch is protected".into())),
        ("failed_precondition", StatusCode::CONFLICT)
    );
    assert_eq!(
        mapped(HostError::Other("timeout".into())),
        ("unavailable", StatusCode::SERVICE_UNAVAILABLE)
    );
}

#[test]
fn a_rate_limit_tells_the_caller_how_long_to_wait_and_never_less_than_one_second() {
    let long: HiveError = HostError::RateLimited {
        retry_after: Duration::from_secs(42),
    }
    .into();
    assert!(long.to_string().contains("42 seconds"), "{long}");
    let none: HiveError = HostError::RateLimited {
        retry_after: Duration::ZERO,
    }
    .into();
    assert!(none.to_string().contains("1 seconds"), "{none}");
}

#[test]
fn an_intake_error_maps_as_the_error_inside_it() {
    assert_eq!(
        mapped(IntakeError::Op(OpError::NotFound("rule".into()))),
        ("not_found", StatusCode::NOT_FOUND)
    );
    assert_eq!(
        mapped(IntakeError::Host(HostError::RateLimited {
            retry_after: Duration::from_secs(5)
        })),
        ("resource_exhausted", StatusCode::TOO_MANY_REQUESTS)
    );
}
