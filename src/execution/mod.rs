//! Live execution client.
//!
//! Commands are signed and sent over REST; order and fill events come from the
//! `orders` and `fills` channels, position reports from the `positions` channel.
//! Trigger orders (TP/SL) have no channel and are polled.

pub mod order;
pub mod revoke;
pub mod session;

use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

use anyhow::Context;
use async_trait::async_trait;
use nautilus_common::{
    clients::ExecutionClient,
    live::{get_runtime, runner::get_exec_event_sender},
    messages::execution::{
        BatchCancelOrders, CancelAllOrders, CancelOrder, GenerateFillReports,
        GenerateOrderStatusReport, GenerateOrderStatusReports, GeneratePositionStatusReports,
        ModifyOrder, QueryAccount, QueryOrder, SubmitOrder, SubmitOrderList,
    },
};
use nautilus_core::{UnixNanos, params::Params, time::get_atomic_clock_realtime};
use nautilus_live::{ExecutionClientCore, ExecutionEventEmitter};
use nautilus_model::{
    accounts::AccountAny,
    enums::{OmsType, OrderType},
    identifiers::{
        AccountId, ClientId, ClientOrderId, InstrumentId, StrategyId, Venue, VenueOrderId,
    },
    instruments::{Instrument, InstrumentAny},
    orders::{Order, OrderAny},
    reports::{FillReport, OrderStatusReport, PositionStatusReport},
    types::{AccountBalance, Currency, MarginBalance, Price, Quantity},
};
use rust_decimal::Decimal;
use tokio::{sync::mpsc::UnboundedReceiver, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use self::session::RisexSession;
use crate::{
    common::{
        consts::MAX_PAGE_LIMIT,
        credential::RisexCredential,
        enums::{RisexMarginMode, RisexOrderStatus, RisexTpslOrderStatus},
        parse::{RisexOrderId, client_order_id_to_u64},
        rate_limit::resolve_quota,
        urls::risex_ws_url,
    },
    config::exec::RisexExecClientConfig,
    http::{
        client::{RisexHttpClient, RisexRawHttpClient},
        error::RisexHttpError,
        models::RisexTpslOrder,
        parse::{
            parse_account_position_status_report, parse_fill_report,
            parse_open_order_status_report, parse_order_status_report,
            parse_position_status_report, parse_tpsl_order_status_report,
        },
        query::{
            RisexAccountTradesQuery, RisexOpenOrdersQuery, RisexOrdersQuery, RisexPositionsQuery,
            RisexTpslOrdersQuery,
        },
    },
    signing::{action::PlaceOrder, tpsl::PlaceTpsl},
    websocket::{
        account_state::{parse_account_balance, parse_position_status_report as parse_ws_position},
        client::RisexWebSocketClient,
        dispatch::OrderDispatch,
        enums::RisexWsStatus,
        messages::{RisexWsMessage, RisexWsPosition},
    },
};

const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);

fn now() -> UnixNanos {
    get_atomic_clock_realtime().get_time_ns()
}

fn spawn(future: impl Future<Output = ()> + Send + 'static) -> JoinHandle<()> {
    get_runtime().spawn(future)
}

/// On-chain order ids are 24-byte hex; trigger (TP/SL) order ids are UUIDs.
fn is_onchain(venue_order_id: &VenueOrderId) -> bool {
    venue_order_id.as_str().starts_with("0x")
}

// unverified: that the `scOrderId` of the place response is the `wide_order_id` of the hex id.
fn onchain_order_id(order_id: &str) -> anyhow::Result<VenueOrderId> {
    let id = RisexOrderId::from_hex(order_id).or_else(|_| RisexOrderId::from_dash(order_id))?;
    Ok(VenueOrderId::new(id.to_hex()))
}

/// The venue writes "0" (or nothing) while a trigger order has not triggered.
fn triggered_order_id(order: &RisexTpslOrder) -> Option<&str> {
    Some(order.triggered_order_id.as_str()).filter(|id| !id.is_empty() && *id != "0")
}

/// Whether the venue answered and said no. Anything else may have reached it.
fn is_rejection(error: &RisexHttpError) -> bool {
    match error {
        RisexHttpError::Venue { .. }
        | RisexHttpError::InvalidRequest(_)
        | RisexHttpError::RateLimit { .. } => true,
        RisexHttpError::Http { status, .. } => (400..500).contains(status),
        _ => false,
    }
}

#[derive(Debug)]
enum Request {
    Order(PlaceOrder),
    Tpsl(PlaceTpsl),
}

#[derive(Debug)]
struct Cancel {
    strategy_id: StrategyId,
    instrument_id: InstrumentId,
    client_order_id: ClientOrderId,
    venue_order_id: Option<VenueOrderId>,
}

impl From<&CancelOrder> for Cancel {
    fn from(cmd: &CancelOrder) -> Self {
        Self {
            strategy_id: cmd.strategy_id,
            instrument_id: cmd.instrument_id,
            client_order_id: cmd.client_order_id,
            venue_order_id: cmd.venue_order_id,
        }
    }
}

