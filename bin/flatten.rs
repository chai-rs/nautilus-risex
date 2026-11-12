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
    env, fmt,
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
        models::{RisexOpenOrder, RisexPosition, RisexTpslOrder},
        parse::RisexBookSnapshot,
        query::{RisexOpenOrdersQuery, RisexPositionsQuery, RisexTpslOrdersQuery},
    },
    signing::action::PlaceOrder,
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

    let plan = Plan::new(
        &open_orders(raw, &account).await?,
        &tpsl_orders(raw, &account).await?,
        open_positions(raw, &account).await?,
    );

    println!();
    println!("RISEx flatten");
    println!("  environment        : {environment:?}");
    println!("  account            : {account}");
    print!("{plan}");
    if plan.is_empty() {
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
    for market_id in plan.order_markets {
        let result = session.cancel_all_orders(market_id).await;
        report(
            format!("cancel all orders, market {market_id}"),
            result.map(|()| String::new()).map_err(Into::into),
        );
    }
    for market_id in plan.tpsl_markets {
        let result = session.cancel_all_tpsl(market_id).await;
        report(
            format!("cancel all TP/SL, market {market_id}"),
            result.map(|()| String::new()).map_err(Into::into),
        );
    }
    for position in &plan.positions {
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

/// What one run sends: built from the account state, printed, then sent after Enter.
struct Plan {
    /// Markets with an open order: one cancel-all each.
    order_markets: BTreeSet<u16>,
    /// Markets with a waiting TP/SL order: one TP/SL cancel-all each.
    tpsl_markets: BTreeSet<u16>,
    /// Positions to close, one reduce-only IOC order each.
    positions: Vec<RisexPosition>,
}

impl Plan {
    fn new(
        orders: &[RisexOpenOrder],
        tpsl: &[RisexTpslOrder],
        mut positions: Vec<RisexPosition>,
    ) -> Self {
        positions.retain(|position| !position.size.is_zero());
        Self {
            order_markets: orders.iter().map(|order| order.market_id).collect(),
            tpsl_markets: tpsl.iter().map(|order| order.market_id).collect(),
            positions,
        }
    }

    fn is_empty(&self) -> bool {
        self.order_markets.is_empty() && self.tpsl_markets.is_empty() && self.positions.is_empty()
    }
}

impl fmt::Display for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "  cancel all orders  : markets {:?}", self.order_markets)?;
        writeln!(f, "  cancel all TP/SL   : markets {:?}", self.tpsl_markets)?;
        for position in &self.positions {
            writeln!(
                f,
                "  close position     : market {} {} {}",
                position.market_id, position.side, position.size
            )?;
        }
        Ok(())
    }
}

async fn open_orders(
    raw: &RisexRawHttpClient,
    account: &str,
) -> anyhow::Result<Vec<RisexOpenOrder>> {
    let mut query = RisexOpenOrdersQuery {
        account: account.to_string(),
        limit: Some(MAX_PAGE_LIMIT),
        ..Default::default()
    };
    let mut orders = Vec::new();
    loop {
        let page = raw.get_open_orders(&query).await?.orders;
        let last = page.len() < MAX_PAGE_LIMIT as usize;
        orders.extend(page);
        if last {
            return Ok(orders);
        }
        query.start_index = Some(orders.len() as u64);
    }
}

