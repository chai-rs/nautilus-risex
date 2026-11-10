//! Prints the most recent public trades of one RISEx market, without a `LiveNode`.
//!
//! Run with: `cargo run --bin risex-trades-probe -- [INSTRUMENT_ID] [LIMIT]`
//!
//! Read-only and public: two GET requests, no account, no key.

use nautilus_model::{
    identifiers::InstrumentId,
    instruments::{Instrument, InstrumentAny},
};
use nautilus_risex::{
    common::{consts::HTTP_TIMEOUT_SECS, enums::RisexEnvironment},
    http::client::RisexHttpClient,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let instrument_id =
        InstrumentId::from(args.next().as_deref().unwrap_or("AERO-USDC-PERP.RISEX"));
    let limit: u32 = args.next().map_or(Ok(20), |limit| limit.parse())?;

    let client = RisexHttpClient::new(RisexEnvironment::default(), None, HTTP_TIMEOUT_SECS, None)?;
    let instruments = client.request_instruments().await?;
    let instrument: &InstrumentAny = instruments
        .iter()
        .find(|instrument| instrument.id() == instrument_id)
        .ok_or_else(|| anyhow::anyhow!("{instrument_id} is not an active market"))?;

    let trades = client
        .request_trades(instrument, None, None, Some(limit))
        .await?;
    println!("{} trades of {instrument_id}, oldest first", trades.len());
    for trade in trades {
        println!("{trade}");
    }
    Ok(())
}
