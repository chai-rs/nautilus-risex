//! Feed handler: turns market data frames into Nautilus data and keeps every book in sync.
//!
//! Pure state: no I/O. The caller passes the current time, emits the data and carries out the
//! recovery actions.

use std::sync::Arc;

use ahash::AHashMap;
use dashmap::DashMap;
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::Data,
    identifiers::InstrumentId,
    instruments::{Instrument, InstrumentAny},
};

use super::{
    enums::{RisexWsDataKind, RisexWsStatus},
    messages::{RisexWsData, RisexWsMessage, RisexWsOrderbook},
    parse::{
        parse_book_levels, parse_funding_rate, parse_oracle_price, parse_timestamp_ns,
        parse_trade_tick,
    },
};
use crate::{
    book::{
        BookLevel,
        recovery::{BookRecovery, RecoveryAction, RecoveryConfig},
        sync::{BookOutput, BookSeq, BookSync},
    },
    common::symbol::MarketRegistry,
};

const NANOS_PER_SECOND: u64 = 1_000_000_000;

/// Output of the [`FeedHandler`].
// Nearly every event is data: a box would cost an allocation per frame
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum FeedEvent {
    Data(Data),
    /// The book of `market_id` lost sync or waits too long for a snapshot: the caller carries
    /// out `action`.
    Recover {
        market_id: u16,
        action: RecoveryAction,
    },
}

#[derive(Debug)]
struct Book {
    instrument: InstrumentAny,
    sync: BookSync,
    recovery: BookRecovery,
}

impl Book {
    fn new(instrument: InstrumentAny, config: RecoveryConfig) -> Self {
        Self {
            sync: new_sync(&instrument),
            recovery: BookRecovery::new(config),
            instrument,
        }
    }
}

fn new_sync(instrument: &InstrumentAny) -> BookSync {
    BookSync::new(
        instrument.id(),
        instrument.price_precision(),
        instrument.size_precision(),
    )
}

fn book_events(output: BookOutput) -> Vec<FeedEvent> {
    let mut events = vec![
        FeedEvent::Data(Data::BookDeltas(Box::new(output.deltas))),
        FeedEvent::Data(Data::BookDepth(Box::new(output.depth))),
    ];
    events.extend(
        output
            .quote
            .map(|quote| FeedEvent::Data(Data::Quote(quote))),
    );
    events
}

/// Consumes [`RisexWsMessage`] frames and keeps one [`BookSync`] and one [`BookRecovery`]
/// per market.
#[derive(Debug)]
pub struct FeedHandler {
    registry: Arc<MarketRegistry>,
    instruments: Arc<DashMap<InstrumentId, InstrumentAny>>,
    recovery: RecoveryConfig,
    books: AHashMap<u16, Book>,
}

impl FeedHandler {
    #[must_use]
    pub fn new(
        registry: Arc<MarketRegistry>,
        instruments: Arc<DashMap<InstrumentId, InstrumentAny>>,
        recovery: RecoveryConfig,
    ) -> Self {
        Self {
            registry,
            instruments,
            recovery,
            books: AHashMap::new(),
        }
    }

    /// Starts the snapshot wait of a market. Call when its book subscribe is sent.
    pub fn expect_snapshot(&mut self, market_id: u16, now: UnixNanos) {
        if let Some(book) = self.book(market_id) {
            book.recovery.expect_snapshot(now);
        }
    }

    /// Drops every book. Call when the client disconnects.
    pub fn clear(&mut self) {
        self.books.clear();
    }

    /// Drops the book of a market. Call when its book channel is unsubscribed.
    pub fn remove_book(&mut self, market_id: u16) {
        self.books.remove(&market_id);
    }

    /// Returns whether the market has a book that is not halted.
    #[must_use]
    pub fn tracks(&self, market_id: u16) -> bool {
        self.books
            .get(&market_id)
            .is_some_and(|book| !book.recovery.is_halted())
    }

    /// Makes every book wait for a new snapshot. Call when the socket was connected again.
    pub fn on_reconnected(&mut self, now: UnixNanos) {
        for book in self.books.values_mut() {
            book.sync = new_sync(&book.instrument);
            book.recovery.expect_snapshot(now);
        }
    }

