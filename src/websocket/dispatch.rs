//! Routes `orders` and `fills` channel rows to order events or reports.
//!
//! Orders submitted through this client are tracked by the venue `u64` client
//! order id and get typed events. Everything else on the account becomes a
//! report for the execution engine to reconcile.

use std::collections::{HashMap, HashSet, VecDeque};

use nautilus_core::UnixNanos;
use nautilus_live::ExecutionEventEmitter;
use nautilus_model::{
    enums::LiquiditySide,
    identifiers::{AccountId, ClientOrderId, TradeId, VenueOrderId},
    instruments::{Instrument, InstrumentAny},
    orders::{Order, OrderAny},
    reports::FillReport,
    types::Money,
};
use parking_lot::Mutex;
use rust_decimal::Decimal;

use super::messages::{RisexWsData, RisexWsFill, RisexWsOrder};
use crate::{
    common::{
        enums::RisexOrderStatus,
        parse::{
            client_order_id_to_u64, parse_decimal, price_from_decimal, quantity_from_decimal,
            trade_id_from_venue,
        },
    },
    http::{models::RisexOrder, parse::parse_order_status_report},
};

const MAX_TRACKED_ORDERS: usize = 4096;
const MAX_SEEN_TRADES: usize = 10_000;

#[derive(Debug)]
struct Tracked {
    order: OrderAny,
    venue_order_id: Option<VenueOrderId>,
    accepted: bool,
    closed: bool,
    /// Venue ids this order had before a modify; their cancel rows are not the order's end.
    replaced: Vec<VenueOrderId>,
}

#[derive(Debug, Default)]
struct State {
    orders: HashMap<u64, Tracked>,
    by_venue: HashMap<VenueOrderId, u64>,
    seen: HashSet<TradeId>,
    seen_queue: VecDeque<TradeId>,
}

impl State {
    fn mark_seen(&mut self, trade_id: TradeId) -> bool {
        if !self.seen.insert(trade_id) {
            return false;
        }
        self.seen_queue.push_back(trade_id);
        if self.seen_queue.len() > MAX_SEEN_TRADES
            && let Some(oldest) = self.seen_queue.pop_front()
        {
            self.seen.remove(&oldest);
        }
        true
    }

    fn key_of(&self, client_order_id: Option<&str>, venue_order_id: &VenueOrderId) -> Option<u64> {
        client_order_id
            .and_then(|id| id.parse::<u64>().ok())
            .filter(|key| self.orders.contains_key(key))
            .or_else(|| self.by_venue.get(venue_order_id).copied())
    }
}

/// Order and fill dispatch of one account.
#[derive(Debug)]
pub struct OrderDispatch {
    account: String,
    account_id: AccountId,
    emitter: ExecutionEventEmitter,
    instruments: HashMap<u16, InstrumentAny>,
    state: Mutex<State>,
}

impl OrderDispatch {
    /// `instruments` is keyed by venue market id.
    #[must_use]
    pub fn new(
        account: &str,
        account_id: AccountId,
        emitter: ExecutionEventEmitter,
        instruments: HashMap<u16, InstrumentAny>,
    ) -> Self {
        Self {
            account: account.to_ascii_lowercase(),
            account_id,
            emitter,
            instruments,
            state: Mutex::new(State::default()),
        }
    }

    /// Starts tracking `order` and returns its venue client order id. Idempotent.
    pub fn track(&self, order: &OrderAny) -> u64 {
        let key = client_order_id_to_u64(order.client_order_id().as_str());
        let mut guard = self.state.lock();
        let State {
            orders, by_venue, ..
        } = &mut *guard;
        // shortcut: closed orders stay tracked until the map is full, then all of them go;
        // a fill that arrives later still reaches the engine, as a FillReport.
        if orders.len() >= MAX_TRACKED_ORDERS {
            orders.retain(|_, tracked| !tracked.closed);
            by_venue.retain(|_, key| orders.contains_key(key));
        }
        let venue_order_id = order.venue_order_id();
        if let Some(venue_order_id) = venue_order_id {
            by_venue.insert(venue_order_id, key);
        }
        orders.entry(key).or_insert_with(|| Tracked {
            order: order.clone(),
            venue_order_id,
            accepted: venue_order_id.is_some(),
            closed: false,
            replaced: Vec::new(),
        });
        key
    }

