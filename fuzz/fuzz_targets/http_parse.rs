//! Arbitrary bytes through the REST model decode and the `http::parse` functions: no panic.
#![no_main]

use std::sync::LazyLock;

use libfuzzer_sys::fuzz_target;
use nautilus_core::UnixNanos;
use nautilus_model::{data::BarType, identifiers::AccountId, instruments::InstrumentAny};
use nautilus_risex::http::{models::*, parse::*};

static AERO: LazyLock<InstrumentAny> = LazyLock::new(|| {
    let markets: RisexResponse<RisexMarkets> =
        serde_json::from_str(include_str!("../../tests/fixtures/http/markets.json")).unwrap();
    let aero = markets.data.markets.iter().find(|m| m.market_id == 15);
    parse_instrument(aero.unwrap(), UnixNanos::default()).unwrap()
});

/// Decodes the input as the REST envelope of `$model` and runs `$body` on the payload.
macro_rules! with {
    ($data:expr, $model:ty, |$payload:ident| $body:expr) => {
        if let Ok(RisexResponse { data: $payload, .. }) =
            serde_json::from_slice::<RisexResponse<$model>>($data)
        {
            $body
        }
    };
}

fuzz_target!(|data: &[u8]| {
    let aero = &*AERO;
    let ts = UnixNanos::default();
    let account = AccountId::new("RISEX-001");

    with!(data, RisexMarkets, |r| for market in &r.markets {
        let _ = parse_instrument(market, ts);
    });
    with!(data, RisexOrderBook, |r| {
        let _ = parse_book_snapshot(&r, aero);
    });
    with!(data, RisexTrades, |r| for trade in &r.trades {
        let _ = parse_trade_tick(trade, aero, ts);
    });
    with!(data, RisexBars, |r| {
        let bar_type = BarType::from("AERO-USDC-PERP.RISEX-1-MINUTE-LAST-EXTERNAL");
        for bar in &r.data {
            let _ = parse_bar(bar, bar_type, aero, ts);
        }
    });
    with!(data, RisexFundingRates, |r| for record in &r.records {
        let _ = parse_funding_rate(record, aero, ts);
    });
    with!(data, RisexOrders, |r| for order in &r.orders {
        let _ = parse_order_status_report(order, account, aero, ts);
    });
    with!(data, RisexOpenOrders, |r| for order in &r.orders {
        let _ = parse_open_order_status_report(order, account, aero, ts);
    });
    with!(data, RisexTpslOrders, |r| for order in &r.orders {
        let _ = parse_tpsl_order_status_report(order, account, aero, ts);
    });
    with!(data, RisexAccountTrades, |r| for trade in &r.trades {
        let _ = parse_fill_report(trade, account, aero, ts);
    });
    with!(data, RisexPositions, |r| for position in &r.positions {
        let _ = parse_position_status_report(position, account, aero, ts);
    });
    with!(data, RisexAccountPositionResponse, |r| {
        let _ = parse_account_position_status_report(&r.position, account, aero, ts);
    });

    // Models with no parse function: decode only
    let _ = serde_json::from_slice::<RisexResponse<RisexSystemConfig>>(data);
    let _ = serde_json::from_slice::<RisexResponse<RisexNonceState>>(data);
    let _ = serde_json::from_slice::<RisexResponse<RisexBalance>>(data);
    let _ = serde_json::from_slice::<RisexResponse<RisexPlaceOrderResponse>>(data);
    let _ = serde_json::from_slice::<RisexErrorResponse>(data);
});
