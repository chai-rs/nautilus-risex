//! Action hashes never panic, and equal inputs give equal hashes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nautilus_risex::signing::action::{
    PlaceOrder, cancel_all_orders_hash, cancel_order_hash, update_leverage_hash,
    update_margin_mode_hash,
};

type Fields = (u16, u32, u32, u8, bool, bool, u8, u8, u8, u16, u64, u16);

fuzz_target!(|input: (Fields, u64, u8)| {
    let (f, id, small) = input;
    let order = PlaceOrder {
        market_id: f.0,
        size_steps: f.1,
        price_ticks: f.2,
        side: f.3,
        post_only: f.4,
        reduce_only: f.5,
        stp_mode: f.6,
        order_type: f.7,
        time_in_force: f.8,
        builder_id: f.9,
        client_order_id: f.10,
        ttl_units: f.11,
    };
    let market = order.market_id;

    assert_eq!(order.action_hash().ok(), order.clone().action_hash().ok());
    assert_eq!(cancel_order_hash(market, id), cancel_order_hash(market, id));
    assert_eq!(
        cancel_all_orders_hash(market),
        cancel_all_orders_hash(market)
    );
    assert_eq!(
        update_leverage_hash(market, small),
        update_leverage_hash(market, small)
    );
    assert_eq!(
        update_margin_mode_hash(market, small),
        update_margin_mode_hash(market, small)
    );
});
