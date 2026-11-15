//! The Usage page: counts of what agents did, read from the event log.

use super::{Ctx, OpError};
use crate::domain::usage::{Usage, USAGE_WEEKS};
use crate::ports::store::Store;

/// The community's runs and durations, and what agents started in the last weeks.
pub async fn get(store: &dyn Store, ctx: &Ctx) -> Result<Usage, OpError> {
    let mut tx = store.begin(ctx.community).await?;
    Ok(tx.usage().usage(USAGE_WEEKS).await?)
}