#[derive(Debug)]
struct Modify {
    key: u64,
    order: OrderAny,
    market_id: u16,
    place: PlaceOrder,
    quantity: Quantity,
    price: Price,
}

/// What the spawned tasks of one connection share.
#[derive(Debug)]
struct Shared {
    session: RisexSession,
    dispatch: OrderDispatch,
    http: Arc<RisexRawHttpClient>,
    emitter: ExecutionEventEmitter,
    account: String,
    account_id: AccountId,
    markets: HashMap<InstrumentId, (u16, InstrumentAny)>,
    instruments: HashMap<u16, InstrumentAny>,
    connected_ns: u64,
}

impl Shared {
    fn market(&self, instrument_id: &InstrumentId) -> anyhow::Result<(u16, &InstrumentAny)> {
        let (market_id, instrument) = self
            .markets
            .get(instrument_id)
            .with_context(|| format!("unknown instrument {instrument_id}"))?;
        Ok((*market_id, instrument))
    }

    async fn emit_rest_balance(&self) -> anyhow::Result<()> {
        let balance = self
            .http
            .get_cross_margin_balance(&self.account)
            .await?
            .balance;
        // shortcut: REST gives one number, so all of it shows as free; the `account`
        // channel frame that follows the subscribe has the locked part.
        let balance =
            AccountBalance::from_total_and_locked(balance, Decimal::ZERO, Currency::USDC())
                .map_err(|e| anyhow::anyhow!("invalid account balance: {e}"))?;
        self.emitter
            .emit_account_state(vec![balance], vec![], true, now(), None);
        Ok(())
    }

    async fn submit(&self, key: u64, request: Request) {
        let result = match request {
            Request::Order(place) => self
                .session
                .place_order(&place)
                .await
                .map(|response| onchain_order_id(&response.order_id)),
            Request::Tpsl(tpsl) => self
                .session
                .place_tpsl(tpsl)
                .await
                .map(|response| Ok(VenueOrderId::new(response.order_id))),
        };
        match result {
            Ok(Ok(venue_order_id)) => self.dispatch.accepted(key, venue_order_id, now()),
            // The order is on the venue: the `orders` channel row accepts it.
            Ok(Err(e)) => log::error!("RISEx order {key} placed with an unreadable id: {e}"),
            Err(e) if is_rejection(&e) => {
                let reason = e.to_string();
                // unverified: the name of the post-only reject reason.
                let due_post_only = reason.to_ascii_lowercase().contains("post");
                self.dispatch.rejected(key, &reason, now(), due_post_only);
            }
            // The request may have reached the venue: the `orders` channel or
            // reconciliation decides.
            Err(e) => log::error!("RISEx submit of order {key} got no answer: {e}"),
        }
    }

    async fn cancel(&self, cancel: &Cancel) {
        let key = client_order_id_to_u64(cancel.client_order_id.as_str());
        let venue_order_id = self.dispatch.venue_order_id(key).or(cancel.venue_order_id);
        let result = match (self.market(&cancel.instrument_id), venue_order_id) {
            (Err(e), _) => Err(e.to_string()),
            (_, None) => Err("order has no venue order id yet".to_string()),
            (Ok((market_id, _)), Some(id)) if is_onchain(&id) => self
                .session
                .cancel_order(market_id, id.as_str())
                .await
                .map_err(|e| e.to_string()),
            // The poll loop reports the canceled trigger order.
            (Ok(_), Some(id)) => self
                .session
                .cancel_tpsl(id.as_str())
                .await
                .map_err(|e| e.to_string()),
        };
        if let Err(reason) = result {
            self.emitter.emit_order_cancel_rejected_event(
                cancel.strategy_id,
                cancel.instrument_id,
                cancel.client_order_id,
                venue_order_id,
                &reason,
                now(),
            );
        }
    }

    /// The venue has no modify: cancel, then place the rest under the same client order id.
    async fn modify(&self, modify: Modify) {
        let Modify {
            key,
            order,
            market_id,
            place,
            quantity,
            price,
        } = modify;
        let Some(old) = self.dispatch.venue_order_id(key).filter(is_onchain) else {
            self.emitter.emit_order_modify_rejected(
                &order,
                None,
                "order is not on the book",
                now(),
            );
            return;
        };
        // The cancel row of the old id must not close the order.
        self.dispatch.begin_modify(key);
        if let Err(e) = self.session.cancel_order(market_id, old.as_str()).await {
            self.dispatch.abort_modify(key);
            self.emitter
                .emit_order_modify_rejected(&order, Some(old), &e.to_string(), now());
            return;
        }
        // unverified: whether the venue lets the replacement reuse the client order id.
        match self.session.place_order(&place).await {
            Ok(response) => match onchain_order_id(&response.order_id) {
                Ok(new) => {
                    self.dispatch.set_venue_order_id(key, new);
                    self.emitter.emit_order_updated(
                        &order,
                        new,
                        quantity,
                        Some(price),
                        None,
                        None,
                        now(),
                    );
                }
                Err(e) => log::error!("RISEx order {key} replaced with an unreadable id: {e}"),
            },
            Err(e) => {
                self.emitter
                    .emit_order_modify_rejected(&order, Some(old), &e.to_string(), now());
                if is_rejection(&e) {
                    // The old order is gone and the venue refused the new one.
                    self.emitter.emit_order_canceled(&order, Some(old), now());
                    self.dispatch.close(key);
                } else {
                    log::error!("RISEx replacement of order {key} got no answer: {e}");
                }
            }
        }
    }

