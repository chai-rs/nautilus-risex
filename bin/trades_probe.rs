//! Prints the most recent public trades of one RISEx market, without a `LiveNode`.
//!
//! Run with: `cargo run --bin risex-trades-probe -- [INSTRUMENT_ID] [LIMIT]`
//!
//! Read-only and public: two GET requests, no account, no key.

use nautilus_model::{
    data::TradeTick,
    identifiers::InstrumentId,
    instruments::{Instrument, InstrumentAny},
};
use nautilus_risex::{
    common::{consts::HTTP_TIMEOUT_SECS, enums::RisexEnvironment},
    http::client::RisexHttpClient,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (instrument_id, limit) = parse_args(std::env::args().skip(1))?;

    let client = RisexHttpClient::new(RisexEnvironment::default(), None, HTTP_TIMEOUT_SECS, None)?;
    let instruments = client.request_instruments().await?;
    let instrument = find_instrument(&instruments, instrument_id)?;

    let trades = client
        .request_trades(instrument, None, None, Some(limit))
        .await?;
    for line in report(instrument_id, &trades) {
        println!("{line}");
    }
    Ok(())
}

/// `[INSTRUMENT_ID] [LIMIT]`, with AERO and 20 as defaults.
fn parse_args(mut args: impl Iterator<Item = String>) -> anyhow::Result<(InstrumentId, u32)> {
    let instrument_id =
        InstrumentId::from(args.next().as_deref().unwrap_or("AERO-USDC-PERP.RISEX"));
    let limit: u32 = args.next().map_or(Ok(20), |limit| limit.parse())?;
    Ok((instrument_id, limit))
}

fn find_instrument(
    instruments: &[InstrumentAny],
    instrument_id: InstrumentId,
) -> anyhow::Result<&InstrumentAny> {
    instruments
        .iter()
        .find(|instrument| instrument.id() == instrument_id)
        .ok_or_else(|| anyhow::anyhow!("{instrument_id} is not an active market"))
}

/// The printed lines: a header, then one row per trade in the given order.
fn report(instrument_id: InstrumentId, trades: &[TradeTick]) -> Vec<String> {
    let header = format!("{} trades of {instrument_id}, oldest first", trades.len());
    std::iter::once(header)
        .chain(trades.iter().map(ToString::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use nautilus_core::UnixNanos;
    use nautilus_risex::http::{
        models::{RisexMarkets, RisexResponse, RisexTrades},
        parse::{parse_instrument, parse_trade_tick},
    };
    use serde::de::DeserializeOwned;

    use super::*;

    /// Loads `tests/fixtures/http/{name}.json` and unwraps the envelope.
    fn fixture<T: DeserializeOwned>(name: &str) -> T {
        let path = format!(
            "{}/tests/fixtures/http/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let json = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_str::<RisexResponse<T>>(&json)
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .data
    }

    /// The active markets of the recorded `GET /v1/markets`.
    fn instruments() -> Vec<InstrumentAny> {
        fixture::<RisexMarkets>("markets")
            .markets
            .iter()
            .filter(|market| market.active)
            .map(|market| parse_instrument(market, UnixNanos::default()).unwrap())
            .collect()
    }

    fn args(args: &[&str]) -> anyhow::Result<(InstrumentId, u32)> {
        parse_args(args.iter().map(ToString::to_string))
    }

    #[test]
    fn args_default_to_aero_and_20() {
        let aero = InstrumentId::from("AERO-USDC-PERP.RISEX");
        let btc = InstrumentId::from("BTC-USDC-PERP.RISEX");
        assert_eq!(args(&[]).unwrap(), (aero, 20));
        assert_eq!(args(&["BTC-USDC-PERP.RISEX"]).unwrap(), (btc, 20));
        assert_eq!(args(&["BTC-USDC-PERP.RISEX", "5"]).unwrap(), (btc, 5));
        assert!(args(&["BTC-USDC-PERP.RISEX", "five"]).is_err());
    }

    #[test]
    fn unknown_market_is_an_error() {
        let instruments = instruments();
        let aero = InstrumentId::from("AERO-USDC-PERP.RISEX");
        assert_eq!(find_instrument(&instruments, aero).unwrap().id(), aero);

        let none = InstrumentId::from("NONE-USDC-PERP.RISEX");
        let e = find_instrument(&instruments, none).unwrap_err();
        assert_eq!(
            e.to_string(),
            "NONE-USDC-PERP.RISEX is not an active market"
        );
    }

    #[test]
    fn report_of_the_recorded_trades() {
        let instrument_id = InstrumentId::from("AERO-USDC-PERP.RISEX");
        let instruments = instruments();
        let instrument = find_instrument(&instruments, instrument_id).unwrap();
        // The venue sends newest first; `request_trades` reverses, and so does this.
        let trades: Vec<TradeTick> = fixture::<RisexTrades>("trades_aero")
            .trades
            .iter()
            .rev()
            .map(|trade| parse_trade_tick(trade, instrument, UnixNanos::default()).unwrap())
            .collect();

        let lines = report(instrument_id, &trades);
        assert_eq!(lines.len(), 51);
        assert_eq!(lines[0], "50 trades of AERO-USDC-PERP.RISEX, oldest first");
        assert_eq!(
            lines[1],
            "AERO-USDC-PERP.RISEX,0.55444,901,BUY,72cb831a969a238407766a6c9dcc8c96,1794279821000000000"
        );
        assert!(lines[50].starts_with("AERO-USDC-PERP.RISEX,0.55296,33,SELL,"));
        assert!(lines[50].ends_with(",1794280444000000000"));
        assert_eq!(report(instrument_id, &[]).len(), 1);
    }
}
