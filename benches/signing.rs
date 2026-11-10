//! Signing: order packing, action hashes and one full permit signature.

use std::hint::black_box;

use alloy_primitives::{Address, address};
use criterion::{Criterion, criterion_group, criterion_main};
use nautilus_risex::signing::{
    RisexSigner,
    action::{PlaceOrder, cancel_order_hash},
    nonce::Nonce,
};

/// First Anvil/Hardhat key: public test key, never funded on a real network.
const KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const ACCOUNT: Address = address!("0x70997970C51812dc3A010C7d01b50e0d17dc79C8");
const AUTH: Address = address!("0x0D919DAA3f12AE715744Eb648c00066c5DBd66f0");
const ROUTER: Address = address!("0xaadde0cea454f2bcb26f46ed54c5709b7bb34a7e");

fn bench(c: &mut Criterion) {
    let order = PlaceOrder {
        market_id: 15,
        size_steps: 25,
        price_ticks: 31_234,
        side: 0,
        post_only: true,
        reduce_only: false,
        stp_mode: 0,
        order_type: 1,
        time_in_force: 0,
        builder_id: 0,
        client_order_id: 42,
        ttl_units: 0,
    };
    let signer = RisexSigner::new(KEY.parse().unwrap(), "RISEx", "1", 4153, AUTH);
    let hash = order.action_hash().unwrap();
    let nonce = Nonce {
        anchor: 2_527_069,
        index: 7,
    };

    let mut group = c.benchmark_group("signing");
    group.bench_function("order_data", |b| {
        b.iter(|| black_box(&order).order_data().unwrap());
    });
    group.bench_function("action_hash", |b| {
        b.iter(|| black_box(&order).action_hash().unwrap());
    });
    group.bench_function("cancel_order_hash", |b| {
        b.iter(|| cancel_order_hash(black_box(15), black_box(123_456_789)));
    });
    group.bench_function("permit", |b| {
        b.iter(|| {
            signer
                .permit(ACCOUNT, ROUTER, black_box(hash), nonce, 1_786_097_000)
                .unwrap()
        });
    });
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
