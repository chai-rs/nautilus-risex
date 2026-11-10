//! Arbitrary level updates into the local book: apply and checksum never panic.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nautilus_risex::book::{BookLevel, LocalBook};
use rust_decimal::Decimal;

/// `lo, mid, hi, negative, scale` of a `Decimal`.
type Parts = (u32, u32, u32, bool, u8);

fn decimal((lo, mid, hi, negative, scale): Parts) -> Decimal {
    // `from_parts` panics above scale 28
    Decimal::from_parts(lo, mid, hi, negative, u32::from(scale) % 29)
}

// Each entry: snapshot instead of update, bid instead of ask, price, quantity.
fuzz_target!(|updates: Vec<(bool, bool, Parts, Parts)>| {
    let mut book = LocalBook::default();
    for (snapshot, bid, price, quantity) in updates {
        let level = [BookLevel {
            price: decimal(price),
            quantity: decimal(quantity),
            order_count: 1,
        }];
        let (bids, asks): (&[BookLevel], &[BookLevel]) =
            if bid { (&level, &[]) } else { (&[], &level) };
        if snapshot {
            book.apply_snapshot(bids, asks);
        } else {
            book.apply_update(bids, asks);
        }
        let _ = book.checksum();
    }
});
