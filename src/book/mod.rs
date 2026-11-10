//! Local order book per market and the venue's CRC32 book checksum.
//!
//! - [`LocalBook`] keeps the whole book, because the venue checksums the full book after each
//!   update, not the levels in the message.
//! - [`sync`] drives one book from WebSocket frames and emits Nautilus data.
//! - [`recovery`] decides what to do when a book loses sync.

pub mod recovery;
pub mod sync;

use std::{collections::BTreeMap, fmt::Write};

use rust_decimal::Decimal;
use serde::Deserialize;

/// One price level as the venue sends it, in snapshots and updates alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct BookLevel {
    pub price: Decimal,
    /// Total quantity at the price; zero deletes the level.
    pub quantity: Decimal,
    #[serde(default)]
    pub order_count: u32,
}

/// Full order book of one market, both sides keyed by price.
#[derive(Debug, Clone, Default)]
pub struct LocalBook {
    bids: BTreeMap<Decimal, BookLevel>,
    asks: BTreeMap<Decimal, BookLevel>,
}

impl LocalBook {
    /// Replaces the whole book.
    pub fn apply_snapshot(&mut self, bids: &[BookLevel], asks: &[BookLevel]) {
        self.bids.clear();
        self.asks.clear();
        self.apply_update(bids, asks);
    }

    /// Sets each level to its new quantity; a zero quantity removes the level, and removing a
    /// level that is not there does nothing.
    pub fn apply_update(&mut self, bids: &[BookLevel], asks: &[BookLevel]) {
        for (side, levels) in [(&mut self.bids, bids), (&mut self.asks, asks)] {
            for level in levels {
                if level.quantity.is_zero() {
                    side.remove(&level.price);
                } else {
                    side.insert(level.price, *level);
                }
            }
        }
    }

    /// Bids from the highest price down; `.take(n)` gives the top `n` levels.
    pub fn bids(&self) -> impl Iterator<Item = &BookLevel> {
        self.bids.values().rev()
    }

    /// Asks from the lowest price up; `.take(n)` gives the top `n` levels.
    pub fn asks(&self) -> impl Iterator<Item = &BookLevel> {
        self.asks.values()
    }

    #[must_use]
    pub fn best_bid(&self) -> Option<&BookLevel> {
        self.bids.values().next_back()
    }

    #[must_use]
    pub fn best_ask(&self) -> Option<&BookLevel> {
        self.asks.values().next()
    }

    /// CRC32-IEEE of the whole book in the venue's format.
    ///
    /// Levels interleave as `bids[0], asks[0], bids[1], asks[1], …` with no depth limit; when one
    /// side runs out, the other side continues alone. Each level gives `price` then `quantity`,
    /// each as the integer `value * 10^18` in base 10, and all values join with `:`.
    #[must_use]
    pub fn checksum(&self) -> u32 {
        let mut text = String::with_capacity((self.bids.len() + self.asks.len()) * 48);
        let (mut bids, mut asks) = (self.bids(), self.asks());

        loop {
            let pair = [bids.next(), asks.next()];
            if pair == [None, None] {
                break;
            }

            for level in pair.into_iter().flatten() {
                for value in [level.price, level.quantity] {
                    write_wei(&mut text, value);
                    text.push(':');
                }
            }
        }
        text.pop();

        crc32fast::hash(text.as_bytes())
    }
}

/// Appends `value * 10^18` as an integer, truncated toward zero like Python's `int()`.
fn write_wei(out: &mut String, value: Decimal) {
    let (mantissa, scale) = (value.mantissa(), value.scale());

    // Writing digits and zeros avoids the overflow of a real multiplication by 10^18
    if scale > 18 {
        let _ = write!(out, "{}", mantissa / 10_i128.pow(scale - 18));
    } else if mantissa == 0 {
        out.push('0');
    } else {
        let _ = write!(out, "{mantissa}");
        out.extend(std::iter::repeat_n('0', (18 - scale) as usize));
    }
}

