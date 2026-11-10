//! Per-market book synchronization: snapshot gate, sequence check, checksum check and the
//! Nautilus data that each accepted frame produces.

use std::fmt::Display;

use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{BookOrder, DEPTH10_LEN, OrderBookDelta, OrderBookDeltas, OrderBookDepth, QuoteTick},
    enums::{BookAction, OrderSide, RecordFlag},
    identifiers::InstrumentId,
    types::{Price, Quantity},
};
use rust_decimal::Decimal;

use super::{BookLevel, LocalBook};

/// Venue position of a book message. Ordered by block, then by log index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BookSeq {
    pub block_number: u64,
    pub log_index: u64,
}

impl BookSeq {
    /// Packs the position into one Nautilus sequence number that keeps the venue order.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        // shortcut: 20 bits hold log indexes below 1,048,576; larger ones in one block share a
        // sequence number. Widen the field if a block ever carries that many logs.
        let log_index = if self.log_index < (1 << 20) {
            self.log_index
        } else {
            (1 << 20) - 1
        };
        (self.block_number << 20) | log_index
    }
}

/// Why a book lost sync. The market emits nothing more until the next snapshot.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BookSyncError {
    #[error("book checksum mismatch: venue {expected}, local {computed}")]
    ChecksumMismatch { expected: u32, computed: u32 },
    #[error("book sequence went backward: last {last:?}, received {received:?}")]
    SequenceBackward { last: BookSeq, received: BookSeq },
    #[error("invalid book level: {0}")]
    InvalidLevel(String),
}

/// Nautilus data produced by one accepted book frame.
#[derive(Debug, Clone)]
pub struct BookOutput {
    pub deltas: OrderBookDeltas,
    /// Present only when the top-of-book price or size changed and both sides have a level.
    pub quote: Option<QuoteTick>,
    /// Up to ten levels per side; a side with fewer levels is shorter, never padded.
    pub depth: OrderBookDepth,
}

/// Synchronization state of one market's `L2_MBP` book.
#[derive(Debug)]
pub struct BookSync {
    instrument_id: InstrumentId,
    price_precision: u8,
    size_precision: u8,
    book: LocalBook,
    synced: bool,
    last_seq: Option<BookSeq>,
    last_top: Option<[Decimal; 4]>,
}

impl BookSync {
    /// Creates a book that waits for its first snapshot.
    #[must_use]
    pub fn new(instrument_id: InstrumentId, price_precision: u8, size_precision: u8) -> Self {
        Self {
            instrument_id,
            price_precision,
            size_precision,
            book: LocalBook::default(),
            synced: false,
            last_seq: None,
            last_top: None,
        }
    }

    /// Returns whether updates are applied. False before the first snapshot and after an error.
    #[must_use]
    pub const fn is_synced(&self) -> bool {
        self.synced
    }

    #[must_use]
    pub const fn book(&self) -> &LocalBook {
        &self.book
    }

    /// Replaces the book with a snapshot. Works in every state, so a snapshot that arrives
    /// before the subscribe acknowledgement is used like any other.
    ///
    /// Pass `seq = None` for a REST snapshot, which carries no position; the next update is
    /// then accepted on its checksum alone.
    ///
    /// # Errors
    ///
    /// Returns an error if a level does not convert to a Nautilus price or quantity. The book
    /// is then out of sync.
    pub fn on_snapshot(
        &mut self,
        bids: &[BookLevel],
        asks: &[BookLevel],
        seq: Option<BookSeq>,
        ts_event: UnixNanos,
        ts_init: UnixNanos,
    ) -> Result<BookOutput, BookSyncError> {
        let result = self.snapshot(bids, asks, seq, ts_event, ts_init);
        self.synced = result.is_ok();
        result
    }

