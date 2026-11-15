//! Turn a database error into the right kind of [`StoreError`].
//!
//! Callers treat `Unavailable` as a passing fault and retry. A violated constraint, a bad
//! column or a failed decode is not passing, so it must not look like one.

use buzz_hive_core::ports::store::StoreError;

/// Classify `error`.
///
/// - A unique or foreign key violation is a `Conflict`: the request cannot succeed as sent.
/// - A lost connection, a full or closed pool, a timeout, a deadlock or a failover is
///   `Unavailable`: the same call may work in a moment.
/// - Anything else is `Internal`: a bug or a schema mismatch, kept for the log.
pub(crate) fn store_error(error: sqlx::Error) -> StoreError {
    match &error {
        sqlx::Error::Database(db) => match db.code().as_deref() {
            Some("23505") => StoreError::Conflict("that already exists".into()),
            Some("23503") => {
                StoreError::Conflict("it refers to something that no longer exists".into())
            }
            Some(code) if is_transient(code) => StoreError::Unavailable(error.to_string()),
            _ => StoreError::Internal(error.to_string()),
        },
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => StoreError::Unavailable(error.to_string()),
        _ => StoreError::Internal(error.to_string()),
    }
}

/// SQLSTATEs that pass: serialization failure, deadlock, lock or statement timeout, and the
/// classes for connection failures, resource limits and operator intervention.
fn is_transient(code: &str) -> bool {
    matches!(code, "40001" | "40P01" | "55P03" | "57014")
        || code.starts_with("08")
        || code.starts_with("53")
        || code.starts_with("57P")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_states_are_recognised() {
        for code in [
            "40001", "40P01", "55P03", "57014", "08006", "53300", "57P01",
        ] {
            assert!(is_transient(code), "{code}");
        }
        for code in ["23505", "23503", "42703", "22P02", "23514"] {
            assert!(!is_transient(code), "{code}");
        }
    }

    #[test]
    fn pool_and_io_faults_are_unavailable() {
        assert!(matches!(
            store_error(sqlx::Error::PoolTimedOut),
            StoreError::Unavailable(_)
        ));
        assert!(matches!(
            store_error(sqlx::Error::PoolClosed),
            StoreError::Unavailable(_)
        ));
        let io = std::io::Error::from(std::io::ErrorKind::ConnectionReset);
        assert!(matches!(
            store_error(sqlx::Error::Io(io)),
            StoreError::Unavailable(_)
        ));
    }

    #[test]
    fn a_missing_row_or_bad_column_is_internal() {
        assert!(matches!(
            store_error(sqlx::Error::RowNotFound),
            StoreError::Internal(_)
        ));
        assert!(matches!(
            store_error(sqlx::Error::ColumnNotFound("x".into())),
            StoreError::Internal(_)
        ));
    }
}