#[cfg(test)]
pub(crate) fn level(price: &str, quantity: &str) -> BookLevel {
    BookLevel {
        price: price.parse().unwrap(),
        quantity: quantity.parse().unwrap(),
        order_count: 1,
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("0.55304", "553040000000000000")]
    #[case("12633", "12633000000000000000000")]
    #[case("74693.3", "74693300000000000000000")]
    #[case("0.000303", "303000000000000")]
    #[case("1.50", "1500000000000000000")]
    #[case("0", "0")]
    #[case("0.000", "0")]
    #[case("0.0000000000000000019", "1")]
    fn wei_is_the_18_decimal_integer(#[case] value: &str, #[case] expected: &str) {
        let mut out = String::new();
        write_wei(&mut out, value.parse().unwrap());
        assert_eq!(out, expected);
    }

    // Expected value from the Python reference in the venue docs
    #[rstest]
    fn checksum_interleaves_and_continues_past_the_shorter_side() {
        let mut book = LocalBook::default();
        book.apply_snapshot(
            &[level("0.55304", "12633")],
            &[level("0.5532", "7.5"), level("0.5531", "100")],
        );

        assert_eq!(book.checksum(), 2_618_543_896);
    }

    #[rstest]
    fn empty_book_hashes_the_empty_string() {
        assert_eq!(LocalBook::default().checksum(), 0);
    }

    #[rstest]
    fn zero_quantity_removes_and_missing_level_is_ignored() {
        let mut book = LocalBook::default();
        book.apply_snapshot(&[level("10", "1"), level("9", "2")], &[level("11", "3")]);
        let before = book.checksum();

        book.apply_update(&[level("8", "0")], &[level("12", "0")]);
        assert_eq!(book.checksum(), before);

        book.apply_update(&[level("10", "0")], &[]);
        assert_eq!(book.best_bid(), Some(&level("9", "2")));
        assert_eq!(book.best_ask(), Some(&level("11", "3")));
        assert_eq!(book.bids().count(), 1);
    }

    fn reference_checksum(book: &LocalBook) -> u32 {
        let wei = |value: Decimal| (value * Decimal::from(10_u64.pow(18))).trunc().to_string();
        let (bids, asks): (Vec<_>, Vec<_>) = (book.bids().collect(), book.asks().collect());
        let mut parts = Vec::new();
        for i in 0..bids.len().max(asks.len()) {
            for level in [bids.get(i), asks.get(i)].into_iter().flatten() {
                parts.push(wei(level.price));
                parts.push(wei(level.quantity));
            }
        }
        crc32fast::hash(parts.join(":").as_bytes())
    }

    proptest! {
        #[test]
        fn random_updates_keep_the_book_sorted_and_the_checksum_deterministic(
            ops in prop::collection::vec((any::<bool>(), 1_u32..40, 0_u32..4), 0..200),
        ) {
            let mut book = LocalBook::default();
            for (is_bid, ticks, quantity) in ops {
                // Scale 2 prices and scale 1 quantities exercise the zero padding
                let level = BookLevel {
                    price: Decimal::new(i64::from(ticks), 2),
                    quantity: Decimal::new(i64::from(quantity) * 5, 1),
                    order_count: 1,
                };
                if is_bid {
                    book.apply_update(&[level], &[]);
                } else {
                    book.apply_update(&[], &[level]);
                }

                let bids: Vec<_> = book.bids().copied().collect();
                let asks: Vec<_> = book.asks().copied().collect();
                prop_assert!(bids.windows(2).all(|w| w[0].price > w[1].price));
                prop_assert!(asks.windows(2).all(|w| w[0].price < w[1].price));
                prop_assert!(bids.iter().chain(&asks).all(|l| !l.quantity.is_zero()));
                prop_assert_eq!(book.best_bid(), bids.first());
                prop_assert_eq!(book.best_ask(), asks.first());

                // Same levels in another insertion order give the same checksum
                let mut rebuilt = LocalBook::default();
                let reversed = |levels: &[BookLevel]| levels.iter().rev().copied().collect::<Vec<_>>();
                rebuilt.apply_snapshot(&reversed(&bids), &reversed(&asks));
                prop_assert_eq!(book.checksum(), rebuilt.checksum());
                prop_assert_eq!(book.checksum(), reference_checksum(&book));
            }
        }
    }
}