    /// Applies an incremental update and verifies it.
    ///
    /// Returns `Ok(None)` when the book is not synced or the update carries no level. Updates
    /// before a snapshot are dropped, not buffered: the venue sends the snapshot before any
    /// update of that subscription, a REST snapshot has no position to replay a buffer against,
    /// and the checksum of the first update after the snapshot exposes anything that was lost.
    ///
    /// # Errors
    ///
    /// Returns an error if the position went backward, the checksum of the local book differs
    /// from `checksum`, or a level does not convert. The book is then out of sync: later updates
    /// are dropped until the next snapshot, so the caller sees one error per loss of sync.
    pub fn on_update(
        &mut self,
        bids: &[BookLevel],
        asks: &[BookLevel],
        checksum: u32,
        seq: BookSeq,
        ts_event: UnixNanos,
        ts_init: UnixNanos,
    ) -> Result<Option<BookOutput>, BookSyncError> {
        if !self.synced {
            return Ok(None);
        }

        let result = self.update(bids, asks, checksum, seq, ts_event, ts_init);
        self.synced = result.is_ok();
        result
    }

    fn snapshot(
        &mut self,
        bids: &[BookLevel],
        asks: &[BookLevel],
        seq: Option<BookSeq>,
        ts_event: UnixNanos,
        ts_init: UnixNanos,
    ) -> Result<BookOutput, BookSyncError> {
        self.book.apply_snapshot(bids, asks);
        self.last_seq = seq;
        let sequence = seq.map_or(0, BookSeq::as_u64);
        let flags = RecordFlag::F_SNAPSHOT as u8;

        // Deltas come from the local book, so they hold exactly the levels the checksum covers
        let mut deltas = vec![OrderBookDelta::clear(
            self.instrument_id,
            sequence,
            ts_event,
            ts_init,
        )];
        for level in self.book.bids() {
            deltas.push(self.delta(OrderSide::Buy, level, flags, sequence, ts_event, ts_init)?);
        }
        for level in self.book.asks() {
            deltas.push(self.delta(OrderSide::Sell, level, flags, sequence, ts_event, ts_init)?);
        }

        self.output(deltas, sequence, ts_event, ts_init)
    }

    fn update(
        &mut self,
        bids: &[BookLevel],
        asks: &[BookLevel],
        checksum: u32,
        seq: BookSeq,
        ts_event: UnixNanos,
        ts_init: UnixNanos,
    ) -> Result<Option<BookOutput>, BookSyncError> {
        if let Some(last) = self.last_seq
            && seq < last
        {
            return Err(BookSyncError::SequenceBackward {
                last,
                received: seq,
            });
        }

        self.book.apply_update(bids, asks);
        let computed = self.book.checksum();
        if computed != checksum {
            return Err(BookSyncError::ChecksumMismatch {
                expected: checksum,
                computed,
            });
        }
        self.last_seq = Some(seq);

        let sequence = seq.as_u64();
        let mut deltas = Vec::with_capacity(bids.len() + asks.len());
        for (side, levels) in [(OrderSide::Buy, bids), (OrderSide::Sell, asks)] {
            for level in levels {
                deltas.push(self.delta(side, level, 0, sequence, ts_event, ts_init)?);
            }
        }

        if deltas.is_empty() {
            return Ok(None);
        }
        self.output(deltas, sequence, ts_event, ts_init).map(Some)
    }

    fn output(
        &mut self,
        mut deltas: Vec<OrderBookDelta>,
        sequence: u64,
        ts_event: UnixNanos,
        ts_init: UnixNanos,
    ) -> Result<BookOutput, BookSyncError> {
        if let Some(last) = deltas.last_mut() {
            last.flags |= RecordFlag::F_LAST as u8;
        }
        let deltas = OrderBookDeltas::new_checked(self.instrument_id, deltas).map_err(invalid)?;

        let (bids, bid_counts) = self.depth_side(OrderSide::Buy, self.book.bids())?;
        let (asks, ask_counts) = self.depth_side(OrderSide::Sell, self.book.asks())?;

        let top = match (bids.first(), asks.first()) {
            (Some(bid), Some(ask)) => Some((bid, ask)),
            _ => None,
        };
        let top_key = match (self.book.best_bid(), self.book.best_ask()) {
            (Some(bid), Some(ask)) => Some([bid.price, bid.quantity, ask.price, ask.quantity]),
            _ => None,
        };
        let quote = match top {
            Some((bid, ask)) if top_key != self.last_top => {
                self.last_top = top_key;
                Some(
                    QuoteTick::new_checked(
                        self.instrument_id,
                        bid.price,
                        ask.price,
                        bid.size,
                        ask.size,
                        ts_event,
                        ts_init,
                    )
                    .map_err(invalid)?,
                )
            }
            _ => None,
        };

        let depth = OrderBookDepth::new_checked(
            self.instrument_id,
            bids,
            asks,
            bid_counts,
            ask_counts,
            RecordFlag::F_SNAPSHOT as u8,
            sequence,
            ts_event,
            ts_init,
        )
        .map_err(invalid)?;

        Ok(BookOutput {
            deltas,
            quote,
            depth,
        })
    }