    /// Returns a [`RecoveryAction::FetchRestSnapshot`] for every market whose snapshot wait
    /// is over.
    pub fn on_timer(&mut self, now: UnixNanos) -> Vec<FeedEvent> {
        self.books
            .iter_mut()
            .filter_map(|(market_id, book)| {
                let action = book.recovery.on_timer(now)?;
                Some(FeedEvent::Recover {
                    market_id: *market_id,
                    action,
                })
            })
            .collect()
    }

    /// Applies a REST snapshot to a book that still waits for one.
    pub fn on_rest_snapshot(
        &mut self,
        market_id: u16,
        bids: &[BookLevel],
        asks: &[BookLevel],
        now: UnixNanos,
    ) -> Vec<FeedEvent> {
        let Some(book) = self.books.get_mut(&market_id) else {
            return Vec::new();
        };
        // The WebSocket snapshot arrived first
        if book.sync.is_synced() || book.recovery.is_halted() {
            return Vec::new();
        }

        match book.sync.on_snapshot(bids, asks, None, now, now) {
            Ok(output) => {
                book.recovery.on_snapshot();
                book_events(output)
            }
            Err(e) => {
                log::warn!("REST book snapshot of market {market_id} rejected: {e}");
                Vec::new()
            }
        }
    }

    /// Handles one frame. `now` is `ts_init` of the data and the time of recovery decisions.
    pub fn handle(&mut self, message: RisexWsMessage, now: UnixNanos) -> Vec<FeedEvent> {
        match message {
            RisexWsMessage::Orderbook(frame) => self.on_orderbook(&frame, now),
            RisexWsMessage::Trades(frame) => {
                let ts_event = parse_timestamp_ns(&frame.worker_timestamp).unwrap_or(now);
                frame
                    .market_id
                    .as_deref()
                    .and_then(|market_id| self.instrument(market_id))
                    .and_then(|instrument| {
                        parse_trade_tick(&frame.data, &instrument, ts_event, now)
                            .map_err(|e| log::warn!("Trade frame dropped: {e}"))
                            .ok()
                    })
                    .map(|trade| FeedEvent::Data(Data::Trade(trade)))
                    .into_iter()
                    .collect()
            }
            RisexWsMessage::Oracle(frame) => {
                let ts_event = UnixNanos::from(frame.data.timestamp * NANOS_PER_SECOND);
                let mut events = Vec::new();
                for (market_id, price) in &frame.data.prices {
                    let Some(instrument) = self.instrument(market_id) else {
                        continue;
                    };
                    match parse_oracle_price(price, &instrument, ts_event, now) {
                        Ok((mark, index)) => {
                            events.extend(mark.map(|m| FeedEvent::Data(Data::MarkPrice(m))));
                            events.extend(index.map(|i| FeedEvent::Data(Data::IndexPrice(i))));
                        }
                        Err(e) => log::warn!("Oracle price of market {market_id} dropped: {e}"),
                    }
                }
                events
            }
            RisexWsMessage::ExpectedFundingRate(frame) => {
                let ts_event = parse_timestamp_ns(&frame.worker_timestamp).unwrap_or(now);
                frame
                    .data
                    .iter()
                    .filter_map(|rate| {
                        let instrument = self.instrument(&rate.market_id)?;
                        parse_funding_rate(rate, &instrument, ts_event, now)
                            .map_err(|e| log::warn!("Funding rate frame dropped: {e}"))
                            .ok()
                    })
                    .map(|rate| FeedEvent::Data(Data::FundingRate(rate)))
                    .collect()
            }
            RisexWsMessage::Reconnected => {
                log::info!("RISEx WebSocket reconnected: every book waits for a snapshot");
                self.on_reconnected(now);
                Vec::new()
            }
            RisexWsMessage::Ack(ack) => {
                if ack.status == RisexWsStatus::Error {
                    log::warn!("RISEx WebSocket request rejected: {ack:?}");
                }
                Vec::new()
            }
            RisexWsMessage::System(frame) => {
                log::info!("RISEx system message: {:?}", frame.data);
                Vec::new()
            }
            RisexWsMessage::Unknown(value) => {
                log::debug!("Unknown RISEx WebSocket frame: {value}");
                Vec::new()
            }
            // Account channels belong to the execution client
            _ => Vec::new(),
        }
    }