    async fn order_report(
        &self,
        venue_order_id: VenueOrderId,
        client_order_id: Option<ClientOrderId>,
    ) -> anyhow::Result<Option<OrderStatusReport>> {
        let report = if is_onchain(&venue_order_id) {
            let order = match self.http.get_order_by_id(venue_order_id.as_str()).await {
                Ok(order) => order,
                Err(RisexHttpError::Http { status: 404, .. }) => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            let Some(instrument) = self.instruments.get(&order.market_id) else {
                return Ok(None);
            };
            let mut report = parse_order_status_report(&order, self.account_id, instrument, now())?;
            report.client_order_id = self.dispatch.client_order_id(order.client_order_id);
            Some(report)
        } else {
            self.tpsl_reports(None, false)
                .await?
                .into_iter()
                .find(|report| report.venue_order_id == venue_order_id)
        };
        Ok(report.map(|mut report| {
            if client_order_id.is_some() {
                report.client_order_id = client_order_id;
            }
            report
        }))
    }

    async fn order_reports(
        &self,
        market_id: Option<u16>,
        open_only: bool,
    ) -> anyhow::Result<Vec<OrderStatusReport>> {
        let ts_init = now();
        let mut reports = Vec::new();
        if open_only {
            let mut query = RisexOpenOrdersQuery {
                account: self.account.clone(),
                market_id,
                limit: Some(MAX_PAGE_LIMIT),
                ..Default::default()
            };
            let mut fetched = 0;
            loop {
                let orders = self.http.get_open_orders(&query).await?.orders;
                for order in &orders {
                    let Some(instrument) = self.instruments.get(&order.market_id) else {
                        continue;
                    };
                    let mut report = parse_open_order_status_report(
                        order,
                        self.account_id,
                        instrument,
                        ts_init,
                    )?;
                    report.client_order_id = self.dispatch.client_order_id(order.client_order_id);
                    reports.push(report);
                }
                fetched += orders.len() as u64;
                if orders.len() < MAX_PAGE_LIMIT as usize {
                    break;
                }
                query.start_index = Some(fetched);
            }
        } else {
            let mut query = RisexOrdersQuery {
                account: self.account.clone(),
                market_id,
                limit: Some(MAX_PAGE_LIMIT),
                ..Default::default()
            };
            loop {
                let page = self.http.get_orders(&query).await?;
                for order in &page.orders {
                    let Some(instrument) = self.instruments.get(&order.market_id) else {
                        continue;
                    };
                    let mut report =
                        parse_order_status_report(order, self.account_id, instrument, ts_init)?;
                    report.client_order_id = self.dispatch.client_order_id(order.client_order_id);
                    reports.push(report);
                }
                if !page.has_next_page || page.orders.is_empty() {
                    break;
                }
                query.page = Some(page.page + 1);
            }
        }
        reports.extend(self.tpsl_reports(market_id, open_only).await?);
        Ok(reports)
    }

    async fn tpsl_orders(
        &self,
        market_id: Option<u16>,
        statuses: Vec<RisexTpslOrderStatus>,
    ) -> anyhow::Result<Vec<RisexTpslOrder>> {
        let mut query = RisexTpslOrdersQuery {
            account: self.account.clone(),
            market_id,
            statuses,
            limit: Some(MAX_PAGE_LIMIT),
            ..Default::default()
        };
        let mut orders = Vec::new();
        loop {
            let page = self.http.get_tpsl_orders(&query).await?;
            let fetched = page.orders.len();
            orders.extend(page.orders);
            if fetched == 0 || orders.len() as u64 >= page.total {
                return Ok(orders);
            }
            query.page = Some(page.page + 1);
        }
    }

    /// Trigger orders that never triggered. A triggered one is reported as the
    /// on-chain order it became, so its fills are not counted twice.
    async fn tpsl_reports(
        &self,
        market_id: Option<u16>,
        open_only: bool,
    ) -> anyhow::Result<Vec<OrderStatusReport>> {
        let statuses = if open_only {
            vec![RisexTpslOrderStatus::Accepted]
        } else {
            vec![]
        };
        let client_order_ids: HashMap<VenueOrderId, ClientOrderId> = self
            .dispatch
            .open_orders()
            .into_iter()
            .map(|(_, client_order_id, venue_order_id)| (venue_order_id, client_order_id))
            .collect();
        let ts_init = now();
        let mut reports = Vec::new();
        for order in self.tpsl_orders(market_id, statuses).await? {
            let Some(instrument) = self.instruments.get(&order.market_id) else {
                continue;
            };
            if triggered_order_id(&order).is_some() {
                continue;
            }
            let mut report =
                parse_tpsl_order_status_report(&order, self.account_id, instrument, ts_init)?;
            report.client_order_id = client_order_ids.get(&report.venue_order_id).copied();
            reports.push(report);
        }
        Ok(reports)
    }

    /// Newest first.
    async fn fill_reports(
        &self,
        market_id: Option<u16>,
        start: u64,
        end: u64,
    ) -> anyhow::Result<Vec<FillReport>> {
        let ts_init = now();
        let mut query = RisexAccountTradesQuery {
            account: self.account.clone(),
            market_id,
            limit: Some(MAX_PAGE_LIMIT),
            sorted_by: Some("-time".to_string()),
            ..Default::default()
        };
        let mut reports = Vec::new();
        loop {
            let page = self.http.get_account_trades(&query).await?;
            // unverified: the unit of the REST time filters, so time is filtered here.
            for trade in page
                .trades
                .iter()
                .filter(|t| (start..=end).contains(&t.time))
            {
                let Some(instrument) = self.instruments.get(&trade.market_id) else {
                    continue;
                };
                let mut report = parse_fill_report(trade, self.account_id, instrument, ts_init)?;
                report.client_order_id = self.dispatch.client_order_id(trade.client_order_id);
                reports.push(report);
            }
            let reached_start = page.trades.last().is_none_or(|t| t.time < start);
            if !page.has_next_page || reached_start {
                return Ok(reports);
            }
            query.page = Some(page.page + 1);
        }
    }

    async fn position_reports(
        &self,
        market: Option<(u16, &InstrumentAny)>,
    ) -> anyhow::Result<Vec<PositionStatusReport>> {
        let ts_init = now();
        if let Some((market_id, instrument)) = market {
            let position = self
                .http
                .get_account_position(&self.account, market_id)
                .await?
                .position;
            let report = parse_account_position_status_report(
                &position,
                self.account_id,
                instrument,
                ts_init,
            )?;
            return Ok(vec![report]);
        }
        let mut query = RisexPositionsQuery {
            account: self.account.clone(),
            ..Default::default()
        };
        let mut reports = Vec::new();
        loop {
            let page = self.http.get_positions(&query).await?;
            for position in &page.positions {
                let Some(instrument) = self.instruments.get(&position.market_id) else {
                    continue;
                };
                reports.push(parse_position_status_report(
                    position,
                    self.account_id,
                    instrument,
                    ts_init,
                )?);
            }
            if !page.has_next_page || page.positions.is_empty() {
                return Ok(reports);
            }
            query.page = Some(page.page + 1);
        }
    }

    /// Reports the positions of this account in a `positions` frame, snapshot or update.
    fn report_positions(&self, positions: &[RisexWsPosition]) {
        for position in positions {
            if !position.account.eq_ignore_ascii_case(&self.account) {
                continue;
            }
            let instrument = position.market_id.parse().ok();
            let Some(instrument) = instrument.and_then(|id: u16| self.instruments.get(&id)) else {
                continue;
            };
            match parse_ws_position(position, self.account_id, instrument, now()) {
                Ok(report) => self.emitter.send_position_report(report),
                Err(e) => log::warn!("RISEx positions row unreadable: {e}"),
            }
        }
    }

    /// After a reconnect: what the channels did not deliver while the socket was down.
    async fn catch_up(&self) {
        match self.fill_reports(None, self.connected_ns, u64::MAX).await {
            Ok(reports) => {
                for report in reports.into_iter().rev() {
                    if self.dispatch.is_new_trade(report.trade_id) {
                        self.emitter.send_fill_report(report);
                    }
                }
            }
            Err(e) => log::error!("RISEx fill catch-up failed: {e}"),
        }
        for (key, client_order_id, venue_order_id) in self.dispatch.open_orders() {
            if !is_onchain(&venue_order_id) {
                continue;
            }
            let order = match self.http.get_order_by_id(venue_order_id.as_str()).await {
                Ok(order) if order.status != RisexOrderStatus::Open => order,
                Ok(_) => continue,
                Err(e) => {
                    log::error!("RISEx order catch-up of {venue_order_id} failed: {e}");
                    continue;
                }
            };
            let Some(instrument) = self.instruments.get(&order.market_id) else {
                continue;
            };
            match parse_order_status_report(&order, self.account_id, instrument, now()) {
                Ok(mut report) => {
                    report.client_order_id = Some(client_order_id);
                    self.emitter.send_order_status_report(report);
                    self.dispatch.close(key);
                }
                Err(e) => log::error!("RISEx order catch-up of {venue_order_id} failed: {e}"),
            }
        }
    }

    async fn poll_tpsl(&self) {
        let open: Vec<_> = self
            .dispatch
            .open_orders()
            .into_iter()
            .filter(|(_, _, venue_order_id)| !is_onchain(venue_order_id))
            .collect();
        if open.is_empty() {
            return;
        }
        let orders = match self.tpsl_orders(None, vec![]).await {
            Ok(orders) => orders,
            Err(e) => {
                log::warn!("RISEx TP/SL poll failed: {e}");
                return;
            }
        };
        for (key, client_order_id, venue_order_id) in open {
            let Some(order) = orders
                .iter()
                .find(|order| order.order_id == venue_order_id.as_str())
            else {
                continue;
            };
            if let Some(triggered) = triggered_order_id(order) {
                // unverified: that the fills of a triggered order carry this id. From here
                // the `orders` and `fills` channels find the order by it.
                self.dispatch
                    .set_venue_order_id(key, VenueOrderId::new(triggered));
            } else if order.status == RisexTpslOrderStatus::Cancelled
                && let Some(instrument) = self.instruments.get(&order.market_id)
            {
                match parse_tpsl_order_status_report(order, self.account_id, instrument, now()) {
                    Ok(mut report) => {
                        report.client_order_id = Some(client_order_id);
                        self.emitter.send_order_status_report(report);
                        self.dispatch.close(key);
                    }
                    Err(e) => log::warn!("RISEx TP/SL order {venue_order_id} unreadable: {e}"),
                }
            }
        }
    }
}

async fn consume(
    shared: Arc<Shared>,
    mut ws: RisexWebSocketClient,
    mut rx: UnboundedReceiver<RisexWsMessage>,
    cancel: CancellationToken,
) {
    loop {
        let message = tokio::select! {
            () = cancel.cancelled() => break,
            message = rx.recv() => match message {
                Some(message) => message,
                None => break,
            },
        };
        match message {
            RisexWsMessage::Orders(frame) => shared.dispatch.handle_orders(&frame, now()),
            RisexWsMessage::Fills(frame) => shared.dispatch.handle_fill(&frame.data, now()),
            RisexWsMessage::Account(frame) => match parse_account_balance(&frame.data) {
                Ok(balance) => {
                    shared
                        .emitter
                        .emit_account_state(vec![balance], vec![], true, now(), None);
                }
                Err(e) => log::warn!("RISEx account frame unreadable: {e}"),
            },
            RisexWsMessage::Positions(frame) => shared.report_positions(&frame.data),
            // Positions also come from REST at each reconciliation: carry on without the channel.
            RisexWsMessage::Ack(ack)
                if ack.status == RisexWsStatus::Error
                    && ack.channel.as_deref() == Some("positions") =>
            {
                log::warn!("RISEx refused the positions subscription: {}", ack.message);
            }
            RisexWsMessage::Ack(ack) if ack.status == RisexWsStatus::Error => {
                // Without the account channels fills would be lost silently: stop instead.
                log::error!(
                    "RISEx refused an account subscription, disconnecting: {} (channel {:?})",
                    ack.message,
                    ack.channel
                );
                cancel.cancel();
                break;
            }
            RisexWsMessage::Reconnected => shared.catch_up().await,
            _ => {}
        }
    }
    ws.disconnect().await;
}

/// Execution client for one RISEx account.
#[derive(Debug)]
pub struct RisexExecutionClient {
    core: ExecutionClientCore,
    config: RisexExecClientConfig,
    emitter: ExecutionEventEmitter,
    http: RisexHttpClient,
    shared: Option<Arc<Shared>>,
    cancel: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}

impl RisexExecutionClient {
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be created.
    pub fn new(core: ExecutionClientCore, config: RisexExecClientConfig) -> anyhow::Result<Self> {
        let raw = RisexRawHttpClient::new_with_quotas(
            config.environment,
            config.base_url_http.clone(),
            config.http_timeout_secs,
            config.proxy_url.clone(),
            resolve_quota(config.rest_quota_per_10s),
        )?;
        let emitter = ExecutionEventEmitter::new(
            get_atomic_clock_realtime(),
            core.trader_id,
            core.account_id,
            core.account_type,
            None,
        );
        Ok(Self {
            core,
            config,
            emitter,
            http: RisexHttpClient::from_raw(raw),
            shared: None,
            cancel: CancellationToken::new(),
            tasks: Vec::new(),
        })
    }

