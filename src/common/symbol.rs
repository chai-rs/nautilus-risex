//! Instrument id construction and the `InstrumentId <-> market_id` registry.

use dashmap::DashMap;
use nautilus_model::identifiers::{InstrumentId, Symbol};

use crate::common::consts::RISEX_VENUE;

/// Suffix of every RISEx symbol: all markets are perpetuals.
pub const PERP_SUFFIX: &str = "-PERP";

/// Splits a venue market name such as `AERO/USDC` into base and quote codes.
///
/// # Errors
///
/// Returns an error if the name is not exactly `BASE/QUOTE` with alphanumeric codes. The venue
/// lists retired markets under names like `DOGE/USDC [deprecated-1779958099]`, which would
/// otherwise collide with the live market.
pub fn split_market_name(name: &str) -> anyhow::Result<(&str, &str)> {
    let is_code = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric());
    match name.split_once('/') {
        Some((base, quote)) if is_code(base) && is_code(quote) => Ok((base, quote)),
        _ => anyhow::bail!("unsupported RISEx market name: {name:?}"),
    }
}

/// Builds the Nautilus instrument id (`{BASE}-{QUOTE}-PERP.RISEX`) for a venue market name.
///
/// # Errors
///
/// Returns an error if the name is rejected by [`split_market_name`].
pub fn instrument_id_from_market_name(name: &str) -> anyhow::Result<InstrumentId> {
    let (base, quote) = split_market_name(name)?;
    let symbol = format!("{base}-{quote}{PERP_SUFFIX}");
    Ok(InstrumentId::new(Symbol::new(symbol), *RISEX_VENUE))
}

/// Two-way map between Nautilus instrument ids and venue market ids.
#[derive(Debug, Default)]
pub struct MarketRegistry {
    by_market_id: DashMap<u16, InstrumentId>,
    by_instrument_id: DashMap<InstrumentId, u16>,
}

impl MarketRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a market, replacing any previous mapping of either key.
    pub fn insert(&self, market_id: u16, instrument_id: InstrumentId) {
        // Drop the stale inverse entries so both maps stay one-to-one.
        if let Some(old) = self.by_market_id.insert(market_id, instrument_id)
            && old != instrument_id
        {
            self.by_instrument_id.remove(&old);
        }

        if let Some(old) = self.by_instrument_id.insert(instrument_id, market_id)
            && old != market_id
        {
            self.by_market_id.remove(&old);
        }
    }

    /// Returns the instrument id for a venue market id.
    #[must_use]
    pub fn instrument_id(&self, market_id: u16) -> Option<InstrumentId> {
        self.by_market_id.get(&market_id).map(|e| *e.value())
    }

    /// Returns the venue market id for an instrument id.
    #[must_use]
    pub fn market_id(&self, instrument_id: &InstrumentId) -> Option<u16> {
        self.by_instrument_id.get(instrument_id).map(|e| *e.value())
    }

    /// Returns every registered market id, sorted.
    #[must_use]
    pub fn all_market_ids(&self) -> Vec<u16> {
        let mut ids: Vec<u16> = self.by_market_id.iter().map(|e| *e.key()).collect();
        ids.sort_unstable();
        ids
    }

    /// Removes every mapping.
    pub fn clear(&self) {
        self.by_market_id.clear();
        self.by_instrument_id.clear();
    }

    /// Returns the number of registered markets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_market_id.len()
    }

    /// Returns `true` if no market is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_market_id.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    fn test_instrument_id_from_market_name() {
        let id = instrument_id_from_market_name("AERO/USDC").unwrap();
        assert_eq!(id.to_string(), "AERO-USDC-PERP.RISEX");
        assert_eq!(
            instrument_id_from_market_name("1000PEPE/USDC")
                .unwrap()
                .to_string(),
            "1000PEPE-USDC-PERP.RISEX"
        );
    }

    #[rstest]
    #[case("DOGE/USDC [deprecated-1779958099]")]
    #[case("AEROUSDC")]
    #[case("/USDC")]
    #[case("A/B/C")]
    fn test_split_market_name_rejects(#[case] name: &str) {
        assert!(split_market_name(name).is_err());
    }

    #[rstest]
    fn test_registry_round_trip_and_eviction() {
        let registry = MarketRegistry::new();
        let aero = instrument_id_from_market_name("AERO/USDC").unwrap();
        let btc = instrument_id_from_market_name("BTC/USDC").unwrap();
        registry.insert(15, aero);
        registry.insert(1, btc);

        assert_eq!(registry.instrument_id(15), Some(aero));
        assert_eq!(registry.market_id(&btc), Some(1));
        assert_eq!(registry.all_market_ids(), vec![1, 15]);

        // Re-listing AERO under a new market id must not leave the old id resolvable.
        registry.insert(99, aero);
        assert_eq!(registry.instrument_id(15), None);
        assert_eq!(registry.market_id(&aero), Some(99));
        assert_eq!(registry.len(), 2);

        registry.clear();
        assert!(registry.is_empty());
    }
}