    fn depth_side<'a>(
        &self,
        side: OrderSide,
        levels: impl Iterator<Item = &'a BookLevel>,
    ) -> Result<(Vec<BookOrder>, Vec<u32>), BookSyncError> {
        levels
            .take(DEPTH10_LEN)
            .map(|level| Ok((self.order(side, level)?, level.order_count)))
            .collect()
    }

    fn delta(
        &self,
        side: OrderSide,
        level: &BookLevel,
        flags: u8,
        sequence: u64,
        ts_event: UnixNanos,
        ts_init: UnixNanos,
    ) -> Result<OrderBookDelta, BookSyncError> {
        let action = if flags & RecordFlag::F_SNAPSHOT as u8 != 0 {
            BookAction::Add
        } else if level.quantity.is_zero() {
            BookAction::Delete
        } else {
            BookAction::Update
        };

        OrderBookDelta::new_checked(
            self.instrument_id,
            action,
            self.order(side, level)?,
            flags,
            sequence,
            ts_event,
            ts_init,
        )
        .map_err(invalid)
    }

    fn order(&self, side: OrderSide, level: &BookLevel) -> Result<BookOrder, BookSyncError> {
        let price = Price::from_decimal_dp(level.price, self.price_precision).map_err(invalid)?;
        let size =
            Quantity::from_decimal_dp(level.quantity, self.size_precision).map_err(invalid)?;

        // Order ID 0: an L2_MBP book keys its levels by price
        Ok(BookOrder::new(side, price, size, 0))
    }
}

fn invalid(e: impl Display) -> BookSyncError {
    BookSyncError::InvalidLevel(e.to_string())
}

#[cfg(test)]
mod tests {
    use nautilus_model::{enums::BookType, orderbook::OrderBook};
    use rstest::rstest;
    use serde_json::Value;

    use super::*;
    use crate::book::level;

    const F_LAST: u8 = RecordFlag::F_LAST as u8;
    const F_SNAPSHOT: u8 = RecordFlag::F_SNAPSHOT as u8;

    fn seq(block_number: u64, log_index: u64) -> BookSeq {
        BookSeq {
            block_number,
            log_index,
        }
    }

    fn ts(nanos: u64) -> UnixNanos {
        UnixNanos::from(nanos)
    }

    fn checksum_after(sync: &BookSync, bids: &[BookLevel], asks: &[BookLevel]) -> u32 {
        let mut book = sync.book().clone();
        book.apply_update(bids, asks);
        book.checksum()
    }

    /// AERO-like book: bids 0.50 and 0.49, asks 0.51 and 0.52, at position (10, 5).
    fn synced() -> BookSync {
        let mut sync = BookSync::new(InstrumentId::from("AERO-USD-PERP.RISEX"), 5, 0);
        sync.on_snapshot(
            &[level("0.50", "100"), level("0.49", "200")],
            &[level("0.51", "300"), level("0.52", "400")],
            Some(seq(10, 5)),
            ts(1),
            ts(2),
        )
        .unwrap();
        sync
    }

    fn update(
        sync: &mut BookSync,
        bids: &[BookLevel],
        asks: &[BookLevel],
        at: BookSeq,
    ) -> Result<Option<BookOutput>, BookSyncError> {
        let checksum = checksum_after(sync, bids, asks);
        sync.on_update(bids, asks, checksum, at, ts(3), ts(4))
    }

    fn levels(value: &Value) -> Vec<BookLevel> {
        serde_json::from_value(value.clone()).unwrap()
    }

    // Acceptance test: every update checksum of the mainnet capture matches the local book
    #[rstest]
    #[case("15", 5, 0, 15)]
    #[case("1", 1, 6, 330)]
    fn fixture_replay_matches_every_checksum(
        #[case] market_id: &str,
        #[case] price_precision: u8,
        #[case] size_precision: u8,
        #[case] expected_updates: usize,
    ) {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/ws/public_aero_btc.jsonl"
        );
        let instrument_id = InstrumentId::from("TEST-USD-PERP.RISEX");
        let mut sync = BookSync::new(instrument_id, price_precision, size_precision);
        let mut nautilus_book = OrderBook::new(instrument_id, BookType::L2_MBP);
        let (mut snapshots, mut matched) = (0, 0);

