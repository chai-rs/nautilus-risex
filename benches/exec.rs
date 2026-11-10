//! Order path before signing: a Nautilus order to the venue's `PlaceOrder`.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::QuoteTick,
    enums::{OrderSide, OrderType},
    instruments::Instrument,
    orders::OrderTestBuilder,
    types::{Price, Quantity},
};
use nautilus_risex::{
    execution::order::{market_price, place_order, units},
    http::{
        models::{RisexMarkets, RisexResponse},
        parse::parse_instrument,
    },
};
use rust_decimal_macros::dec;

const MARKETS: &str = include_str!("../tests/fixtures/http/markets.json");
const AERO: u16 = 15;

fn bench(c: &mut Criterion) {
    let ts = UnixNanos::default();
    let markets: RisexResponse<RisexMarkets> = serde_json::from_str(MARKETS).unwrap();
    let aero = markets.data.markets.iter().find(|m| m.market_id == AERO);
    let aero = parse_instrument(aero.unwrap(), ts).unwrap();
    let tick = aero.price_increment().as_decimal();
    let order = OrderTestBuilder::new(OrderType::Limit)
        .instrument_id(aero.id())
        .side(OrderSide::Buy)
        .quantity(Quantity::from("25"))
        .price(Price::from("0.31234"))
        .build();
    let quote = QuoteTick::new(
        aero.id(),
        Price::from("0.30000"),
        Price::from("0.30011"),
        Quantity::from("100"),
        Quantity::from("100"),
        ts,
        ts,
    );

    let mut group = c.benchmark_group("exec");
    group.bench_function("units", |b| {
        b.iter(|| units(black_box(dec!(0.31234)), tick, "price").unwrap());
    });
    group.bench_function("market_price", |b| {
        b.iter(|| market_price(OrderSide::Buy, Some(black_box(&quote)), tick, 50).unwrap());
    });
    group.bench_function("place_order", |b| {
        b.iter(|| {
            place_order(
                black_box(&order),
                AERO,
                &aero,
                dec!(25),
                dec!(0.31234),
                0,
                ts,
            )
            .unwrap()
        });
    });
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
