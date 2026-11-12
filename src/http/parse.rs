//! Conversion of RISEx REST models to Nautilus types.

use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Bar, BarType, FundingRateUpdate, TradeTick},
    enums::{MarketStatusAction, OrderStatus, OrderType, PositionSide},
    identifiers::{AccountId, Symbol, VenueOrderId},
    instruments::{CryptoPerpetual, Instrument, InstrumentAny},
    reports::{FillReport, OrderStatusReport, PositionStatusReport},
    types::{Currency, Money, Price, Quantity},
};
use rust_decimal::Decimal;

use super::models::{
    RisexAccountPosition, RisexAccountTrade, RisexBar, RisexFundingRate, RisexMarket,
    RisexOpenOrder, RisexOrder, RisexOrderBook, RisexPosition, RisexTpslOrder, RisexTrade,
};
use crate::common::{
    enums::{RisexOrderStatus, RisexOrderType, RisexStopType, RisexTpslOrderStatus},
    parse::{price_from_decimal, quantity_from_decimal, trade_id_from_venue},
    symbol::{instrument_id_from_market_name, split_market_name},
};

const NANOS_PER_MINUTE: u64 = 60_000_000_000;

/// Order book levels of one REST snapshot, best price first on both sides.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RisexBookSnapshot {
    pub bids: Vec<(Price, Quantity)>,
    pub asks: Vec<(Price, Quantity)>,
}

/// Parses a market into a [`CryptoPerpetual`].
///
/// # Errors
///
/// Returns an error if the market name is not `BASE/QUOTE` or the market configuration is
/// not usable (a step, the leverage or the margin factor is not positive).
pub fn parse_instrument(market: &RisexMarket, ts_init: UnixNanos) -> anyhow::Result<InstrumentAny> {
    let config = &market.config;
    let (base, quote) = split_market_name(&config.name)?;
    anyhow::ensure!(
        config.step_price > Decimal::ZERO
            && config.step_size > Decimal::ZERO
            && config.max_leverage > Decimal::ZERO
            && config.maintenance_margin_factor > Decimal::ZERO,
        "invalid configuration for market {}",
        config.name
    );

    let price_precision = u8::try_from(config.step_price.normalize().scale())?;
    let size_precision = u8::try_from(config.step_size.normalize().scale())?;
    let quote_currency = Currency::get_or_create_crypto(quote);

    let instrument = CryptoPerpetual::builder()
        .instrument_id(instrument_id_from_market_name(&config.name)?)
        .raw_symbol(Symbol::new(&config.name))
        .base_currency(Currency::get_or_create_crypto(base))
        .quote_currency(quote_currency)
        .settlement_currency(quote_currency)
        .is_inverse(false)
        .price_precision(price_precision)
        .size_precision(size_precision)
        .price_increment(price_from_decimal(config.step_price, price_precision)?)
        .size_increment(quantity_from_decimal(config.step_size, size_precision)?)
        .min_quantity(quantity_from_decimal(
            config.min_order_size,
            size_precision,
        )?)
        .margin_init(Decimal::ONE / config.max_leverage)
        // The factor is 1.5 x `max_leverage` on every market, so it is read as the
        // leverage at which a position is liquidated.
        .margin_maint(Decimal::ONE / config.maintenance_margin_factor)
        .ts_event(ts_init)
        .ts_init(ts_init)
        .build()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    Ok(InstrumentAny::CryptoPerpetual(instrument))
}