    fn shared(&self) -> anyhow::Result<Arc<Shared>> {
        self.shared
            .clone()
            .filter(|_| self.is_connected())
            .context("RISEx execution client is not connected")
    }

    fn request(
        &self,
        shared: &Shared,
        order: &OrderAny,
        slippage_bps: u32,
    ) -> anyhow::Result<Request> {
        let instrument_id = order.instrument_id();
        let (market_id, instrument) = shared.market(&instrument_id)?;
        let price = match order.order_type() {
            OrderType::Market => order::market_price(
                order.order_side(),
                self.core.cache().quote(&instrument_id),
                instrument.price_increment().as_decimal(),
                slippage_bps,
            )?,
            OrderType::Limit => order
                .price()
                .context("limit order without price")?
                .as_decimal(),
            _ => return order::place_tpsl(order, market_id, instrument).map(Request::Tpsl),
        };
        order::place_order(
            order,
            market_id,
            instrument,
            order.quantity().as_decimal(),
            price,
            self.config.builder_id,
            now(),
        )
        .map(Request::Order)
    }

    fn submit(&self, orders: Vec<OrderAny>, params: Option<&Params>) -> anyhow::Result<()> {
        let shared = self.shared()?;
        let slippage_bps = params
            .and_then(|params| params.get_u64("market_order_slippage_bps"))
            .and_then(|bps| u32::try_from(bps).ok())
            .unwrap_or(self.config.market_order_slippage_bps);
        let mut requests = Vec::with_capacity(orders.len());
        for order in orders {
            if order.is_closed() {
                log::warn!("Cannot submit closed order {}", order.client_order_id());
                continue;
            }
            match self.request(&shared, &order, slippage_bps) {
                Ok(request) => {
                    self.emitter.emit_order_submitted(&order);
                    requests.push((shared.dispatch.track(&order), request));
                }
                Err(e) => self.emitter.emit_order_denied(&order, &e.to_string()),
            }
        }
        // One task, one order after the other: the venue has no batch endpoint.
        spawn(async move {
            for (key, request) in requests {
                shared.submit(key, request).await;
            }
        });
        Ok(())
    }