        for line in std::fs::read_to_string(path).unwrap().lines() {
            let frame: Value = serde_json::from_str(line).unwrap();
            if frame["channel"] != "orderbook" || frame["market_id"] != market_id {
                continue;
            }
            let (bids, asks) = (
                levels(&frame["data"]["bids"]),
                levels(&frame["data"]["asks"]),
            );
            let at = seq(
                frame["block_number"].as_u64().unwrap(),
                frame["log_index"].as_u64().unwrap(),
            );
            let ts_event = ts(frame["worker_timestamp"].as_str().unwrap().parse().unwrap());

            let output = if frame["type"] == "snapshot" {
                snapshots += 1;
                sync.on_snapshot(&bids, &asks, Some(at), ts_event, ts_event)
                    .unwrap()
            } else {
                let checksum = u32::try_from(frame["checksum"].as_u64().unwrap()).unwrap();
                matched += 1;
                sync.on_update(&bids, &asks, checksum, at, ts_event, ts_event)
                    .unwrap_or_else(|e| panic!("update {matched} of market {market_id}: {e}"))
                    .expect("fixture updates always carry a level")
            };

            nautilus_book.apply_deltas(&output.deltas).unwrap();
            assert_eq!(output.deltas.sequence, at.as_u64());
        }

        assert_eq!(snapshots, 1);
        assert_eq!(matched, expected_updates);
        assert!(sync.is_synced());