/// The trading state of a market: an action and its reason. No reason is normal trading.
pub type MarketStatus = (MarketStatusAction, Option<&'static str>);

/// Reads the trading state of a market from its flags. `maintenance` is the venue-wide
/// state of `GET /v1/system/config`.
// unverified: the meaning of each flag follows the docs. The recorded markets show only
// normal, post-only and inactive ones.
#[must_use]
pub fn parse_market_status(market: &RisexMarket, maintenance: bool) -> MarketStatus {
    if !market.active {
        (MarketStatusAction::NotAvailableForTrading, Some("inactive"))
    } else if !market.config.unlocked {
        (MarketStatusAction::Halt, Some("locked"))
    } else if maintenance {
        // Maintenance is a venue-wide post-only mode: resting orders and cancels only.
        (MarketStatusAction::Pause, Some("maintenance"))
    } else if market.post_only {
        (MarketStatusAction::Pause, Some("post_only"))
    } else if market.reduce_only {
        (MarketStatusAction::Trading, Some("reduce_only"))
    } else {
        (MarketStatusAction::Trading, None)
    }
}

/// Parses a public trade.
///
/// # Errors
///
/// Returns an error if the price or size does not fit the instrument, or the size is zero.
pub fn parse_trade_tick(
    trade: &RisexTrade,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> anyhow::Result<TradeTick> {
    TradeTick::new_checked(
        instrument.id(),
        price_from_decimal(trade.price, instrument.price_precision())?,
        quantity_from_decimal(trade.size, instrument.size_precision())?,
        trade.maker_side.as_aggressor_of_maker(),
        trade_id_from_venue(&trade.id),
        UnixNanos::from(trade.time),
        ts_init,
    )
}

/// Parses a bar. Bar prices can carry more decimals than the tick and are rounded to it.
///
/// # Errors
///
/// Returns an error if a value does not fit the instrument or the bar is inconsistent.
pub fn parse_bar(
    bar: &RisexBar,
    bar_type: BarType,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> anyhow::Result<Bar> {
    let price = |value| price_from_decimal(value, instrument.price_precision());
    // The venue opens a bar at the previous close, which can lie outside its own high-low
    // range: widen the range, a bar with the open outside it is rejected by Nautilus.
    let high = bar.high.max(bar.open).max(bar.close);
    let low = bar.low.min(bar.open).min(bar.close);
    Bar::new_checked(
        bar_type,
        price(bar.open)?,
        price(high)?,
        price(low)?,
        price(bar.close)?,
        quantity_from_decimal(bar.volume, instrument.size_precision())?,
        UnixNanos::from(bar.time),
        ts_init,
    )
}

/// Parses a settled funding rate. The event time is the end of the funding period.
#[must_use]
pub fn parse_funding_rate(
    record: &RisexFundingRate,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> FundingRateUpdate {
    let minutes = record.end_time.saturating_sub(record.start_time) / NANOS_PER_MINUTE;
    FundingRateUpdate::new(
        instrument.id(),
        record.funding_rate,
        u16::try_from(minutes).ok().filter(|minutes| *minutes > 0),
        None,
        UnixNanos::from(record.end_time),
        ts_init,
    )
}

/// Parses the levels of an order book snapshot.
///
/// # Errors
///
/// Returns an error if a price or size does not fit the instrument.
pub fn parse_book_snapshot(
    book: &RisexOrderBook,
    instrument: &InstrumentAny,
) -> anyhow::Result<RisexBookSnapshot> {
    let levels = |side: &[super::models::RisexBookLevel]| {
        side.iter()
            .map(|level| {
                Ok((
                    price_from_decimal(level.price, instrument.price_precision())?,
                    quantity_from_decimal(level.quantity, instrument.size_precision())?,
                ))
            })
            .collect::<anyhow::Result<Vec<_>>>()
    };
    Ok(RisexBookSnapshot {
        bids: levels(&book.bids)?,
        asks: levels(&book.asks)?,
    })
}

/// Parses an order of `GET /v1/orders` or `GET /v1/orders/by-id/{order_id}`.
///
/// The report has no client order id: the venue returns only the `u64` sent at placement
/// (`order.client_order_id`), which the caller maps back to its own id.
///
/// # Errors
///
/// Returns an error if the order has no status or a value does not fit the instrument.
pub fn parse_order_status_report(
    order: &RisexOrder,
    account_id: AccountId,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> anyhow::Result<OrderStatusReport> {
    let price_precision = instrument.price_precision();
    let size_precision = instrument.size_precision();
    let order_status = match order.status {
        RisexOrderStatus::Open if order.filled_size > Decimal::ZERO => OrderStatus::PartiallyFilled,
        RisexOrderStatus::Open => OrderStatus::Accepted,
        RisexOrderStatus::Filled => OrderStatus::Filled,
        RisexOrderStatus::Cancelled => OrderStatus::Canceled,
        RisexOrderStatus::None => anyhow::bail!("order {} has no status", order.id),
    };
    // The venue reports no update time.
    let ts_accepted = UnixNanos::from(order.created_at);

    let mut report = OrderStatusReport::new(
        account_id,
        instrument.id(),
        None,
        VenueOrderId::new(&order.id),
        Some(order.side.into()),
        order.order_type.into(),
        order.time_in_force.into(),
        order_status,
        quantity_from_decimal(order.size, size_precision)?,
        quantity_from_decimal(order.filled_size, size_precision)?,
        ts_accepted,
        ts_accepted,
        ts_init,
        None,
    )
    .with_post_only(order.post_only)
    .with_reduce_only(order.reduce_only);

    if order.price > Decimal::ZERO {
        report = report.with_price(price_from_decimal(order.price, price_precision)?);
    }
    if order.avg_price > Decimal::ZERO {
        report = report.with_avg_px(order.avg_price);
    }
    if order.expiry != 0 {
        report = report.with_expire_time(UnixNanos::from(order.expiry));
    }
    if !order.cancel_reason.is_empty() {
        report = report.with_cancel_reason(order.cancel_reason.clone());
    }
    if let Some(stop_price) = order.stop_price {
        report = report.with_trigger_price(price_from_decimal(stop_price, price_precision)?);
        if let Some(trigger_type) = order.stop_price_option.and_then(|o| o.as_trigger_type()) {
            report = report.with_trigger_type(trigger_type);
        }
    }

    Ok(report)
}

/// Parses an account trade. The signed venue fee is kept as is: a negative commission is
/// a rebate.
///
/// # Errors
///
/// Returns an error if a value does not fit the instrument.
pub fn parse_fill_report(
    trade: &RisexAccountTrade,
    account_id: AccountId,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> anyhow::Result<FillReport> {
    Ok(FillReport::new(
        account_id,
        instrument.id(),
        VenueOrderId::new(&trade.order_id),
        trade_id_from_venue(&trade.id),
        trade.side.into(),
        quantity_from_decimal(trade.size, instrument.size_precision())?,
        price_from_decimal(trade.price, instrument.price_precision())?,
        Money::from_decimal(trade.fee, instrument.quote_currency())?,
        trade.liquidity_indicator.into(),
        None,
        None,
        UnixNanos::from(trade.time),
        ts_init,
        None,
    ))
}

/// Parses a position of `GET /v1/positions`.
///
/// # Errors
///
/// Returns an error if the size does not fit the instrument.
pub fn parse_position_status_report(
    position: &RisexPosition,
    account_id: AccountId,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> anyhow::Result<PositionStatusReport> {
    // The size is signed and the side is sent next to it: the side decides.
    let quantity = quantity_from_decimal(position.size.abs(), instrument.size_precision())?;
    let position_side = if quantity.is_zero() {
        PositionSide::Flat
    } else {
        position.side.as_position_side()
    };

    Ok(PositionStatusReport::new(
        account_id,
        instrument.id(),
        position_side,
        quantity,
        ts_init,
        ts_init,
        None,
        None,
        (position.avg_entry_price > Decimal::ZERO).then_some(position.avg_entry_price),
    ))
}

/// Parses a row of `GET /v1/orders/open`. The row has no filled size and no timestamps, so
/// the report says `Accepted` with nothing filled: use it to find resting orders, not to
/// reconcile fills.
///
/// # Errors
///
/// Returns an error if a value does not fit the instrument.
pub fn parse_open_order_status_report(
    order: &RisexOpenOrder,
    account_id: AccountId,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> anyhow::Result<OrderStatusReport> {
    let size_precision = instrument.size_precision();
    let price = Decimal::from(order.price_ticks) * instrument.price_increment().as_decimal();
    let size = Decimal::from(order.size_steps) * instrument.size_increment().as_decimal();

    let mut report = OrderStatusReport::new(
        account_id,
        instrument.id(),
        None,
        VenueOrderId::new(&order.order_id),
        Some(order.side.into()),
        order.order_type.into(),
        order.time_in_force.into(),
        OrderStatus::Accepted,
        quantity_from_decimal(size, size_precision)?,
        quantity_from_decimal(Decimal::ZERO, size_precision)?,
        ts_init,
        ts_init,
        ts_init,
        None,
    )
    .with_post_only(order.post_only)
    .with_reduce_only(order.reduce_only);

    if order.price_ticks > 0 {
        report = report.with_price(price_from_decimal(price, instrument.price_precision())?);
    }

    Ok(report)
}

/// Parses a TP/SL order. Take profit maps to the if-touched order types and stop loss to the
/// stop order types.
///
/// # Errors
///
/// Returns an error if the order has no stop type or a value does not fit the instrument.
pub fn parse_tpsl_order_status_report(
    order: &RisexTpslOrder,
    account_id: AccountId,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> anyhow::Result<OrderStatusReport> {
    let price_precision = instrument.price_precision();
    let size_precision = instrument.size_precision();
    let order_type = match (order.stop_type, order.order_type) {
        (RisexStopType::StopLoss, RisexOrderType::Market) => OrderType::StopMarket,
        (RisexStopType::StopLoss, RisexOrderType::Limit) => OrderType::StopLimit,
        (RisexStopType::TakeProfit, RisexOrderType::Market) => OrderType::MarketIfTouched,
        (RisexStopType::TakeProfit, RisexOrderType::Limit) => OrderType::LimitIfTouched,
        (RisexStopType::None, _) => {
            anyhow::bail!("TP/SL order {} has no stop type", order.order_id)
        }
    };
    let order_status = match order.status {
        RisexTpslOrderStatus::Accepted => OrderStatus::Accepted,
        RisexTpslOrderStatus::Triggered => OrderStatus::Triggered,
        RisexTpslOrderStatus::Success => OrderStatus::Filled,
        RisexTpslOrderStatus::Cancelled => OrderStatus::Canceled,
    };
    // TP/SL times are unix seconds.
    let ts_accepted = UnixNanos::from(order.created_at * 1_000_000_000);

    let mut report = OrderStatusReport::new(
        account_id,
        instrument.id(),
        None,
        VenueOrderId::new(&order.order_id),
        Some(order.side.into()),
        order_type,
        order.tif.into(),
        order_status,
        quantity_from_decimal(order.size, size_precision)?,
        quantity_from_decimal(order.filled_size, size_precision)?,
        ts_accepted,
        ts_accepted,
        ts_init,
        None,
    )
    .with_reduce_only(order.reduce_only);

    if let Some(stop_price) = order.stop_price {
        report = report.with_trigger_price(price_from_decimal(stop_price, price_precision)?);
        if let Some(trigger_type) = order.stop_price_option.and_then(|o| o.as_trigger_type()) {
            report = report.with_trigger_type(trigger_type);
        }
    }
    if let Some(limit_price) = order.limit_price.filter(|price| *price > Decimal::ZERO) {
        report = report.with_price(price_from_decimal(limit_price, price_precision)?);
    }
    if !order.cancel_reason.is_empty() {
        report = report.with_cancel_reason(order.cancel_reason.clone());
    }

    Ok(report)
}

/// Parses the position of `GET /v1/account/position` (one market).
///
/// # Errors
///
/// Returns an error if the size does not fit the instrument.
pub fn parse_account_position_status_report(
    position: &RisexAccountPosition,
    account_id: AccountId,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> anyhow::Result<PositionStatusReport> {
    let quantity = quantity_from_decimal(position.size.abs(), instrument.size_precision())?;
    let position_side = if quantity.is_zero() {
        PositionSide::Flat
    } else {
        position.side.as_position_side()
    };

    Ok(PositionStatusReport::new(
        account_id,
        instrument.id(),
        position_side,
        quantity,
        ts_init,
        ts_init,
        None,
        None,
        position
            .avg_entry_price
            .filter(|price| *price > Decimal::ZERO),
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use nautilus_model::{
        enums::{AggressorSide, LiquiditySide, OrderSide, TimeInForce, TriggerType},
        identifiers::InstrumentId,
    };
    use rstest::rstest;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::http::models::{
        RisexAccountPositionResponse, RisexAccountTrades, RisexBars, RisexFundingRates,
        RisexMarkets, RisexOpenOrders, RisexOrders, RisexPositions, RisexTpslOrders, RisexTrades,
        tests::fixture,
    };

    fn instruments() -> Vec<(u16, InstrumentAny)> {
        fixture::<RisexMarkets>("markets")
            .markets
            .iter()
            .filter(|market| market.active)
            .map(|market| {
                (
                    market.market_id,
                    parse_instrument(market, UnixNanos::default()).unwrap(),
                )
            })
            .collect()
    }

    pub(crate) fn instrument(market_id: u16) -> InstrumentAny {
        let (_, instrument) = instruments()
            .into_iter()
            .find(|(id, _)| *id == market_id)
            .unwrap();
        instrument
    }

    fn account_id() -> AccountId {
        AccountId::new("RISEX-001")
    }

    #[rstest]
    fn test_parse_instruments() {
        let instruments = instruments();
        // 54 markets, one of them deprecated and inactive.
        assert_eq!(instruments.len(), 53);

        let aero = instrument(15);
        assert_eq!(aero.id(), InstrumentId::from("AERO-USDC-PERP.RISEX"));
        assert_eq!(aero.raw_symbol().as_str(), "AERO/USDC");
        assert_eq!(aero.price_increment(), Price::from("0.00001"));
        assert_eq!(aero.size_increment(), Quantity::from("1"));
        assert_eq!(aero.min_quantity(), Some(Quantity::from("20")));
        assert_eq!(aero.quote_currency().code.as_str(), "USDC");
        assert_eq!(aero.margin_init(), Decimal::ONE / dec!(3));
        assert_eq!(aero.margin_maint(), Decimal::ONE / dec!(4.5));

        let btc = instrument(1);
        assert_eq!(btc.id(), InstrumentId::from("BTC-USDC-PERP.RISEX"));
        assert_eq!(btc.price_increment(), Price::from("0.1"));
        assert_eq!(btc.size_increment(), Quantity::from("0.000001"));
        assert_eq!(btc.margin_init(), dec!(0.02));
    }

    #[rstest]
    fn test_parse_market_status() {
        let markets = fixture::<RisexMarkets>("markets").markets;
        let count = |status| {
            let found = markets
                .iter()
                .filter(|m| parse_market_status(m, false) == status);
            found.count()
        };
        assert_eq!(count((MarketStatusAction::Trading, None)), 52);
        assert_eq!(count((MarketStatusAction::Pause, Some("post_only"))), 1);
        let inactive = (MarketStatusAction::NotAvailableForTrading, Some("inactive"));
        assert_eq!(count(inactive), 1);

        // Hand-made: no recorded market is locked, reduce-only or in maintenance.
        let mut btc = markets.iter().find(|m| m.market_id == 1).unwrap().clone();
        let paused = (MarketStatusAction::Pause, Some("maintenance"));
        assert_eq!(parse_market_status(&btc, true), paused);
        btc.reduce_only = true;
        let reduce_only = (MarketStatusAction::Trading, Some("reduce_only"));
        assert_eq!(parse_market_status(&btc, false), reduce_only);
        btc.config.unlocked = false;
        let locked = (MarketStatusAction::Halt, Some("locked"));
        assert_eq!(parse_market_status(&btc, true), locked);
    }

    #[rstest]
    fn test_parse_instrument_rejects_deprecated_market() {
        let markets = fixture::<RisexMarkets>("markets").markets;
        let deprecated = markets.iter().find(|market| !market.active).unwrap();

        assert!(parse_instrument(deprecated, UnixNanos::default()).is_err());
    }

    #[rstest]
    fn test_parse_trade_tick() {
        let aero = instrument(15);
        let trades = fixture::<RisexTrades>("trades_aero").trades;

        let tick = parse_trade_tick(&trades[0], &aero, UnixNanos::from(7)).unwrap();

        assert_eq!(tick.instrument_id, aero.id());
        assert_eq!(tick.price, Price::from("0.55296"));
        assert_eq!(tick.size, Quantity::from("33"));
        // The resting order was a buy, so the taker sold.
        assert_eq!(tick.aggressor_side, AggressorSide::Sell);
        assert_eq!(tick.ts_event, UnixNanos::from(1_794_280_444_000_000_000));
        assert_eq!(tick.ts_init, UnixNanos::from(7));
        assert_eq!(tick.trade_id, trade_id_from_venue(&trades[0].id));
        assert!(
            trades
                .iter()
                .all(|t| parse_trade_tick(t, &aero, UnixNanos::default()).is_ok())
        );
    }

    #[rstest]
    fn test_parse_bar() {
        let aero = instrument(15);
        let bar_type = BarType::from("AERO-USDC-PERP.RISEX-1-MINUTE-LAST-EXTERNAL");
        let bars = fixture::<RisexBars>("bars_aero_1m").data;

        let bar = parse_bar(&bars[1], bar_type, &aero, UnixNanos::default()).unwrap();

        assert_eq!(bar.bar_type, bar_type);
        // "0.553025" is rounded half to even at the 0.00001 tick.
        assert_eq!(bar.open, Price::from("0.55302"));
        assert_eq!(bar.high, Price::from("0.55302"));
        assert_eq!(bar.low, Price::from("0.55250"));
        assert_eq!(bar.close, Price::from("0.55265"));
        assert_eq!(bar.volume, Quantity::from("0"));
        assert_eq!(bar.ts_event, UnixNanos::from(1_794_277_020_000_000_000));
        assert!(
            bars.iter()
                .all(|b| parse_bar(b, bar_type, &aero, UnixNanos::default()).is_ok())
        );

        // Open 0.55246 below the venue low 0.55247: the range is widened to the open.
        let bar = parse_bar(&bars[4], bar_type, &aero, UnixNanos::default()).unwrap();
        assert_eq!(bar.ts_event, UnixNanos::from(1_794_277_200_000_000_000));
        assert_eq!(bar.low, Price::from("0.55246"));
        assert_eq!(bar.high, Price::from("0.55280"));
    }

    #[rstest]
    fn test_parse_funding_rate() {
        let aero = instrument(15);
        let records = fixture::<RisexFundingRates>("funding_aero").records;

        let update = parse_funding_rate(&records[0], &aero, UnixNanos::default());

        assert_eq!(update.instrument_id, aero.id());
        assert_eq!(update.rate, dec!(0.000040527567452405));
        assert_eq!(update.interval, Some(60));
        assert_eq!(update.next_funding_ns, None);
        assert_eq!(update.ts_event, UnixNanos::from(1_794_279_600_000_000_000));
    }

    #[rstest]
    fn test_parse_book_snapshot() {
        let aero = instrument(15);

        let snapshot = parse_book_snapshot(&fixture("orderbook_aero"), &aero).unwrap();

        assert_eq!(
            snapshot.bids[0],
            (Price::from("0.55296"), Quantity::from("12155"))
        );
        assert_eq!(
            snapshot.asks[0],
            (Price::from("0.55313"), Quantity::from("12708"))
        );
        assert!(snapshot.bids.is_sorted_by(|a, b| a.0 > b.0));
        assert!(snapshot.asks.is_sorted_by(|a, b| a.0 < b.0));
    }

    #[rstest]
    fn test_parse_order_status_report() {
        let orders = fixture::<RisexOrders>("orders_history").orders;
        let order = &orders[0];

        let report =
            parse_order_status_report(order, account_id(), &instrument(19), UnixNanos::from(7))
                .unwrap();

        assert_eq!(report.venue_order_id.as_str(), order.id);
        assert_eq!(report.client_order_id, None);
        assert_eq!(report.order_side, Some(OrderSide::Sell));
        assert_eq!(report.order_type, OrderType::Limit);
        assert_eq!(report.time_in_force, TimeInForce::Gtc);
        assert_eq!(report.order_status, OrderStatus::Accepted);
        assert_eq!(report.quantity, Quantity::from("757.724"));
        assert_eq!(report.filled_qty, Quantity::from("0.000"));
        assert_eq!(report.price, Some(Price::from("85.491")));
        assert_eq!(report.avg_px, None);
        assert_eq!(report.trigger_price, None);
        assert_eq!(report.expire_time, None);
        assert!(report.post_only);
        assert_eq!(
            report.ts_accepted,
            UnixNanos::from(1_794_280_475_000_000_000)
        );

        let mut order = order.clone();
        order.filled_size = dec!(1);
        order.stop_price = Some(dec!(80));
        let report =
            parse_order_status_report(&order, account_id(), &instrument(19), UnixNanos::from(7))
                .unwrap();
        assert_eq!(report.order_status, OrderStatus::PartiallyFilled);
        assert_eq!(report.trigger_price, Some(Price::from("80.000")));

        let cancelled = orders
            .iter()
            .find(|o| o.status == RisexOrderStatus::Cancelled)
            .unwrap();
        let report = parse_order_status_report(
            cancelled,
            account_id(),
            &instrument(cancelled.market_id),
            UnixNanos::from(7),
        )
        .unwrap();
        assert_eq!(report.order_status, OrderStatus::Canceled);
    }

    #[rstest]
    fn test_parse_fill_report() {
        let aero = instrument(15);
        let trades = fixture::<RisexAccountTrades>("account_trades").trades;
        let trade = &trades[0];

        let report = parse_fill_report(trade, account_id(), &aero, UnixNanos::from(7)).unwrap();

        assert_eq!(report.venue_order_id.as_str(), trade.order_id);
        assert_eq!(report.trade_id, trade_id_from_venue(&trade.id));
        assert_eq!(report.order_side, OrderSide::Buy);
        assert_eq!(report.last_qty, Quantity::from("33"));
        assert_eq!(report.last_px, Price::from("0.55296"));
        // A maker rebate stays a negative commission.
        assert_eq!(report.commission.as_decimal(), dec!(-0.00091238));
        assert_eq!(report.commission.currency.code.as_str(), "USDC");
        assert_eq!(report.liquidity_side, LiquiditySide::Maker);
        assert_eq!(report.ts_event, UnixNanos::from(1_794_280_475_000_000_000));
    }

    #[rstest]
    fn test_parse_position_status_report() {
        let positions = fixture::<RisexPositions>("positions").positions;

        let long = &positions[0];
        let report =
            parse_position_status_report(long, account_id(), &instrument(1), UnixNanos::from(7))
                .unwrap();
        assert_eq!(report.position_side, PositionSide::Long);
        assert_eq!(report.quantity, Quantity::from("4.208681"));
        assert_eq!(report.avg_px_open, Some(dec!(74943.503816206781632597)));

        let short = positions
            .iter()
            .find(|p| p.side == crate::common::enums::RisexOrderSide::Sell)
            .unwrap();
        let report = parse_position_status_report(
            short,
            account_id(),
            &instrument(short.market_id),
            UnixNanos::from(7),
        )
        .unwrap();
        assert_eq!(report.position_side, PositionSide::Short);
        assert!(report.quantity.is_positive());
        assert!(report.signed_decimal_qty < Decimal::ZERO);
    }

    #[rstest]
    fn test_parse_open_order_status_report() {
        // market 5: 71231 steps, 74109 ticks.
        let orders = fixture::<RisexOpenOrders>("orders_open").orders;
        let order = orders.iter().find(|o| o.market_id == 5).unwrap();
        let instrument = instrument(5);

        let report =
            parse_open_order_status_report(order, account_id(), &instrument, UnixNanos::from(7))
                .unwrap();

        assert_eq!(report.venue_order_id.as_str(), order.order_id);
        assert_eq!(report.order_status, OrderStatus::Accepted);
        assert_eq!(
            report.quantity.as_decimal(),
            Decimal::from(order.size_steps) * instrument.size_increment().as_decimal()
        );
        assert_eq!(
            report.price.unwrap().as_decimal(),
            Decimal::from(order.price_ticks) * instrument.price_increment().as_decimal()
        );
        assert!(report.filled_qty.is_zero());
        assert_eq!(report.post_only, order.post_only);
    }

    #[rstest]
    fn test_parse_tpsl_order_status_report() {
        let orders = fixture::<RisexTpslOrders>("tpsl_orders").orders;
        let order = &orders[0];

        let report = parse_tpsl_order_status_report(
            order,
            account_id(),
            &instrument(33),
            UnixNanos::from(7),
        )
        .unwrap();

        assert_eq!(report.venue_order_id.as_str(), order.order_id);
        assert_eq!(report.order_type, OrderType::StopLimit);
        assert_eq!(report.order_status, OrderStatus::Canceled);
        assert_eq!(report.order_side, Some(OrderSide::Sell));
        assert_eq!(report.trigger_price.unwrap().as_decimal(), dec!(0.16));
        assert_eq!(report.price.unwrap().as_decimal(), dec!(0.1544));
        assert_eq!(report.trigger_type, Some(TriggerType::MarkPrice));
        assert_eq!(
            report.ts_accepted,
            UnixNanos::from(1_790_049_416_000_000_000)
        );
        assert!(report.reduce_only);

        let mut order = order.clone();
        order.stop_type = RisexStopType::TakeProfit;
        order.order_type = RisexOrderType::Market;
        order.status = RisexTpslOrderStatus::Success;
        let report = parse_tpsl_order_status_report(
            &order,
            account_id(),
            &instrument(33),
            UnixNanos::from(7),
        )
        .unwrap();
        assert_eq!(report.order_type, OrderType::MarketIfTouched);
        assert_eq!(report.order_status, OrderStatus::Filled);
    }

    #[rstest]
    fn test_parse_account_position_status_report() {
        let mut position = fixture::<RisexAccountPositionResponse>("position_aero").position;

        let report = parse_account_position_status_report(
            &position,
            account_id(),
            &instrument(15),
            UnixNanos::from(7),
        )
        .unwrap();
        assert_eq!(report.position_side, PositionSide::Long);
        assert_eq!(report.quantity, Quantity::from("99299"));
        assert_eq!(report.avg_px_open, None);

        position.size = Decimal::ZERO;
        let report = parse_account_position_status_report(
            &position,
            account_id(),
            &instrument(15),
            UnixNanos::from(7),
        )
        .unwrap();
        assert_eq!(report.position_side, PositionSide::Flat);
    }
}