    fn modify(&self, shared: &Shared, cmd: &ModifyOrder) -> anyhow::Result<Modify> {
        let order = self.core.get_order(&cmd.client_order_id)?;
        anyhow::ensure!(
            order.order_type() == OrderType::Limit,
            "only limit orders can be modified"
        );
        let (market_id, instrument) = shared.market(&cmd.instrument_id)?;
        let quantity = cmd.quantity.unwrap_or(order.quantity());
        let price = cmd.price.or(order.price()).context("order without price")?;
        // shortcut: the open size is read before the cancel, so a fill that lands between
        // cancel and place makes the replacement too large; read the filled size from the
        // cancel row if modify is used on orders that fill often.
        let leaves = quantity.as_decimal() - order.filled_qty().as_decimal();
        let place = order::place_order(
            &order,
            market_id,
            instrument,
            leaves,
            price.as_decimal(),
            self.config.builder_id,
            now(),
        )?;
        Ok(Modify {
            key: shared.dispatch.track(&order),
            order,
            market_id,
            place,
            quantity,
            price,
        })
    }

    /// Sets the leverage of one market (`POST /v1/account/leverage`).
    ///
    /// Nautilus has no leverage command: strategy or bootstrap code calls this directly.
    /// The request runs in the background and its outcome is logged.
    ///
    /// # Errors
    ///
    /// Returns an error if the client is not connected, the instrument is not known or
    /// `leverage` is zero.
    pub fn update_leverage(&self, instrument_id: InstrumentId, leverage: u8) -> anyhow::Result<()> {
        anyhow::ensure!(leverage > 0, "leverage must be at least 1");
        let shared = self.shared()?;
        let (market_id, _) = shared.market(&instrument_id)?;
        spawn(async move {
            match shared.session.update_leverage(market_id, leverage).await {
                Ok(()) => log::info!("RISEx leverage of {instrument_id} set to {leverage}"),
                Err(e) => log::error!("RISEx leverage update of {instrument_id} failed: {e}"),
            }
        });
        Ok(())
    }