    /// Emits `OrderAccepted` once; later calls for the same order do nothing.
    pub fn accepted(&self, key: u64, venue_order_id: VenueOrderId, ts_event: UnixNanos) {
        let mut guard = self.state.lock();
        let State {
            orders, by_venue, ..
        } = &mut *guard;
        if let Some(tracked) = orders.get_mut(&key)
            && !tracked.accepted
            && !tracked.closed
        {
            tracked.accepted = true;
            tracked.venue_order_id = Some(venue_order_id);
            by_venue.insert(venue_order_id, key);
            self.emitter
                .emit_order_accepted(&tracked.order, venue_order_id, ts_event);
        }
    }

    /// Emits `OrderRejected` unless the venue already showed the order.
    pub fn rejected(&self, key: u64, reason: &str, ts_event: UnixNanos, due_post_only: bool) {
        let mut guard = self.state.lock();
        match guard.orders.get_mut(&key) {
            Some(tracked) if !tracked.accepted && !tracked.closed => {
                tracked.closed = true;
                self.emitter
                    .emit_order_rejected(&tracked.order, reason, ts_event, due_post_only);
            }
            _ => log::warn!("RISEx reject of order {key} ignored, already live: {reason}"),
        }
    }

    /// Marks the order as ended by the caller, who emits the event.
    pub fn close(&self, key: u64) {
        if let Some(tracked) = self.state.lock().orders.get_mut(&key) {
            tracked.closed = true;
        }
    }

    /// Before the cancel of a modify: the cancel row of the current venue id is expected.
    pub fn begin_modify(&self, key: u64) {
        if let Some(tracked) = self.state.lock().orders.get_mut(&key)
            && let Some(venue_order_id) = tracked.venue_order_id
        {
            tracked.replaced.push(venue_order_id);
        }
    }

    /// The cancel of a modify failed: the order is still the old one.
    pub fn abort_modify(&self, key: u64) {
        if let Some(tracked) = self.state.lock().orders.get_mut(&key) {
            tracked.replaced.pop();
        }
    }

    /// The new order of a modify is on the venue.
    pub fn set_venue_order_id(&self, key: u64, venue_order_id: VenueOrderId) {
        let mut guard = self.state.lock();
        let State {
            orders, by_venue, ..
        } = &mut *guard;
        if let Some(tracked) = orders.get_mut(&key) {
            tracked.venue_order_id = Some(venue_order_id);
            by_venue.insert(venue_order_id, key);
        }
    }

    #[must_use]
    pub fn venue_order_id(&self, key: u64) -> Option<VenueOrderId> {
        self.state.lock().orders.get(&key)?.venue_order_id
    }

    /// The Nautilus id behind a venue client order id, for orders tracked here.
    #[must_use]
    pub fn client_order_id(&self, key: u64) -> Option<ClientOrderId> {
        Some(self.state.lock().orders.get(&key)?.order.client_order_id())
    }

    /// Tracked orders that are live on the venue as far as this client knows.
    #[must_use]
    pub fn open_orders(&self) -> Vec<(u64, ClientOrderId, VenueOrderId)> {
        let guard = self.state.lock();
        guard
            .orders
            .iter()
            .filter(|(_, tracked)| !tracked.closed)
            .filter_map(|(key, tracked)| {
                Some((
                    *key,
                    tracked.order.client_order_id(),
                    tracked.venue_order_id?,
                ))
            })
            .collect()
    }

    /// Records a venue trade id; `false` when it was already handled.
    pub fn is_new_trade(&self, trade_id: TradeId) -> bool {
        self.state.lock().mark_seen(trade_id)
    }

