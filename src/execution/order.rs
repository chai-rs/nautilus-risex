//! Nautilus orders to venue requests.
//!
//! Exact integer math: a price or size that is not a whole number of ticks or
//! steps is an error, never rounded.

use alloy_primitives::Address;
use anyhow::Context;
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::QuoteTick,
    enums::{OrderSide, OrderType, TimeInForce, TriggerType},
    instruments::{Instrument, InstrumentAny},
    orders::{Order, OrderAny},
};
use rust_decimal::{Decimal, prelude::ToPrimitive};

use crate::{
    common::{
        enums::{RisexOrderSide, RisexOrderType, RisexStpMode, RisexTimeInForce},
        parse::client_order_id_to_u64,
    },
    signing::{
        action::{MAX_PRICE_TICKS, PlaceOrder},
        tpsl::PlaceTpsl,
    },
};

/// One `ttl_units` step of a GTT order: 5 minutes.
const TTL_UNIT_NANOS: u64 = 300 * 1_000_000_000;

/// `value / increment` as a whole number.
///
/// # Errors
///
/// Returns an error if `value` is not a multiple of `increment` or does not fit.
pub fn units(value: Decimal, increment: Decimal, what: &str) -> anyhow::Result<u32> {
    anyhow::ensure!(
        increment > Decimal::ZERO,
        "{what} increment is not positive"
    );
    anyhow::ensure!(
        (value % increment).is_zero(),
        "{what} {value} is not a multiple of {increment}"
    );
    (value / increment)
        .to_u32()
        .with_context(|| format!("{what} {value} is out of range"))
}

/// Price cap of a market order: the opposite top of book moved by
/// `slippage_bps`, rounded away from the book to a whole tick.
///
/// # Errors
///
/// Returns an error if there is no quote or the side is not buy or sell.
pub fn market_price(
    side: OrderSide,
    quote: Option<&QuoteTick>,
    tick: Decimal,
    slippage_bps: u32,
) -> anyhow::Result<Decimal> {
    let quote = quote.context("no quote to cap the market order price")?;
    let slip = Decimal::from(slippage_bps) / Decimal::from(10_000);
    match side {
        OrderSide::Buy => {
            Ok((quote.ask_price.as_decimal() * (Decimal::ONE + slip) / tick).ceil() * tick)
        }
        OrderSide::Sell => {
            Ok((quote.bid_price.as_decimal() * (Decimal::ONE - slip) / tick).floor() * tick)
        }
    }
}

/// The venue request of a market or limit order. `quantity` and `price` are
/// passed apart from the order so a modify can send the new values.
///
/// # Errors
///
/// Returns an error if the venue cannot take the order as it is.
pub fn place_order(
    order: &OrderAny,
    market_id: u16,
    instrument: &InstrumentAny,
    quantity: Decimal,
    price: Decimal,
    builder_id: u16,
    now: UnixNanos,
) -> anyhow::Result<PlaceOrder> {
    let size_steps = units(
        quantity,
        instrument.size_increment().as_decimal(),
        "quantity",
    )?;
    anyhow::ensure!(size_steps > 0, "quantity is zero");
    if let Some(min) = instrument.min_quantity() {
        anyhow::ensure!(
            quantity >= min.as_decimal(),
            "quantity {quantity} is below the minimum {min}"
        );
    }
    let price_ticks = units(price, instrument.price_increment().as_decimal(), "price")?;
    anyhow::ensure!(
        (1..=MAX_PRICE_TICKS).contains(&price_ticks),
        "price {price} is outside the venue range"
    );

    let (time_in_force, post_only) = match order.order_type() {
        // A market order goes as IOC with `price` as the cap; the venue's own
        // market type has no cap.
        OrderType::Market if order.time_in_force() == TimeInForce::Fok => {
            (RisexTimeInForce::Fok, false)
        }
        OrderType::Market => (RisexTimeInForce::Ioc, false),
        OrderType::Limit => (
            RisexTimeInForce::try_from(order.time_in_force())?,
            order.is_post_only(),
        ),
        other => anyhow::bail!("unsupported order type: {other}"),
    };
    // The router rejects a TTL on any other time in force.
    let ttl_units = if time_in_force == RisexTimeInForce::Gtt {
        ttl_units(order.expire_time(), now)?
    } else {
        0
    };

    Ok(PlaceOrder {
        market_id,
        size_steps,
        price_ticks,
        side: side_code(order.order_side()),
        post_only,
        reduce_only: order.is_reduce_only(),
        stp_mode: RisexStpMode::ExpireMaker.code(),
        order_type: RisexOrderType::Limit.code(),
        time_in_force: time_in_force.code(),
        builder_id,
        client_order_id: client_order_id_to_u64(order.client_order_id().as_str()),
        ttl_units,
    })
}

