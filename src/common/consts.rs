//! Venue-wide constants for the RISEx adapter.

use std::sync::LazyLock;

use nautilus_model::identifiers::Venue;
use ustr::Ustr;

/// Venue name used in instrument and account identifiers.
pub const RISEX: &str = "RISEX";

/// The RISEx venue.
pub static RISEX_VENUE: LazyLock<Venue> = LazyLock::new(|| Venue::new(Ustr::from(RISEX)));

/// Chain id of RISE mainnet (EIP-712 domain).
pub const RISEX_MAINNET_CHAIN_ID: u64 = 4153;
/// Chain id of RISE testnet (EIP-712 domain).
pub const RISEX_TESTNET_CHAIN_ID: u64 = 11_155_931;

/// Default HTTP request timeout.
pub const HTTP_TIMEOUT_SECS: u64 = 10;

/// Largest `limit` the paginated history endpoints accept.
pub const MAX_PAGE_LIMIT: u32 = 1000;
/// Largest `limit` (levels per side) `/v1/orderbook` accepts.
pub const MAX_BOOK_LIMIT: u32 = 250;

/// Decimals of the integer strings on wei-denominated fields (`oracle` channel, `/v1/positions`).
pub const WEI_DECIMALS: u32 = 18;
