//! Flattens a RISEx account: cancels every open order and every waiting TP/SL
//! order, then closes every position with a reduce-only IOC order.
//!
//! The plan is printed first and nothing is sent before Enter is pressed.
//!
//! Environment selection: pass `testnet` as the first argv to target testnet;
//! defaults to mainnet.
//!
//! Environment variables (read at run time only):
//! - `RISEX_ACCOUNT_ADDRESS`, `RISEX_SIGNER_PRIVATE_KEY`
//!   (or the `RISEX_TESTNET_*` variants).
//!
//! Run with:
//!
//! ```bash
//! cargo run --bin risex-flatten           # mainnet
//! cargo run --bin risex-flatten testnet   # testnet
//! ```

use std::{
    collections::{BTreeSet, HashMap},
    env,
    io::{BufRead, Write},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::QuoteTick,
    instruments::{Instrument, InstrumentAny},
};
use nautilus_risex::{
    common::{
        consts::MAX_PAGE_LIMIT,
        credential::RisexCredential,
        enums::{RisexEnvironment, RisexTpslOrderStatus},
        parse::client_order_id_to_u64,
    },
    config::exec::RisexExecClientConfig,
    execution::{order::close_order, session::RisexSession},
    http::{
        client::{RisexHttpClient, RisexRawHttpClient},
        models::RisexPosition,
        query::{RisexOpenOrdersQuery, RisexPositionsQuery, RisexTpslOrdersQuery},
    },
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    nautilus_common::logging::ensure_logging_initialized();

    let environment = if env::args().nth(1).is_some_and(|arg| arg == "testnet") {
        RisexEnvironment::Testnet
    } else {
        RisexEnvironment::Mainnet
    };
    let credential = RisexCredential::resolve(None, None, environment)?
        .context("no RISEx credentials in env")?;
    let account = credential.account_address().to_string();
    let defaults = RisexExecClientConfig::default();

    let http = RisexHttpClient::new(environment, None, defaults.http_timeout_secs, None)?;
    let registry = http.market_registry();
    let instruments: HashMap<u16, InstrumentAny> = http
        .request_instruments()
        .await?
        .into_iter()
        .filter_map(|instrument| Some((registry.market_id(&instrument.id())?, instrument)))
        .collect();
    let raw = http.raw();

    let order_markets = open_order_markets(raw, &account).await?;
    let tpsl_markets = tpsl_markets(raw, &account).await?;
    let positions = open_positions(raw, &account).await?;

    println!();
    println!("RISEx flatten");
    println!("  environment        : {environment:?}");
    println!("  account            : {account}");
    println!("  cancel all orders  : markets {order_markets:?}");
    println!("  cancel all TP/SL   : markets {tpsl_markets:?}");
    for position in &positions {
        println!(
            "  close position     : market {} {} {}",
            position.market_id, position.side, position.size
        );
    }
    if order_markets.is_empty() && tpsl_markets.is_empty() && positions.is_empty() {
        println!("Nothing to do.");
        return Ok(());
    }
    print!("\nProceed? [Enter to continue, Ctrl+C to abort] ");
    std::io::stdout().flush()?;
    std::io::stdin().lock().read_line(&mut String::new())?;

    let session =
        RisexSession::connect(Arc::new(raw.clone()), &credential, defaults.deadline_secs).await?;
    let mut failed = 0;
    let mut report = |what: String, result: anyhow::Result<String>| match result {
        Ok(detail) => println!("{what}: ok {detail}"),
        Err(e) => {
            println!("{what}: FAILED {e:#}");
            failed += 1;
        }
    };

    // Orders go first: a resting order can block a reduce-only close.
    for market_id in order_markets {
        let result = session.cancel_all_orders(market_id).await;
        report(
            format!("cancel all orders, market {market_id}"),
            result.map(|()| String::new()).map_err(Into::into),
        );
    }
    for market_id in tpsl_markets {
        let result = session.cancel_all_tpsl(market_id).await;
        report(
            format!("cancel all TP/SL, market {market_id}"),
            result.map(|()| String::new()).map_err(Into::into),
        );
    }
    for position in &positions {
        let result = close(
            &http,
            &session,
            &instruments,
            position,
            defaults.market_order_slippage_bps,
        )
        .await;
        report(
            format!("close position, market {}", position.market_id),
            result,
        );
    }

    // shortcut: one IOC order per position, so a thin book leaves a rest. Run again until
    // it prints "Nothing to do"; upgrade to a fill-checking loop if that is too manual.
    anyhow::ensure!(
        failed == 0,
        "{failed} step(s) failed, check the account and run again"
    );
    println!("Sent. Run again to confirm the account is flat.");
    Ok(())
}

