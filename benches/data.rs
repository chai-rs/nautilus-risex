//! WebSocket data path: recorded frames to `RisexWsMessage`, then to book levels and a trade tick.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use nautilus_core::UnixNanos;
use nautilus_risex::{
    http::{
        models::{RisexMarkets, RisexResponse},
        parse::parse_instrument,
    },
    websocket::{
        RisexWsMessage,
        parse::{parse_book_levels, parse_trade_tick},
    },
};

const FRAMES: &str = include_str!("../tests/fixtures/ws/public_aero_btc.jsonl");
const MARKETS: &str = include_str!("../tests/fixtures/http/markets.json");

// Lines of the fixture, all BTC (market 1): the 517-level snapshot, a one-level update, a trade.
const BOOK_SNAPSHOT: usize = 0;
const BOOK_UPDATE: usize = 4;
const TRADE: usize = 159;

fn frame(line: usize) -> &'static str {
    FRAMES.lines().nth(line).unwrap()
}

fn decode(text: &str) -> RisexWsMessage {
    serde_json::from_str(text).unwrap()
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode");
    for (name, line) in [
        ("book_snapshot_517", BOOK_SNAPSHOT),
        ("book_update", BOOK_UPDATE),
        ("trade", TRADE),
    ] {
        let text = frame(line);
        group.bench_function(name, |b| b.iter(|| decode(black_box(text))));
    }
    group.finish();

    let markets: RisexResponse<RisexMarkets> = serde_json::from_str(MARKETS).unwrap();
    let btc = markets.data.markets.iter().find(|m| m.market_id == 1);
    let btc = parse_instrument(btc.unwrap(), UnixNanos::default()).unwrap();
    let RisexWsMessage::Orderbook(book) = decode(frame(BOOK_SNAPSHOT)) else {
        panic!("line {BOOK_SNAPSHOT} is not an orderbook frame");
    };
    let RisexWsMessage::Trades(trade) = decode(frame(TRADE)) else {
        panic!("line {TRADE} is not a trades frame");
    };
    let ts = UnixNanos::default();

    let mut group = c.benchmark_group("parse");
    group.bench_function("book_levels_517", |b| {
        b.iter(|| {
            let bids = parse_book_levels(black_box(&book.data.bids)).unwrap();
            (bids, parse_book_levels(&book.data.asks).unwrap())
        });
    });
    group.bench_function("trade_tick", |b| {
        b.iter(|| parse_trade_tick(black_box(&trade.data), &btc, ts, ts).unwrap());
    });
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
