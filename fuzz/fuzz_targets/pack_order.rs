//! `PlaceOrder::order_data` never panics, and a packed word unpacks to the fields it was built from.
#![no_main]

use libfuzzer_sys::fuzz_target;
use nautilus_risex::signing::action::PlaceOrder;

fn check(order: &PlaceOrder) {
    let Ok(word) = order.order_data() else {
        return;
    };
    let bits = |shift: u32, width: u32| (word >> shift) & ((1 << width) - 1);
    assert_eq!(bits(0, 6), 0b10, "version 1 in bit 1, nothing else");
    assert_eq!(bits(6, 1), u128::from(order.side));
    assert_eq!(bits(7, 1), u128::from(order.post_only));
    assert_eq!(bits(8, 1), u128::from(order.reduce_only));
    assert_eq!(bits(9, 2), u128::from(order.stp_mode));
    assert_eq!(bits(11, 1), u128::from(order.order_type));
    assert_eq!(bits(12, 2), u128::from(order.time_in_force));
    assert_eq!(bits(14, 24), u128::from(order.price_ticks));
    assert_eq!(bits(38, 32), u128::from(order.size_steps));
    assert_eq!(bits(70, 16), u128::from(order.market_id));
    assert_eq!(word >> 86, 0, "fits the uint88");
}

type Fields = (u16, u32, u32, u8, bool, bool, u8, u8, u8, u16, u64, u16);

fuzz_target!(|f: Fields| {
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
    check(&order);
    // Raw fields are almost never all in range; fold them so the unpack asserts run too.
    check(&PlaceOrder {
        price_ticks: order.price_ticks >> 8,
        side: order.side % 2,
        stp_mode: order.stp_mode % 3,
        order_type: order.order_type % 2,
        time_in_force: order.time_in_force % 4,
        ..order
    });
});
