//! Raw venue enums as they appear on the RiseX WebSocket feed.
//!
//! Order and fill enums live in `common::enums`; account frames read them through
//! [`lenient`] so a value the venue adds later does not make the whole frame undecodable.

use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

/// The channels the venue accepts (any other name is answered with `invalid channel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RisexWsChannel {
    Orderbook,
    Trades,
    Oracle,
    ExpectedFundingRate,
    Funding,
    System,
    Orders,
    Fills,
    Positions,
    Account,
}

/// The `type` of a channel data frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RisexWsDataKind {
    Snapshot,
    Update,
}

/// The `status` of a reply to a client request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RisexWsStatus {
    Success,
    Error,
}

/// The request `method` a reply answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RisexWsAckMethod {
    Subscribe,
    Unsubscribe,
    Auth,
}

/// Decodes a venue enum of `common::enums`, turning a value this crate does not know
/// (or the `""` of a flat position) into `None` so the frame itself still decodes.
pub(super) fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
{
    let text = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    let value = text.parse().ok();
    if value.is_none() && !text.is_empty() {
        log::warn!(
            "Unknown RiseX {} value {text:?}",
            std::any::type_name::<T>()
        );
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RisexMaintenancePhase {
    None,
    Scheduled,
    Active,
    #[serde(other)]
    Unknown,
}