async fn open_order_markets(
    raw: &RisexRawHttpClient,
    account: &str,
) -> anyhow::Result<BTreeSet<u16>> {
    let mut query = RisexOpenOrdersQuery {
        account: account.to_string(),
        limit: Some(MAX_PAGE_LIMIT),
        ..Default::default()
    };
    let mut markets = BTreeSet::new();
    let mut fetched = 0;
    loop {
        let orders = raw.get_open_orders(&query).await?.orders;
        markets.extend(orders.iter().map(|order| order.market_id));
        fetched += orders.len() as u64;
        if orders.len() < MAX_PAGE_LIMIT as usize {
            return Ok(markets);
        }
        query.start_index = Some(fetched);
    }
}

async fn tpsl_markets(raw: &RisexRawHttpClient, account: &str) -> anyhow::Result<BTreeSet<u16>> {
    let mut query = RisexTpslOrdersQuery {
        account: account.to_string(),
        statuses: vec![RisexTpslOrderStatus::Accepted],
        limit: Some(MAX_PAGE_LIMIT),
        ..Default::default()
    };
    let mut markets = BTreeSet::new();
    let mut fetched = 0;
    loop {
        let page = raw.get_tpsl_orders(&query).await?;
        markets.extend(page.orders.iter().map(|order| order.market_id));
        fetched += page.orders.len() as u64;
        if page.orders.is_empty() || fetched >= page.total {
            return Ok(markets);
        }
        query.page = Some(page.page + 1);
    }
}

async fn open_positions(
    raw: &RisexRawHttpClient,
    account: &str,
) -> anyhow::Result<Vec<RisexPosition>> {
    let mut query = RisexPositionsQuery {
        account: account.to_string(),
        ..Default::default()
    };
    let mut positions = Vec::new();
    loop {
        let page = raw.get_positions(&query).await?;
        let last = !page.has_next_page || page.positions.is_empty();
        positions.extend(page.positions.into_iter().filter(|p| !p.size.is_zero()));
        if last {
            return Ok(positions);
        }
        query.page = Some(page.page + 1);
    }
}

async fn close(
    http: &RisexHttpClient,
    session: &RisexSession,
    instruments: &HashMap<u16, InstrumentAny>,
    position: &RisexPosition,
    slippage_bps: u32,
) -> anyhow::Result<String> {
    let instrument = instruments
        .get(&position.market_id)
        .context("market is not in the instrument list")?;
    let book = http.request_book_snapshot(instrument, Some(1)).await?;
    let (Some(bid), Some(ask)) = (book.bids.first(), book.asks.first()) else {
        anyhow::bail!("book has no bid or no ask");
    };
    let quote = QuoteTick::new(
        instrument.id(),
        bid.0,
        ask.0,
        bid.1,
        ask.1,
        UnixNanos::default(),
        UnixNanos::default(),
    );
    // A new id per run: the venue may refuse a client order id it has seen.
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let order = close_order(
        position.market_id,
        instrument,
        position.side,
        position.size,
        &quote,
        slippage_bps,
        client_order_id_to_u64(&format!("FLATTEN-{}-{now}", position.market_id)),
    )?;
    Ok(session.place_order(&order).await?.order_id)
}