    fn on_orderbook(
        &mut self,
        frame: &RisexWsData<RisexWsOrderbook>,
        now: UnixNanos,
    ) -> Vec<FeedEvent> {
        let Ok(market_id) = u16::try_from(frame.data.market_id) else {
            return Vec::new();
        };
        let Some(book) = self.book(market_id) else {
            return Vec::new();
        };
        if book.recovery.is_halted() {
            return Vec::new();
        }

        let ts_event = parse_timestamp_ns(&frame.worker_timestamp).unwrap_or(now);
        let seq = frame
            .block_number
            .zip(frame.log_index)
            .map(|(block_number, log_index)| BookSeq {
                block_number,
                log_index,
            });
        let result = parse_book_levels(&frame.data.bids)
            .and_then(|bids| Ok((bids, parse_book_levels(&frame.data.asks)?)))
            .and_then(|(bids, asks)| match frame.kind {
                RisexWsDataKind::Snapshot => {
                    let output = book.sync.on_snapshot(&bids, &asks, seq, ts_event, now)?;
                    book.recovery.on_snapshot();
                    Ok(Some(output))
                }
                RisexWsDataKind::Update => {
                    let (Some(checksum), Some(seq)) = (frame.checksum, seq) else {
                        anyhow::bail!("update without checksum or sequence");
                    };
                    Ok(book
                        .sync
                        .on_update(&bids, &asks, checksum, seq, ts_event, now)?)
                }
            });

        match result {
            Ok(Some(output)) => book_events(output),
            // The book waits for a snapshot
            Ok(None) => Vec::new(),
            Err(e) => {
                // A frame that cannot be applied leaves the book unknown, like a bad checksum
                book.sync = new_sync(&book.instrument);
                let action = book.recovery.on_desync(now);
                log::warn!("Book of market {market_id} lost sync ({e}): {action:?}");
                vec![FeedEvent::Recover { market_id, action }]
            }
        }
    }

    fn instrument(&self, market_id: &str) -> Option<InstrumentAny> {
        let instrument_id = self.registry.instrument_id(market_id.parse().ok()?)?;
        self.instruments
            .get(&instrument_id)
            .map(|instrument| instrument.clone())
    }