/// A reduce-only IOC order that closes a position of `size` (sign ignored) held
/// on `side` (`BUY` = long), capped like a market order.
///
/// # Errors
///
/// Returns an error if the position is flat or does not fit the step grid.
pub fn close_order(
    market_id: u16,
    instrument: &InstrumentAny,
    side: RisexOrderSide,
    size: Decimal,
    quote: &QuoteTick,
    slippage_bps: u32,
    client_order_id: u64,
) -> anyhow::Result<PlaceOrder> {
    let closing = match side {
        RisexOrderSide::Buy => OrderSide::Sell,
        RisexOrderSide::Sell => OrderSide::Buy,
    };
    let tick = instrument.price_increment().as_decimal();
    let price = market_price(closing, Some(quote), tick, slippage_bps)?;
    let size_steps = units(
        size.abs(),
        instrument.size_increment().as_decimal(),
        "position size",
    )?;
    anyhow::ensure!(size_steps > 0, "position is flat");
    let price_ticks = units(price, tick, "price")?;
    anyhow::ensure!(
        (1..=MAX_PRICE_TICKS).contains(&price_ticks),
        "price {price} is outside the venue range"
    );

    Ok(PlaceOrder {
        market_id,
        size_steps,
        price_ticks,
        side: side_code(closing),
        post_only: false,
        reduce_only: true,
        stp_mode: RisexStpMode::ExpireMaker.code(),
        order_type: RisexOrderType::Limit.code(),
        time_in_force: RisexTimeInForce::Ioc.code(),
        builder_id: 0,
        client_order_id,
        ttl_units: 0,
    })
}

/// The venue request of a trigger order; `account` and `deadline` are filled
/// by the session.
///
/// # Errors
///
/// Returns an error if the venue cannot take the order as it is.
pub fn place_tpsl(
    order: &OrderAny,
    market_id: u16,
    instrument: &InstrumentAny,
) -> anyhow::Result<PlaceTpsl> {
    // unverified: the uint8 codes, inferred from the OpenAPI enum order.
    let (stop_type, limit) = match order.order_type() {
        OrderType::MarketIfTouched => (0, false),
        OrderType::LimitIfTouched => (0, true),
        OrderType::StopMarket => (1, false),
        OrderType::StopLimit => (1, true),
        other => anyhow::bail!("unsupported trigger order type: {other}"),
    };
    let tick = instrument.price_increment().as_decimal();
    let size = order.quantity().as_decimal();
    anyhow::ensure!(
        units(size, instrument.size_increment().as_decimal(), "quantity")? > 0,
        "quantity is zero"
    );
    let stop_price = order
        .trigger_price()
        .context("trigger order without trigger price")?
        .as_decimal();
    anyhow::ensure!(
        units(stop_price, tick, "trigger price")? > 0,
        "trigger price is zero"
    );

    let (order_type, limit_price, tif) = if limit {
        let price = order
            .price()
            .context("limit trigger order without price")?
            .as_decimal();
        anyhow::ensure!(units(price, tick, "price")? > 0, "price is zero");
        // The request has no expiry field, so GTD cannot be sent.
        anyhow::ensure!(
            order.time_in_force() == TimeInForce::Gtc,
            "limit trigger orders take GTC only, not {}",
            order.time_in_force()
        );
        (
            RisexOrderType::Limit,
            price.normalize().to_string(),
            RisexTimeInForce::Gtc,
        )
    } else {
        // The venue takes market trigger orders as IOC only.
        (
            RisexOrderType::Market,
            "0".to_string(),
            RisexTimeInForce::Ioc,
        )
    };
    let stop_price_option = match order.trigger_type() {
        None | Some(TriggerType::Default | TriggerType::MarkPrice) => 1,
        Some(TriggerType::LastPrice) => 0,
        Some(other) => anyhow::bail!("unsupported trigger type: {other}"),
    };

    Ok(PlaceTpsl {
        account: Address::ZERO,
        market_id: u64::from(market_id),
        side: side_code(order.order_side()),
        size: size.normalize().to_string(),
        stop_type,
        order_type: order_type.code(),
        stop_price: stop_price.normalize().to_string(),
        limit_price,
        stop_price_option,
        tif: tif.code(),
        deadline: 0,
        size_percent_bps: 0,
    })
}

