//! Hot-path micros: local book apply, CRC32 book checksum, decimal and wei parsing.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use nautilus_risex::{
    book::{BookLevel, LocalBook},
    common::parse::{parse_decimal, parse_wei},
    websocket::{RisexWsMessage, parse::parse_book_levels},
};

const FRAMES: &str = include_str!("../tests/fixtures/ws/public_aero_btc.jsonl");

/// Both sides of the orderbook frame on `line` of the fixture.
fn levels(line: usize) -> (Vec<BookLevel>, Vec<BookLevel>) {
    let frame = serde_json::from_str(FRAMES.lines().nth(line).unwrap()).unwrap();
    let RisexWsMessage::Orderbook(frame) = frame else {
        panic!("line {line} is not an orderbook frame");
    };
    (
        parse_book_levels(&frame.data.bids).unwrap(),
        parse_book_levels(&frame.data.asks).unwrap(),
    )
}

fn bench(c: &mut Criterion) {
    // BTC: the 517-level snapshot (line 0) and a one-level update (line 4)
    let (bids, asks) = levels(0);
    let (update_bids, update_asks) = levels(4);
    let mut book = LocalBook::default();
    book.apply_snapshot(&bids, &asks);

    let mut group = c.benchmark_group("book");
    group.bench_function("apply_snapshot_517", |b| {
        b.iter(|| book.apply_snapshot(black_box(&bids), black_box(&asks)));
    });
    // The update is a delete: after the first pass it removes a level that is already gone.
    group.bench_function("apply_update", |b| {
        b.iter(|| book.apply_update(black_box(&update_bids), black_box(&update_asks)));
    });
    group.bench_function("checksum_517", |b| {
        b.iter(|| black_box(&book).checksum());
    });
    group.finish();

    let mut group = c.benchmark_group("parse");
    group.bench_function("decimal", |b| {
        b.iter(|| parse_decimal(black_box("74693.3")).unwrap());
    });
    group.bench_function("wei", |b| {
        b.iter(|| parse_wei(black_box("74693300000000000000000")).unwrap());
    });
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