    /// Handles one `orders` frame, snapshot or update.
    pub fn handle_orders(&self, msg: &RisexWsData<Vec<RisexWsOrder>>, ts_init: UnixNanos) {
        let mut guard = self.state.lock();
        for row in &msg.data {
            if !row.sender.eq_ignore_ascii_case(&self.account) {
                continue;
            }
            let venue_order_id = VenueOrderId::new(&row.id);
            match guard.key_of(Some(&row.client_order_id), &venue_order_id) {
                Some(key) => self.on_tracked(&mut guard, key, venue_order_id, row, ts_init),
                None => self.report_untracked(row, ts_init),
            }
        }
    }

    fn on_tracked(
        &self,
        state: &mut State,
        key: u64,
        venue_order_id: VenueOrderId,
        row: &RisexWsOrder,
        ts_event: UnixNanos,
    ) {
        let State {
            orders, by_venue, ..
        } = state;
        let Some(tracked) = orders.get_mut(&key) else {
            return;
        };
        if tracked.closed || tracked.replaced.contains(&venue_order_id) {
            return;
        }
        let Some(status) = row.status.filter(|s| *s != RisexOrderStatus::None) else {
            return;
        };
        // The row carries the venue's own id: it wins over the one derived from the
        // HTTP response, and it is the new id while a modify is in flight.
        tracked.venue_order_id = Some(venue_order_id);
        by_venue.insert(venue_order_id, key);

        let reason = row.cancel_reason.to_ascii_lowercase();
        // unverified: the cancel reason of a post-only order that would cross is not documented.
        if status == RisexOrderStatus::Cancelled
            && !tracked.accepted
            && tracked.order.is_post_only()
            && reason.contains("post")
        {
            tracked.closed = true;
            self.emitter
                .emit_order_rejected(&tracked.order, &row.cancel_reason, ts_event, true);
            return;
        }
        // Also covers an IOC order that ended before the HTTP response came back.
        if !tracked.accepted {
            tracked.accepted = true;
            self.emitter
                .emit_order_accepted(&tracked.order, venue_order_id, ts_event);
        }
        match status {
            // Fills come from the `fills` channel only.
            RisexOrderStatus::Filled => tracked.closed = true,
            RisexOrderStatus::Cancelled => {
                tracked.closed = true;
                if reason.contains("expir") {
                    self.emitter
                        .emit_order_expired(&tracked.order, Some(venue_order_id), ts_event);
                } else {
                    self.emitter.emit_order_canceled(
                        &tracked.order,
                        Some(venue_order_id),
                        ts_event,
                    );
                }
            }
            _ => {}
        }
    }

    fn report_untracked(&self, row: &RisexWsOrder, ts_init: UnixNanos) {
        let report = rest_order(row).and_then(|order| {
            let instrument = self
                .instruments
                .get(&order.market_id)
                .ok_or_else(|| anyhow::anyhow!("unknown market {}", order.market_id))?;
            parse_order_status_report(&order, self.account_id, instrument, ts_init)
        });
        match report {
            Ok(report) => self.emitter.send_order_status_report(report),
            Err(e) => log::warn!("RISEx orders row {} skipped: {e}", row.id),
        }
    }

    /// Handles one `fills` frame. A trade id seen before is dropped.
    pub fn handle_fill(&self, fill: &RisexWsFill, ts_init: UnixNanos) {
        if let Err(e) = self.try_fill(fill, ts_init) {
            log::error!("RISEx fill {} not processed: {e}", fill.id);
        }
    }

