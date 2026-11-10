//! Market data frames of the RISEx WebSocket feed as Nautilus types.

use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{FundingRateUpdate, IndexPriceUpdate, MarkPriceUpdate, TradeTick},
    enums::AggressorSide,
    instruments::{Instrument, InstrumentAny},
};

use super::messages::{
    RisexWsBookLevel, RisexWsExpectedFundingRate, RisexWsOraclePrice, RisexWsTrade,
};
use crate::{
    book::BookLevel,
    common::parse::{
        parse_decimal, parse_wei, price_from_decimal, quantity_from_decimal, trade_id_from_venue,
    },
};

/// Parses a nanosecond timestamp string (`worker_timestamp`, `next_funding_time`).
///
/// # Errors
///
/// Returns an error if `value` is not an unsigned integer.
pub fn parse_timestamp_ns(value: &str) -> anyhow::Result<UnixNanos> {
    value
        .parse::<u64>()
        .map(UnixNanos::from)
        .map_err(|e| anyhow::anyhow!("invalid timestamp {value:?}: {e}"))
}

/// Parses one side of an `orderbook` frame.
///
/// # Errors
///
/// Returns an error if a price or quantity is not a decimal number.
pub fn parse_book_levels(levels: &[RisexWsBookLevel]) -> anyhow::Result<Vec<BookLevel>> {
    levels
        .iter()
        .map(|level| {
            Ok(BookLevel {
                price: parse_decimal(&level.price)?,
                quantity: parse_decimal(&level.quantity)?,
                order_count: level.order_count,
            })
        })
        .collect()
}

/// Parses a `trades` frame.
///
/// # Errors
///
/// Returns an error if the price, the size or the maker side is not valid.
pub fn parse_trade_tick(
    trade: &RisexWsTrade,
    instrument: &InstrumentAny,
    ts_event: UnixNanos,
    ts_init: UnixNanos,
) -> anyhow::Result<TradeTick> {
    // The aggressor is the taker, the opposite of the maker side (0 = buy, 1 = sell)
    let aggressor_side = match trade.maker_side {
        0 => AggressorSide::Sell,
        1 => AggressorSide::Buy,
        other => anyhow::bail!("invalid maker_side {other}"),
    };
    TradeTick::new_checked(
        instrument.id(),
        price_from_decimal(parse_decimal(&trade.price)?, instrument.price_precision())?,
        quantity_from_decimal(parse_decimal(&trade.size)?, instrument.size_precision())?,
        aggressor_side,
        trade_id_from_venue(&trade.id),
        ts_event,
        ts_init,
    )
}

/// Parses the prices of one market of an `oracle` frame. Either price can be absent.
///
/// # Errors
///
/// Returns an error if a price is not a wei integer.
pub fn parse_oracle_price(
    price: &RisexWsOraclePrice,
    instrument: &InstrumentAny,
    ts_event: UnixNanos,
    ts_init: UnixNanos,
) -> anyhow::Result<(Option<MarkPriceUpdate>, Option<IndexPriceUpdate>)> {
    let parse = |value: &Option<String>| {
        value
            .as_deref()
            .map(|wei| price_from_decimal(parse_wei(wei)?, instrument.price_precision()))
            .transpose()
    };
    let id = instrument.id();
    Ok((
        parse(&price.mark_price)?.map(|value| MarkPriceUpdate::new(id, value, ts_event, ts_init)),
        parse(&price.index_price)?.map(|value| IndexPriceUpdate::new(id, value, ts_event, ts_init)),
    ))
}

/// Parses one entry of an `expected_funding_rate` frame. The rate is the estimate for the
/// funding at `next_funding_time`; the frame does not carry the funding interval.
///
/// # Errors
///
/// Returns an error if the rate or the next funding time is not a number.
pub fn parse_funding_rate(
    rate: &RisexWsExpectedFundingRate,
    instrument: &InstrumentAny,
    ts_event: UnixNanos,
    ts_init: UnixNanos,
) -> anyhow::Result<FundingRateUpdate> {
    Ok(FundingRateUpdate::new(
        instrument.id(),
        parse_decimal(&rate.estimated_rate)?,
        None,
        Some(parse_timestamp_ns(&rate.next_funding_time)?),
        ts_event,
        ts_init,
    ))
}