    /// Sets the margin mode of one market (`POST /v1/account/margin-mode`).
    ///
    /// The request runs in the background and its outcome is logged.
    ///
    /// # Errors
    ///
    /// Returns an error if the client is not connected or the instrument is not known.
    pub fn update_margin_mode(
        &self,
        instrument_id: InstrumentId,
        margin_mode: RisexMarginMode,
    ) -> anyhow::Result<()> {
        let shared = self.shared()?;
        let (market_id, _) = shared.market(&instrument_id)?;
        spawn(async move {
            match shared
                .session
                .update_margin_mode(market_id, margin_mode)
                .await
            {
                Ok(()) => log::info!("RISEx margin mode of {instrument_id} set to {margin_mode:?}"),
                Err(e) => log::error!("RISEx margin mode update of {instrument_id} failed: {e}"),
            }
        });
        Ok(())
    }
}

#[async_trait(?Send)]
impl ExecutionClient for RisexExecutionClient {
    fn is_connected(&self) -> bool {
        self.core.is_connected() && !self.cancel.is_cancelled()
    }

    fn client_id(&self) -> ClientId {
        self.core.client_id
    }

    fn account_id(&self) -> AccountId {
        self.core.account_id
    }

    fn venue(&self) -> Venue {
        self.core.venue
    }

    fn oms_type(&self) -> OmsType {
        self.core.oms_type
    }

    fn get_account(&self) -> Option<AccountAny> {
        self.core.cache().account_owned(&self.core.account_id)
    }