    fn try_fill(&self, fill: &RisexWsFill, ts_init: UnixNanos) -> anyhow::Result<()> {
        let market_id: u16 = fill.market_id.parse()?;
        let instrument = self
            .instruments
            .get(&market_id)
            .ok_or_else(|| anyhow::anyhow!("unknown market {market_id}"))?;
        let side = fill.side.ok_or_else(|| anyhow::anyhow!("no side"))?;
        let last_qty =
            quantity_from_decimal(parse_decimal(&fill.size)?, instrument.size_precision())?;
        let last_px =
            price_from_decimal(parse_decimal(&fill.price)?, instrument.price_precision())?;
        // Signed: a negative fee is a maker rebate.
        let commission =
            Money::from_decimal(parse_decimal(&fill.fee)?, instrument.quote_currency())
                .map_err(|e| anyhow::anyhow!("invalid fee: {e}"))?;
        let liquidity_side = fill
            .liquidity_indicator
            .map_or(LiquiditySide::NoLiquiditySide, Into::into);
        let ts_event = fill.time.parse::<u64>().map_or(ts_init, UnixNanos::from);
        let trade_id = trade_id_from_venue(&fill.id);
        let venue_order_id = VenueOrderId::new(&fill.order_id);

        let mut guard = self.state.lock();
        // Marked only after the row parsed, so a bad frame can still be recovered over REST.
        if !guard.mark_seen(trade_id) {
            return Ok(());
        }
        let key = guard.key_of(fill.client_order_id.as_deref(), &venue_order_id);
        let Some(tracked) = key.and_then(|key| guard.orders.get_mut(&key)) else {
            let report = FillReport::new(
                self.account_id,
                instrument.id(),
                venue_order_id,
                trade_id,
                side.into(),
                last_qty,
                last_px,
                commission,
                liquidity_side,
                None,
                None,
                ts_event,
                ts_init,
                None,
            );
            self.emitter.send_fill_report(report);
            return Ok(());
        };
        if !tracked.accepted {
            tracked.accepted = true;
            tracked.venue_order_id = Some(venue_order_id);
            self.emitter
                .emit_order_accepted(&tracked.order, venue_order_id, ts_event);
        }
        self.emitter.emit_order_filled(
            &tracked.order,
            venue_order_id,
            None,
            trade_id,
            last_qty,
            last_px,
            instrument.quote_currency(),
            Some(commission),
            liquidity_side,
            ts_event,
        );
        Ok(())
    }
}

fn decimal(value: &str) -> anyhow::Result<Decimal> {
    if value.is_empty() {
        Ok(Decimal::ZERO)
    } else {
        parse_decimal(value)
    }
}

/// An `orders` channel row in the shape of `GET /v1/orders`, so one parser serves both.
fn rest_order(row: &RisexWsOrder) -> anyhow::Result<RisexOrder> {
    let missing = |field: &str| anyhow::anyhow!("no {field}");
    let int = |value: &str| value.parse::<u64>().unwrap_or(0);
    Ok(RisexOrder {
        id: row.id.clone(),
        price: decimal(&row.price)?,
        size: decimal(&row.size)?,
        market_id: row.market_id.parse()?,
        side: row.side.ok_or_else(|| missing("side"))?,
        order_type: row.order_type.ok_or_else(|| missing("type"))?,
        time_in_force: row.time_in_force.ok_or_else(|| missing("time in force"))?,
        expiry: int(&row.expiry),
        post_only: row.post_only,
        reduce_only: row.reduce_only,
        cancel_reason: row.cancel_reason.clone(),
        block_number: int(&row.block_number),
        log_index: int(&row.log_index),
        stp_mode: row.stp_mode,
        filled_size: decimal(&row.filled_size)?,
        status: row.status.ok_or_else(|| missing("status"))?,
        sender: row.sender.clone(),
        avg_price: decimal(&row.avg_price)?,
        cancel_requested: row.cancel_requested,
        created_at: int(&row.created_at),
        tx_hash: row.tx_hash.clone(),
        client_order_id: int(&row.client_order_id),
        // Plain orders carry a default stop type and an empty stop price on this channel.
        stop_type: None,
        stop_price: None,
        stop_price_option: None,
        is_liquidation: row.is_liquidation,
        wide_order_id: int(&row.wide_order_id),
        resting_order_id: int(&row.resting_order_id),
    })
}