fn side_code(side: OrderSide) -> u8 {
    match side {
        OrderSide::Buy => 0,
        OrderSide::Sell => 1,
    }
}

/// Floored, so the order never outlives the requested expiry.
fn ttl_units(expire: Option<UnixNanos>, now: UnixNanos) -> anyhow::Result<u16> {
    let expire = expire.context("GTD order without expire time")?;
    let units = expire.as_u64().saturating_sub(now.as_u64()) / TTL_UNIT_NANOS;
    anyhow::ensure!(units >= 1, "expire time is less than 5 minutes away");
    u16::try_from(units).map_err(|_| anyhow::anyhow!("expire time is more than 227 days away"))
}

#[cfg(test)]
mod tests {
    use nautilus_model::{
        identifiers::InstrumentId,
        orders::OrderTestBuilder,
        types::{Price, Quantity},
    };
    use rust_decimal_macros::dec;

    use super::*;
    use crate::http::parse::tests::instrument;

    const AERO: u16 = 15;

    fn limit(quantity: &str, price: &str) -> OrderAny {
        OrderTestBuilder::new(OrderType::Limit)
            .instrument_id(instrument(AERO).id())
            .side(OrderSide::Buy)
            .quantity(Quantity::from(quantity))
            .price(Price::from(price))
            .build()
    }

    fn quote() -> QuoteTick {
        QuoteTick::new(
            InstrumentId::from("AERO-USDC-PERP.RISEX"),
            Price::from("0.30000"),
            Price::from("0.30011"),
            Quantity::from("100"),
            Quantity::from("100"),
            UnixNanos::default(),
            UnixNanos::default(),
        )
    }

    fn convert(order: &OrderAny) -> anyhow::Result<PlaceOrder> {
        place_order(
            order,
            AERO,
            &instrument(AERO),
            order.quantity().as_decimal(),
            order.price().unwrap().as_decimal(),
            0,
            UnixNanos::default(),
        )
    }

    #[test]
    fn limit_order_in_ticks_and_steps() {
        let order = limit("25", "0.31234");
        let place = convert(&order).unwrap();
        assert_eq!(place.market_id, AERO);
        assert_eq!(place.size_steps, 25);
        assert_eq!(place.price_ticks, 31_234);
        assert_eq!(
            (place.side, place.order_type, place.time_in_force),
            (0, 1, 0)
        );
        assert_eq!(place.ttl_units, 0);
        assert!(!place.post_only && !place.reduce_only);
    }

    #[test]
    fn off_grid_values_are_errors_not_rounded() {
        let aero = instrument(AERO);
        let order = limit("25", "0.31234");
        let place = |quantity, price| {
            place_order(
                &order,
                AERO,
                &aero,
                quantity,
                price,
                0,
                UnixNanos::default(),
            )
        };
        assert!(place(dec!(25), dec!(0.312345)).is_err());
        assert!(place(dec!(25.5), dec!(0.31234)).is_err());
        // Below `min_order_size` 20.
        assert!(place(dec!(19), dec!(0.31234)).is_err());
        assert!(place(dec!(0), dec!(0.31234)).is_err());
        assert!(place(dec!(25), dec!(0)).is_err());
        // Above the 24-bit tick range.
        assert!(place(dec!(25), dec!(167.77216)).is_err());
        assert!(place(dec!(25), dec!(167.77215)).is_ok());
    }