    fn generate_account_state(
        &self,
        balances: Vec<AccountBalance>,
        margins: Vec<MarginBalance>,
        reported: bool,
        ts_event: UnixNanos,
        info: Option<Params>,
    ) -> anyhow::Result<()> {
        self.emitter
            .emit_account_state(balances, margins, reported, ts_event, info);
        Ok(())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        if !self.core.is_started() {
            self.emitter.set_sender(get_exec_event_sender());
            self.core.set_started();
        }
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.cancel.cancel();
        self.core.set_stopped();
        Ok(())
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        if self.is_connected() {
            return Ok(());
        }
        let config = &self.config;
        let credential = RisexCredential::resolve(
            config.account_address.clone(),
            config.signer_private_key.clone(),
            config.environment,
        )?
        .context("RISEx execution needs an account address and a signer key")?;

        let registry = self.http.market_registry();
        let mut markets = HashMap::new();
        let mut instruments = HashMap::new();
        for instrument in self.http.request_instruments().await? {
            if let Some(market_id) = registry.market_id(&instrument.id()) {
                instruments.insert(market_id, instrument.clone());
                markets.insert(instrument.id(), (market_id, instrument));
            }
        }
        let market_of = |instrument_id: &InstrumentId| {
            markets
                .get(instrument_id)
                .map(|(market_id, _)| *market_id)
                .with_context(|| format!("unknown instrument {instrument_id} in the config"))
        };

        let http = self.http.inner.clone();
        let session =
            RisexSession::connect(http.clone(), &credential, config.deadline_secs).await?;
        for (instrument_id, margin_mode) in &config.margin_mode {
            session
                .update_margin_mode(market_of(instrument_id)?, *margin_mode)
                .await
                .with_context(|| format!("setting the margin mode of {instrument_id}"))?;
        }
        for (instrument_id, leverage) in &config.leverage {
            session
                .update_leverage(market_of(instrument_id)?, *leverage)
                .await
                .with_context(|| format!("setting the leverage of {instrument_id}"))?;
        }

        let account = session.account().to_string();
        let market_ids: Vec<u32> = instruments.keys().map(|id| u32::from(*id)).collect();
        let shared = Arc::new(Shared {
            dispatch: OrderDispatch::new(
                &account,
                self.core.account_id,
                self.emitter.clone(),
                instruments.clone(),
            ),
            session,
            http,
            emitter: self.emitter.clone(),
            account,
            account_id: self.core.account_id,
            markets,
            instruments,
            connected_ns: now().as_u64(),
        });
        shared.emit_rest_balance().await?;

        let url = config
            .base_url_ws
            .clone()
            .unwrap_or_else(|| risex_ws_url(config.environment).to_string());
        let mut ws = RisexWebSocketClient::new(url);
        // shortcut: no WS auth; the account channels deliver on the `makers` filter alone
        // (verified 10 Nov 2026). Add the auth request when the venue starts to refuse.
        ws.subscribe_orders(&market_ids, &shared.account).await?;
        ws.subscribe_fills(&market_ids, &shared.account).await?;
        ws.subscribe_positions(&market_ids, &shared.account).await?;
        ws.subscribe_account(&shared.account).await?;
        let rx = ws.connect().await?;

        self.cancel = CancellationToken::new();
        self.tasks = vec![spawn(consume(shared.clone(), ws, rx, self.cancel.clone()))];
        let poll_interval = Duration::from_secs(config.tpsl_poll_interval_secs.max(1));
        let (poller, cancel) = (shared.clone(), self.cancel.clone());
        self.tasks.push(spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(poll_interval) => poller.poll_tpsl().await,
                }
            }
        }));
        self.shared = Some(shared);
        self.core.set_connected();
        log::info!("Connected RISEx execution client {}", self.core.client_id);
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.cancel.cancel();
        for mut task in self.tasks.drain(..) {
            if tokio::time::timeout(DISCONNECT_TIMEOUT, &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
        self.shared = None;
        self.core.set_disconnected();
        Ok(())
    }

    fn submit_order(&self, cmd: SubmitOrder) -> anyhow::Result<()> {
        let order = self.core.get_order(&cmd.client_order_id)?;
        self.submit(vec![order], cmd.params.as_ref())
    }

    fn submit_order_list(&self, cmd: SubmitOrderList) -> anyhow::Result<()> {
        let orders = self.core.get_orders_for_list(&cmd.order_list)?;
        self.submit(orders, cmd.params.as_ref())
    }

    fn modify_order(&self, cmd: ModifyOrder) -> anyhow::Result<()> {
        let shared = self.shared()?;
        match self.modify(&shared, &cmd) {
            Ok(modify) => {
                spawn(async move { shared.modify(modify).await });
            }
            Err(e) => self.emitter.emit_order_modify_rejected_event(
                cmd.strategy_id,
                cmd.instrument_id,
                cmd.client_order_id,
                cmd.venue_order_id,
                &e.to_string(),
                now(),
            ),
        }
        Ok(())
    }

    fn cancel_order(&self, cmd: CancelOrder) -> anyhow::Result<()> {
        let shared = self.shared()?;
        spawn(async move { shared.cancel(&Cancel::from(&cmd)).await });
        Ok(())
    }

    fn cancel_all_orders(&self, cmd: CancelAllOrders) -> anyhow::Result<()> {
        let shared = self.shared()?;
        let (market_id, _) = shared.market(&cmd.instrument_id)?;
        let cache = self.core.cache();
        let open = cache.orders_open(
            None,
            Some(&cmd.instrument_id),
            Some(&cmd.strategy_id),
            None,
            cmd.order_side,
        );
        if cmd.order_side.is_some() {
            let cancels: Vec<Cancel> = open
                .iter()
                .map(|order| Cancel {
                    strategy_id: order.strategy_id(),
                    instrument_id: order.instrument_id(),
                    client_order_id: order.client_order_id(),
                    venue_order_id: order.venue_order_id(),
                })
                .collect();
            spawn(async move {
                for cancel in &cancels {
                    shared.cancel(cancel).await;
                }
            });
            return Ok(());
        }
        // The venue call cancels every order of the account in the market,
        // whichever strategy sent it.
        let triggers = open
            .iter()
            .any(|order| !matches!(order.order_type(), OrderType::Market | OrderType::Limit));
        spawn(async move {
            if let Err(e) = shared.session.cancel_all_orders(market_id).await {
                log::error!("RISEx cancel-all of market {market_id} failed: {e}");
            }
            if triggers && let Err(e) = shared.session.cancel_all_tpsl(market_id).await {
                log::error!("RISEx TP/SL cancel-all of market {market_id} failed: {e}");
            }
        });
        Ok(())
    }

    fn batch_cancel_orders(&self, cmd: BatchCancelOrders) -> anyhow::Result<()> {
        let shared = self.shared()?;
        // One after the other: the venue has no batch endpoint.
        spawn(async move {
            for cancel in &cmd.cancels {
                shared.cancel(&Cancel::from(cancel)).await;
            }
        });
        Ok(())
    }

    fn query_account(&self, _cmd: QueryAccount) -> anyhow::Result<()> {
        let shared = self.shared()?;
        spawn(async move {
            if let Err(e) = shared.emit_rest_balance().await {
                log::warn!("RISEx account query failed: {e}");
            }
        });
        Ok(())
    }

    fn query_order(&self, cmd: QueryOrder) -> anyhow::Result<()> {
        let shared = self.shared()?;
        spawn(async move {
            let key = client_order_id_to_u64(cmd.client_order_id.as_str());
            let Some(venue_order_id) = shared.dispatch.venue_order_id(key).or(cmd.venue_order_id)
            else {
                log::warn!("RISEx order {} has no venue order id", cmd.client_order_id);
                return;
            };
            match shared
                .order_report(venue_order_id, Some(cmd.client_order_id))
                .await
            {
                Ok(Some(report)) => shared.emitter.send_order_status_report(report),
                Ok(None) => log::warn!("RISEx order {venue_order_id} not found"),
                Err(e) => log::warn!("RISEx order query of {venue_order_id} failed: {e}"),
            }
        });
        Ok(())
    }

    async fn generate_order_status_report(
        &self,
        cmd: &GenerateOrderStatusReport,
    ) -> anyhow::Result<Option<OrderStatusReport>> {
        let shared = self.shared()?;
        let tracked = cmd.client_order_id.and_then(|id| {
            shared
                .dispatch
                .venue_order_id(client_order_id_to_u64(id.as_str()))
        });
        match cmd.venue_order_id.or(tracked) {
            Some(venue_order_id) => {
                shared
                    .order_report(venue_order_id, cmd.client_order_id)
                    .await
            }
            None => Ok(None),
        }
    }

    async fn generate_order_status_reports(
        &self,
        cmd: &GenerateOrderStatusReports,
    ) -> anyhow::Result<Vec<OrderStatusReport>> {
        let shared = self.shared()?;
        let market_id = match &cmd.instrument_id {
            Some(instrument_id) => Some(shared.market(instrument_id)?.0),
            None => None,
        };
        let mut reports = shared.order_reports(market_id, cmd.open_only).await?;
        if !cmd.open_only {
            // unverified: the unit of the REST time filters, so time is filtered here.
            let start = cmd.start.unwrap_or_default();
            let end = cmd.end.unwrap_or(UnixNanos::max());
            reports.retain(|report| (start..=end).contains(&report.ts_last));
        }
        Ok(reports)
    }

    async fn generate_fill_reports(
        &self,
        cmd: GenerateFillReports,
    ) -> anyhow::Result<Vec<FillReport>> {
        let shared = self.shared()?;
        let market_id = match &cmd.instrument_id {
            Some(instrument_id) => Some(shared.market(instrument_id)?.0),
            None => None,
        };
        let start = cmd.start.map_or(0, |ts| ts.as_u64());
        let end = cmd.end.map_or(u64::MAX, |ts| ts.as_u64());
        let mut reports = shared.fill_reports(market_id, start, end).await?;
        if let Some(venue_order_id) = cmd.venue_order_id {
            reports.retain(|report| report.venue_order_id == venue_order_id);
        }
        // The engine has these fills now: the `fills` channel must not send them again.
        for report in &reports {
            shared.dispatch.is_new_trade(report.trade_id);
        }
        Ok(reports)
    }

    async fn generate_position_status_reports(
        &self,
        cmd: &GeneratePositionStatusReports,
    ) -> anyhow::Result<Vec<PositionStatusReport>> {
        let shared = self.shared()?;
        let market = match &cmd.instrument_id {
            Some(instrument_id) => Some(shared.market(instrument_id)?),
            None => None,
        };
        shared.position_reports(market).await
    }
}