async fn tpsl_orders(
    raw: &RisexRawHttpClient,
    account: &str,
) -> anyhow::Result<Vec<RisexTpslOrder>> {
    let mut query = RisexTpslOrdersQuery {
        account: account.to_string(),
        statuses: vec![RisexTpslOrderStatus::Accepted],
        limit: Some(MAX_PAGE_LIMIT),
        ..Default::default()
    };
    let mut orders = Vec::new();
    loop {
        let page = raw.get_tpsl_orders(&query).await?;
        let empty = page.orders.is_empty();
        orders.extend(page.orders);
        if empty || orders.len() as u64 >= page.total {
            return Ok(orders);
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
        positions.extend(page.positions);
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
    // A new id per run: the venue may refuse a client order id it has seen.
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let order = closing_order(instrument, position, &book, slippage_bps, now)?;
    Ok(session.place_order(&order).await?.order_id)
}

/// The reduce-only IOC order that closes `position` against the top of `book`.
fn closing_order(
    instrument: &InstrumentAny,
    position: &RisexPosition,
    book: &RisexBookSnapshot,
    slippage_bps: u32,
    now_secs: u64,
) -> anyhow::Result<PlaceOrder> {
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
    close_order(
        position.market_id,
        instrument,
        position.side,
        position.size,
        &quote,
        slippage_bps,
        client_order_id_to_u64(&format!("FLATTEN-{}-{now_secs}", position.market_id)),
    )
}

#[cfg(test)]
mod tests {
    use nautilus_risex::{
        common::enums::RisexOrderSide,
        http::{
            models::{
                RisexMarkets, RisexOpenOrders, RisexOrderBook, RisexPositions, RisexResponse,
                RisexTpslOrders,
            },
            parse::{parse_book_snapshot, parse_instrument},
        },
    };
    use rust_decimal::Decimal;
    use serde::de::DeserializeOwned;

    use super::*;

    const AERO: u16 = 15;
    const NOW: u64 = 1_800_000_000;

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

    fn positions() -> Vec<RisexPosition> {
        fixture::<RisexPositions>("positions").positions
    }

    /// The AERO instrument, its recorded long position and its recorded book.
    fn aero() -> (InstrumentAny, RisexPosition, RisexBookSnapshot) {
        let markets = fixture::<RisexMarkets>("markets").markets;
        let market = markets.iter().find(|m| m.market_id == AERO).unwrap();
        let instrument = parse_instrument(market, UnixNanos::default()).unwrap();
        let position = positions()
            .into_iter()
            .find(|p| p.market_id == AERO)
            .unwrap();
        let book =
            parse_book_snapshot(&fixture::<RisexOrderBook>("orderbook_aero"), &instrument).unwrap();
        (instrument, position, book)
    }

    #[test]
    fn plan_of_the_recorded_account() {
        let plan = Plan::new(
            &fixture::<RisexOpenOrders>("orders_open").orders,
            &fixture::<RisexTpslOrders>("tpsl_orders").orders,
            positions(),
        );

        // 635 open orders: some in each market from 1 to 54, but none in 13.
        let markets: BTreeSet<u16> = (1..=54).filter(|id| *id != 13).collect();
        assert_eq!(plan.order_markets, markets);
        assert_eq!(plan.tpsl_markets, BTreeSet::from([33]));
        assert_eq!(plan.positions.len(), 53);
        assert!(!plan.is_empty());

        let text = plan.to_string();
        assert_eq!(text.lines().count(), 2 + 53);
        assert!(text.starts_with("  cancel all orders  : markets {1, 2, 3, "));
        assert!(text.contains("\n  cancel all TP/SL   : markets {33}\n"));
        assert!(text.contains("\n  close position     : market 15 BUY 99299\n"));
    }

    #[test]
    fn flat_position_is_dropped_and_empty_plan_has_nothing_to_do() {
        let mut positions = positions();
        positions.truncate(2);
        positions[0].size = Decimal::ZERO;
        let kept = positions[1].market_id;

        let plan = Plan::new(&[], &[], positions);
        assert_eq!(plan.positions.len(), 1);
        assert_eq!(plan.positions[0].market_id, kept);
        assert!(!plan.is_empty());

        let plan = Plan::new(&[], &[], Vec::new());
        assert!(plan.is_empty());
        assert_eq!(plan.to_string().lines().count(), 2);
    }

    #[test]
    fn closing_order_of_a_long_and_of_a_short() {
        let (instrument, mut position, book) = aero();
        let id = client_order_id_to_u64("FLATTEN-15-1800000000");

        // Long 99299 at bid 0.55296: sell, 50 bps under the bid, down to the tick.
        let order = closing_order(&instrument, &position, &book, 50, NOW).unwrap();
        let expected = PlaceOrder {
            market_id: AERO,
            size_steps: 99_299,
            price_ticks: 55_019,
            side: 1,
            post_only: false,
            reduce_only: true,
            stp_mode: 0,
            order_type: 1,
            time_in_force: 3,
            builder_id: 0,
            client_order_id: id,
            ttl_units: 0,
        };
        assert_eq!(order, expected);

        // Short at ask 0.55313: buy, 50 bps over the ask, up to the tick.
        position.side = RisexOrderSide::Sell;
        position.size = -position.size;
        let order = closing_order(&instrument, &position, &book, 50, NOW).unwrap();
        assert_eq!(
            order,
            PlaceOrder {
                price_ticks: 55_590,
                side: 0,
                ..expected
            }
        );

        // A later run has another client order id.
        let later = closing_order(&instrument, &position, &book, 50, NOW + 1).unwrap();
        assert_ne!(later.client_order_id, id);
    }

    #[test]
    fn closing_order_needs_both_sides_of_the_book() {
        let (instrument, position, mut book) = aero();
        book.asks.clear();
        let e = closing_order(&instrument, &position, &book, 50, NOW).unwrap_err();
        assert_eq!(e.to_string(), "book has no bid or no ask");
    }
}