        // The emitted deltas rebuild the same book inside Nautilus
        let book = sync.book();
        let price = |level: &BookLevel| Price::from_decimal_dp(level.price, price_precision).ok();
        assert_eq!(
            nautilus_book.best_bid_price(),
            book.best_bid().and_then(price)
        );
        assert_eq!(
            nautilus_book.best_ask_price(),
            book.best_ask().and_then(price)
        );
        assert_eq!(nautilus_book.bids(None).count(), book.bids().count());
        assert_eq!(nautilus_book.asks(None).count(), book.asks().count());
    }

    #[rstest]
    fn snapshot_emits_clear_then_adds_with_flags() {
        let mut sync = BookSync::new(InstrumentId::from("AERO-USD-PERP.RISEX"), 5, 0);
        // A zero level in a snapshot is not a level
        let output = sync
            .on_snapshot(
                &[level("0.49", "200"), level("0.50", "100")],
                &[level("0.51", "300"), level("0.60", "0")],
                Some(seq(10, 5)),
                ts(1),
                ts(2),
            )
            .unwrap();

        let deltas = &output.deltas.deltas;
        let actions: Vec<_> = deltas.iter().map(|d| d.action).collect();
        assert_eq!(
            actions,
            [
                BookAction::Clear,
                BookAction::Add,
                BookAction::Add,
                BookAction::Add
            ]
        );
        let flags: Vec<_> = deltas.iter().map(|d| d.flags).collect();
        assert_eq!(
            flags,
            [F_SNAPSHOT, F_SNAPSHOT, F_SNAPSHOT, F_SNAPSHOT | F_LAST]
        );
        assert_eq!(deltas[1].order.price, Price::from("0.50000"));
        assert_eq!(deltas[1].order.side, Some(OrderSide::Buy));
        assert_eq!(deltas[3].order.side, Some(OrderSide::Sell));
        assert_eq!(output.deltas.sequence, seq(10, 5).as_u64());
        assert_eq!(output.deltas.ts_event, ts(1));
        assert_eq!(output.deltas.ts_init, ts(2));
    }

    #[rstest]
    fn empty_snapshot_is_a_single_clear_marked_last() {
        let mut sync = BookSync::new(InstrumentId::from("AERO-USD-PERP.RISEX"), 5, 0);
        let output = sync.on_snapshot(&[], &[], None, ts(1), ts(2)).unwrap();

        assert_eq!(output.deltas.deltas.len(), 1);
        assert_eq!(output.deltas.deltas[0].flags, F_SNAPSHOT | F_LAST);
        assert!(output.quote.is_none());
        assert!(output.depth.bids.is_empty());
    }

    #[rstest]
    fn update_emits_update_and_delete_with_last_flag() {
        let mut sync = synced();
        let output = update(
            &mut sync,
            &[level("0.49", "0")],
            &[level("0.53", "50")],
            seq(10, 6),
        )
        .unwrap()
        .unwrap();

        let deltas = &output.deltas.deltas;
        assert_eq!(deltas.len(), 2);
        assert_eq!((deltas[0].action, deltas[0].flags), (BookAction::Delete, 0));
        assert_eq!(
            (deltas[1].action, deltas[1].flags),
            (BookAction::Update, F_LAST)
        );
        assert_eq!(deltas[1].order.size, Quantity::from("50"));
    }

    #[rstest]
    fn update_before_snapshot_is_dropped() {
        let mut sync = BookSync::new(InstrumentId::from("AERO-USD-PERP.RISEX"), 5, 0);
        let result = sync.on_update(&[level("0.50", "1")], &[], 0, seq(1, 1), ts(1), ts(1));

        assert_eq!(result.unwrap().map(|o| o.deltas.deltas.len()), None);
        assert!(!sync.is_synced());
        assert_eq!(sync.book().bids().count(), 0);
    }

    #[rstest]
    fn corrupted_level_stops_the_market_until_the_next_snapshot() {
        let mut sync = synced();
        let good = checksum_after(&sync, &[level("0.50", "150")], &[]);

        // The venue checksummed quantity 150; the frame we apply says 151
        let error = sync
            .on_update(&[level("0.50", "151")], &[], good, seq(10, 6), ts(3), ts(4))
            .unwrap_err();
        assert!(
            matches!(error, BookSyncError::ChecksumMismatch { expected, .. } if expected == good)
        );
        assert!(!sync.is_synced());

        // No deltas and no second error while the market waits for its snapshot
        let next = sync.on_update(&[level("0.50", "150")], &[], good, seq(10, 7), ts(3), ts(4));
        assert!(next.unwrap().is_none());

        // The recovery policy answers the error with a resubscribe
        let mut recovery = crate::book::recovery::BookRecovery::new(
            crate::book::recovery::RecoveryConfig::default(),
        );
        assert_eq!(
            recovery.on_desync(ts(5)),
            crate::book::recovery::RecoveryAction::Resubscribe
        );

        sync.on_snapshot(&[level("0.50", "150")], &[], Some(seq(11, 0)), ts(6), ts(6))
            .unwrap();
        assert!(sync.is_synced());
    }

    #[rstest]
    fn backward_sequence_stops_the_market() {
        let mut sync = synced();
        update(&mut sync, &[level("0.50", "110")], &[], seq(12, 3)).unwrap();

        // Same position again is not backward
        assert!(update(&mut sync, &[level("0.50", "110")], &[], seq(12, 3)).is_ok());

        for (at, last) in [(seq(12, 2), seq(12, 3)), (seq(11, 9), seq(12, 3))] {
            let mut sync = BookSync {
                book: sync.book().clone(),
                ..synced()
            };
            sync.last_seq = Some(last);
            let error = update(&mut sync, &[level("0.50", "120")], &[], at).unwrap_err();
            assert_eq!(
                error,
                BookSyncError::SequenceBackward { last, received: at }
            );
            assert!(!sync.is_synced());
            // The rejected update did not touch the book
            assert_eq!(sync.book().best_bid(), Some(&level("0.50", "110")));
        }
    }

    #[rstest]
    fn delete_of_a_missing_level_keeps_the_book_and_stays_synced() {
        let mut sync = synced();
        let before = sync.book().checksum();

        let output = update(&mut sync, &[level("0.40", "0")], &[], seq(10, 6))
            .unwrap()
            .unwrap();

        assert_eq!(output.deltas.deltas[0].action, BookAction::Delete);
        assert_eq!(sync.book().checksum(), before);
        assert!(sync.is_synced());
        assert!(output.quote.is_none());
    }

    #[rstest]
    fn snapshot_applies_in_any_state() {
        // Before the first snapshot (also the "snapshot before the subscribe ack" case)
        let mut sync = synced();
        assert!(sync.is_synced());

        // While synced: replaces the book and may move the position back
        sync.on_snapshot(&[level("0.30", "1")], &[], Some(seq(9, 0)), ts(1), ts(1))
            .unwrap();
        assert_eq!(sync.book().best_bid(), Some(&level("0.30", "1")));
        assert!(sync.book().best_ask().is_none());

        // After an error
        sync.on_update(&[], &[], 1, seq(9, 1), ts(1), ts(1))
            .unwrap_err();
        assert!(!sync.is_synced());
        sync.on_snapshot(&[level("0.31", "2")], &[], None, ts(1), ts(1))
            .unwrap();
        assert!(sync.is_synced());

        // After a REST snapshot any position is accepted
        assert!(update(&mut sync, &[level("0.31", "3")], &[], seq(1, 0)).is_ok());
    }

    #[rstest]
    fn quote_is_emitted_only_when_the_top_of_book_changes() {
        let mut sync = BookSync::new(InstrumentId::from("AERO-USD-PERP.RISEX"), 5, 0);
        let first = sync
            .on_snapshot(
                &[level("0.50", "100"), level("0.49", "200")],
                &[level("0.51", "300")],
                Some(seq(10, 5)),
                ts(1),
                ts(2),
            )
            .unwrap();
        let quote = first.quote.unwrap();
        assert_eq!(quote.bid_price, Price::from("0.50000"));
        assert_eq!(quote.ask_price, Price::from("0.51000"));
        assert_eq!(quote.bid_size, Quantity::from("100"));
        assert_eq!(quote.ask_size, Quantity::from("300"));

        let mut quote_after = |bids: &[BookLevel], asks: &[BookLevel], log_index| {
            update(&mut sync, bids, asks, seq(10, log_index))
                .unwrap()
                .unwrap()
                .quote
        };

        // A deeper level changes: no quote
        assert!(quote_after(&[level("0.49", "250")], &[], 6).is_none());
        // Top size changes
        let quote = quote_after(&[level("0.50", "90")], &[], 7).unwrap();
        assert_eq!(quote.bid_size, Quantity::from("90"));
        assert_eq!((quote.ts_event, quote.ts_init), (ts(3), ts(4)));
        // Top price changes
        let quote = quote_after(&[], &[level("0.505", "10")], 8).unwrap();
        assert_eq!(quote.ask_price, Price::from("0.50500"));
        // One side empties: no quote; the same top comes back: still no change to report
        assert!(quote_after(&[], &[level("0.505", "0"), level("0.51", "0")], 9).is_none());
        assert!(quote_after(&[], &[level("0.505", "10")], 10).is_none());
    }

    #[rstest]
    fn depth_holds_at_most_ten_levels_and_is_not_padded() {
        let mut sync = BookSync::new(InstrumentId::from("AERO-USD-PERP.RISEX"), 5, 0);
        let bids: Vec<_> = (1..=12)
            .map(|i| BookLevel {
                order_count: i,
                ..level(&format!("0.{i:02}"), "5")
            })
            .collect();
        let depth = sync
            .on_snapshot(&bids, &[level("0.50", "7")], None, ts(1), ts(2))
            .unwrap()
            .depth;

        assert_eq!(depth.bids.len(), 10);
        assert_eq!(depth.bids[0].price, Price::from("0.12000"));
        assert_eq!(depth.bids[9].price, Price::from("0.03000"));
        assert_eq!(depth.bid_counts[..2], [12, 11]);
        assert_eq!(depth.asks.len(), 1);
        assert_eq!(depth.asks[0].size, Quantity::from("7"));
        assert_eq!(depth.ask_counts[..], [1]);
        assert_eq!(depth.flags, F_SNAPSHOT);
    }

    #[rstest]
    fn level_that_does_not_convert_stops_the_market() {
        let mut sync = synced();
        let error = update(&mut sync, &[level("0.50", "-1")], &[], seq(10, 6)).unwrap_err();

        assert!(matches!(error, BookSyncError::InvalidLevel(_)));
        assert!(!sync.is_synced());
    }

    #[rstest]
    fn sequence_number_keeps_the_venue_order() {
        assert!(seq(1, 999_999).as_u64() < seq(2, 0).as_u64());
        assert!(seq(2, 0).as_u64() < seq(2, 1).as_u64());
        assert!(seq(2, u64::MAX).as_u64() < seq(3, 0).as_u64());
    }
}