    #[test]
    fn market_order_is_ioc_at_the_slippage_cap() {
        let aero = instrument(AERO);
        let quote = quote();
        let tick = aero.price_increment().as_decimal();
        // 0.30011 * 1.005 = 0.30161055, up to the tick.
        let buy = market_price(OrderSide::Buy, Some(&quote), tick, 50).unwrap();
        assert_eq!(buy, dec!(0.30162));
        // 0.3 * 0.995 = 0.2985.
        let sell = market_price(OrderSide::Sell, Some(&quote), tick, 50).unwrap();
        assert_eq!(sell, dec!(0.29850));
        assert!(market_price(OrderSide::Buy, None, tick, 50).is_err());

        let order = OrderTestBuilder::new(OrderType::Market)
            .instrument_id(aero.id())
            .side(OrderSide::Sell)
            .quantity(Quantity::from("20"))
            .build();
        let place =
            place_order(&order, AERO, &aero, dec!(20), sell, 7, UnixNanos::default()).unwrap();
        assert_eq!(
            (place.side, place.order_type, place.time_in_force),
            (1, 1, 3)
        );
        assert_eq!((place.price_ticks, place.builder_id), (29_850, 7));
    }

    #[test]
    fn close_order_is_reduce_only_ioc_on_the_opposite_side() {
        let aero = instrument(AERO);
        let close = |side, size| close_order(AERO, &aero, side, size, &quote(), 50, 7);

        let long = close(RisexOrderSide::Buy, dec!(99299)).unwrap();
        assert_eq!(
            (long.side, long.size_steps, long.price_ticks),
            (1, 99_299, 29_850)
        );
        assert_eq!((long.order_type, long.time_in_force), (1, 3));
        assert!(long.reduce_only && !long.post_only);
        assert_eq!(long.client_order_id, 7);

        // The venue sends a short size as a negative number next to the side.
        let short = close(RisexOrderSide::Sell, dec!(-25)).unwrap();
        assert_eq!(
            (short.side, short.size_steps, short.price_ticks),
            (0, 25, 30_162)
        );

        assert!(close(RisexOrderSide::Buy, dec!(0)).is_err());
        assert!(close(RisexOrderSide::Buy, dec!(25.5)).is_err());
    }

    #[test]
    fn gtd_becomes_gtt_with_floored_ttl() {
        let minutes = |m: u64| UnixNanos::from(m * 60 * 1_000_000_000);
        let gtd = |expire| {
            OrderTestBuilder::new(OrderType::Limit)
                .instrument_id(instrument(AERO).id())
                .side(OrderSide::Buy)
                .quantity(Quantity::from("25"))
                .price(Price::from("0.31234"))
                .time_in_force(TimeInForce::Gtd)
                .expire_time(expire)
                .build()
        };
        let place = convert(&gtd(minutes(14))).unwrap();
        assert_eq!((place.time_in_force, place.ttl_units), (1, 2));
        assert!(convert(&gtd(minutes(4))).is_err());
        assert!(convert(&gtd(minutes(5 * 65_536))).is_err());
    }

    #[test]
    fn stop_market_is_a_stop_loss_ioc() {
        let aero = instrument(AERO);
        let order = OrderTestBuilder::new(OrderType::StopMarket)
            .instrument_id(aero.id())
            .side(OrderSide::Sell)
            .quantity(Quantity::from("20"))
            .trigger_price(Price::from("0.25000"))
            .build();
        let tpsl = place_tpsl(&order, AERO, &aero).unwrap();
        assert_eq!((tpsl.stop_type, tpsl.order_type, tpsl.tif), (1, 0, 3));
        assert_eq!((tpsl.side, tpsl.stop_price_option), (1, 1));
        assert_eq!(tpsl.size, "20");
        assert_eq!(tpsl.stop_price, "0.25");
        assert_eq!(tpsl.limit_price, "0");
        assert!(place_tpsl(&limit("25", "0.31234"), AERO, &aero).is_err());
    }
}
