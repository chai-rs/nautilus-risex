//! Arbitrary bytes through the `RisexWsMessage` decode and the data parse functions: no panic.
#![no_main]

use std::sync::LazyLock;

use libfuzzer_sys::fuzz_target;
use nautilus_core::UnixNanos;
use nautilus_model::instruments::InstrumentAny;
use nautilus_risex::{
    http::{
        models::{RisexMarkets, RisexResponse},
        parse::parse_instrument,
    },
    websocket::{
        RisexWsMessage,
        parse::{
            parse_book_levels, parse_funding_rate, parse_oracle_price, parse_timestamp_ns,
            parse_trade_tick,
        },
    },
};

static AERO: LazyLock<InstrumentAny> = LazyLock::new(|| {
    let markets: RisexResponse<RisexMarkets> =
        serde_json::from_str(include_str!("../../tests/fixtures/http/markets.json")).unwrap();
    let aero = markets.data.markets.iter().find(|m| m.market_id == 15);
    parse_instrument(aero.unwrap(), UnixNanos::default()).unwrap()
});

fuzz_target!(|data: &[u8]| {
    let Ok(message) = serde_json::from_slice::<RisexWsMessage>(data) else {
        return;
    };
    let ts = UnixNanos::default();
    match &message {
        RisexWsMessage::Orderbook(frame) => {
            let _ = parse_timestamp_ns(&frame.worker_timestamp);
            let _ = parse_book_levels(&frame.data.bids);
            let _ = parse_book_levels(&frame.data.asks);
        }
        RisexWsMessage::Trades(frame) => {
            let _ = parse_trade_tick(&frame.data, &AERO, ts, ts);
        }
        RisexWsMessage::Oracle(frame) => {
            for price in frame.data.prices.values() {
                let _ = parse_oracle_price(price, &AERO, ts, ts);
            }
        }
        RisexWsMessage::ExpectedFundingRate(frame) => {
            for rate in &frame.data {
                let _ = parse_funding_rate(rate, &AERO, ts, ts);
            }
        }
        _ => {}
    }
});