    fn book(&mut self, market_id: u16) -> Option<&mut Book> {
        if !self.books.contains_key(&market_id) {
            let instrument = self.instrument(&market_id.to_string())?;
            self.books
                .insert(market_id, Book::new(instrument, self.recovery));
        }
        self.books.get_mut(&market_id)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nautilus_model::{enums::AggressorSide, types::Price};
    use rstest::rstest;
    use rust_decimal_macros::dec;

    use super::*;
    use crate::http::{
        models::{RisexMarkets, RisexResponse},
        parse::parse_instrument,
    };

    const FIXTURE: &str = include_str!("../../tests/fixtures/ws/public_aero_btc.jsonl");
    const MARKETS: &str = include_str!("../../tests/fixtures/http/markets.json");
    const BTC: u16 = 1;
    const AERO: u16 = 15;

    fn handler(snapshot_timeout_secs: u64) -> FeedHandler {
        let registry = Arc::new(MarketRegistry::default());
        let instruments = Arc::new(DashMap::new());
        let markets: RisexResponse<RisexMarkets> = serde_json::from_str(MARKETS).unwrap();
        for market in markets.data.markets.iter().filter(|market| market.active) {
            let instrument = parse_instrument(market, UnixNanos::default()).unwrap();
            registry.insert(market.market_id, instrument.id());
            instruments.insert(instrument.id(), instrument);
        }
        let recovery = RecoveryConfig {
            snapshot_timeout: Duration::from_secs(snapshot_timeout_secs),
            ..RecoveryConfig::default()
        };
        FeedHandler::new(registry, instruments, recovery)
    }

    fn frames() -> Vec<RisexWsMessage> {
        FIXTURE
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn secs(seconds: u64) -> UnixNanos {
        UnixNanos::from(seconds * NANOS_PER_SECOND)
    }

    fn recoveries(events: &[FeedEvent]) -> Vec<(u16, RecoveryAction)> {
        events
            .iter()
            .filter_map(|event| match event {
                FeedEvent::Recover { market_id, action } => Some((*market_id, *action)),
                FeedEvent::Data(_) => None,
            })
            .collect()
    }

    /// First update frame of the BTC book, with a checksum that cannot match.
    fn bad_update() -> RisexWsMessage {
        frames()
            .into_iter()
            .find_map(|frame| match frame {
                RisexWsMessage::Orderbook(mut frame)
                    if frame.kind == RisexWsDataKind::Update && frame.data.market_id == 1 =>
                {
                    frame.checksum = frame.checksum.map(|checksum| checksum ^ 1);
                    Some(RisexWsMessage::Orderbook(frame))
                }
                _ => None,
            })
            .unwrap()
    }

    fn snapshot(market_id: u32) -> RisexWsMessage {
        frames()
            .into_iter()
            .find(|frame| {
                matches!(frame, RisexWsMessage::Orderbook(frame)
                    if frame.kind == RisexWsDataKind::Snapshot && frame.data.market_id == market_id)
            })
            .unwrap()
    }

    #[rstest]
    fn fixture_replay_gives_the_right_data_and_no_recovery() {
        let mut handler = handler(10);
        let now = secs(1_794_280_500);
        let (mut book_frames, mut deltas, mut depths, mut quotes) = (0, 0, 0, 0);
        let (mut trades, mut marks, mut indexes, mut rates) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());

        for frame in frames() {
            let is_book = matches!(frame, RisexWsMessage::Orderbook(_));
            let events = handler.handle(frame, now);
            // A checksum mismatch is the only way to a recovery action
            assert_eq!(recoveries(&events), []);
            if is_book {
                book_frames += 1;
                // Every book frame was applied: deltas and depth come with each of them
                assert!(events.len() >= 2);
            }
            for event in events {
                match event {
                    FeedEvent::Data(Data::BookDeltas(_)) => deltas += 1,
                    FeedEvent::Data(Data::BookDepth(_)) => depths += 1,
                    FeedEvent::Data(Data::Quote(_)) => quotes += 1,
                    FeedEvent::Data(Data::Trade(trade)) => trades.push(trade),
                    FeedEvent::Data(Data::MarkPrice(mark)) => marks.push(mark),
                    FeedEvent::Data(Data::IndexPrice(index)) => indexes.push(index),
                    FeedEvent::Data(Data::FundingRate(rate)) => rates.push(rate),
                    other => panic!("unexpected event {other:?}"),
                }
            }
        }

        // 2 snapshots, 330 BTC updates and 15 AERO updates
        assert_eq!((book_frames, deltas, depths), (347, 347, 347));
        assert!(quotes >= 2);
        assert!(handler.books[&BTC].sync.is_synced());
        assert!(handler.books[&AERO].sync.is_synced());

        let btc = handler.registry.instrument_id(BTC).unwrap();
        let aero = handler.registry.instrument_id(AERO).unwrap();
        assert_eq!(btc.to_string(), "BTC-USDC-PERP.RISEX");

        assert_eq!(trades.len(), 10);
        assert_eq!(trades.iter().filter(|t| t.instrument_id == aero).count(), 1);
        let trade = trades.iter().find(|t| t.instrument_id == btc).unwrap();
        assert_eq!(trade.price, Price::from("74690.0"));
        assert_eq!(trade.size.as_decimal(), dec!(0.000213));
        // The maker sold
        assert_eq!(trade.aggressor_side, AggressorSide::Buy);
        assert_eq!(trade.ts_init, now);
        assert!(trade.ts_event.as_u64() > 1_794_280_485 * NANOS_PER_SECOND);

        // 44 oracle frames with both markets
        assert_eq!((marks.len(), indexes.len()), (88, 88));
        let mark = marks.iter().find(|m| m.instrument_id == aero).unwrap();
        assert_eq!(mark.value, Price::from("0.55301"));
        assert_eq!(mark.ts_event, secs(1_794_280_486));
        let index = indexes.iter().find(|i| i.instrument_id == btc).unwrap();
        assert_eq!(index.value, Price::from("74697.5"));

        // 8 funding frames with both markets
        assert_eq!(rates.len(), 16);
        let rate = rates.iter().find(|r| r.instrument_id == btc).unwrap();
        assert_eq!(rate.rate, dec!(0.0000105616850842));
        assert_eq!(rate.next_funding_ns, Some(secs(1_794_283_200)));
    }

    #[rstest]
    fn bad_checksum_asks_for_a_resubscribe_and_the_book_waits_for_a_snapshot() {
        let mut handler = handler(10);
        handler.handle(snapshot(1), secs(0));

        let events = handler.handle(bad_update(), secs(1));

        assert_eq!(recoveries(&events), [(BTC, RecoveryAction::Resubscribe)]);
        assert_eq!(events.len(), 1);
        assert!(!handler.books[&BTC].sync.is_synced());

        // Updates are dropped without a second action until the snapshot arrives
        let update = frames()
            .into_iter()
            .find(|frame| {
                matches!(frame, RisexWsMessage::Orderbook(frame)
                    if frame.kind == RisexWsDataKind::Update && frame.data.market_id == 1)
            })
            .unwrap();
        assert!(handler.handle(update.clone(), secs(2)).is_empty());

        assert_eq!(handler.handle(snapshot(1), secs(3)).len(), 3);
        assert!(handler.handle(update, secs(3)).len() >= 2);
        // The snapshot ended the wait that the resubscribe started
        assert!(handler.on_timer(secs(1_000)).is_empty());
    }

    #[rstest]
    fn reconnect_makes_every_book_wait_for_a_snapshot() {
        let mut handler = handler(10);
        handler.handle(snapshot(1), secs(0));
        handler.handle(snapshot(15), secs(0));

        assert!(
            handler
                .handle(RisexWsMessage::Reconnected, secs(5))
                .is_empty()
        );

        assert!(!handler.books[&BTC].sync.is_synced());
        assert!(!handler.books[&AERO].sync.is_synced());
        let mut waiting = recoveries(&handler.on_timer(secs(15)));
        waiting.sort_by_key(|(market_id, _)| *market_id);
        assert_eq!(
            waiting,
            [
                (BTC, RecoveryAction::FetchRestSnapshot),
                (AERO, RecoveryAction::FetchRestSnapshot)
            ]
        );
    }

    #[rstest]
    fn rest_snapshot_fills_a_waiting_book_only() {
        let mut handler = handler(10);
        let bids = [BookLevel {
            price: dec!(0.55),
            quantity: dec!(100),
            order_count: 1,
        }];
        // Not subscribed: nothing to fill
        assert!(
            handler
                .on_rest_snapshot(AERO, &bids, &[], secs(0))
                .is_empty()
        );

        handler.expect_snapshot(AERO, secs(0));
        assert_eq!(
            recoveries(&handler.on_timer(secs(10))),
            [(AERO, RecoveryAction::FetchRestSnapshot)]
        );
        // Deltas and depth; a one-sided book has no quote
        assert_eq!(
            handler.on_rest_snapshot(AERO, &bids, &[], secs(10)).len(),
            2
        );
        assert!(handler.on_timer(secs(1_000)).is_empty());

        // The book is in sync: a late REST snapshot must not replace it
        assert!(
            handler
                .on_rest_snapshot(AERO, &bids, &[], secs(11))
                .is_empty()
        );
    }

    #[rstest]
    fn halted_and_removed_books_ignore_frames() {
        let mut handler = handler(10);
        handler.recovery.max_resubscribes = 0;
        handler.handle(snapshot(1), secs(0));

        let reconnect = handler.handle(bad_update(), secs(1));
        assert_eq!(
            recoveries(&reconnect),
            [(BTC, RecoveryAction::ReconnectSocket)]
        );
        handler.handle(snapshot(1), secs(2));
        let halt = handler.handle(bad_update(), secs(3));
        assert_eq!(recoveries(&halt), [(BTC, RecoveryAction::Halt)]);

        assert!(!handler.tracks(BTC));
        assert!(handler.handle(snapshot(1), secs(4)).is_empty());

        // A new subscribe starts the market again
        handler.remove_book(BTC);
        assert_eq!(handler.handle(snapshot(1), secs(5)).len(), 3);
        assert!(handler.tracks(BTC));
    }

    #[rstest]
    fn frames_of_unknown_markets_are_dropped() {
        let mut handler = handler(10);
        handler.instruments.clear();

        for frame in frames() {
            assert!(handler.handle(frame, secs(0)).is_empty());
        }
    }
}
