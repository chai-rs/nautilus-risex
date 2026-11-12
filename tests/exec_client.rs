//! Execution client against a local axum mock of the venue (HTTP and WebSocket).
//!
//! Nothing here talks to a real host: both base URLs point at 127.0.0.1. The mock
//! recomputes the EIP-712 digest of every signed POST from the body it received and
//! recovers the signer from it; any mismatch fails the test when the harness drops.

use std::{
    cell::RefCell,
    collections::HashSet,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alloy::{
    signers::local::PrivateKeySigner,
    sol,
    sol_types::{Eip712Domain, SolStruct},
};
use alloy_primitives::{Address, B256, Signature, U256, aliases::U48};
use axum::{
    Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::Engine;
use nautilus_common::{
    cache::Cache,
    clients::ExecutionClient,
    live::runner::replace_exec_event_sender,
    messages::{
        ExecutionEvent, ExecutionReport,
        execution::{
            CancelAllOrders, CancelOrder, GenerateFillReports, GenerateOrderStatusReport,
            GenerateOrderStatusReports, GeneratePositionStatusReports, ModifyOrder, SubmitOrder,
        },
    },
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    data::QuoteTick,
    enums::{AccountType, LiquiditySide, OmsType, OrderSide, OrderType, PositionSide},
    events::{AccountState, OrderEventAny},
    identifiers::{ClientId, ClientOrderId, InstrumentId, TraderId, VenueOrderId},
    orders::{Order, OrderAny, OrderTestBuilder},
    types::{Money, Price, Quantity},
};
use nautilus_risex::{
    common::{
        consts::RISEX_VENUE,
        enums::RisexMarginMode,
        parse::{RisexOrderId, client_order_id_to_u64},
    },
    config::exec::RisexExecClientConfig,
    execution::RisexExecutionClient,
    signing::action::{
        PlaceOrder, cancel_all_orders_hash, cancel_order_hash, update_leverage_hash,
        update_margin_mode_hash,
    },
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::{Notify, broadcast, mpsc::UnboundedReceiver};

/// The account of the recorded fixtures.
const ACCOUNT: &str = "0x36Dc0d99DCaEe443cEbaa5FB8c11877d157C26df";
const AERO_MARKET: u16 = 15;
const TPSL_ORDER_ID: &str = "5f0c1d1e-8a55-4c61-9d0b-0d5c5b1c2a77";

// The mock's own copy of the signed types: it must not trust the client's.
sol! {
    struct VerifyWitness {
        address account;
        address target;
        bytes32 hash;
        uint48 nonceAnchor;
        uint8 nonceBitmap;
        uint32 deadline;
    }

    struct PlaceTpslOrder {
        address account;
        uint64 marketId;
        uint8 side;
        string size;
        uint8 stopType;
        string stopPrice;
        string limitPrice;
        uint8 orderType;
        uint8 stopPriceOption;
        uint8 tif;
        uint32 deadline;
        uint32 sizePercentBps;
    }

    struct CancelTpslOrder {
        address account;
        string orderId;
        uint32 deadline;
    }

    struct CancelAllTpslOrders {
        address account;
        uint64 marketId;
        uint32 deadline;
    }
}

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/http/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The first recorded `update` frame of an account channel.
fn ws_fixture(channel: &str) -> Value {
    include_str!("fixtures/ws/account_channels_unauth.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|frame| frame["channel"] == channel && frame["type"] == "update")
        .unwrap()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn now_nanos() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string()
}

fn num(value: &Value, key: &str) -> Result<u64, String> {
    let field = &value[key];
    field
        .as_u64()
        .or_else(|| field.as_str()?.parse().ok())
        .ok_or_else(|| format!("{key} is not a number: {field}"))
}

fn narrow<T: TryFrom<u64>>(value: &Value, key: &str) -> Result<T, String> {
    T::try_from(num(value, key)?).map_err(|_| format!("{key} is out of range"))
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("{key} is not a string: {}", value[key]))
}

fn flag(value: &Value, key: &str) -> Result<bool, String> {
    value[key]
        .as_bool()
        .ok_or_else(|| format!("{key} is not a bool: {}", value[key]))
}

fn address(value: &Value, key: &str) -> Result<Address, String> {
    text(value, key)?
        .parse()
        .map_err(|e| format!("{key} is not an address: {e}"))
}

fn place_hash(order: &Value) -> Result<B256, String> {
    PlaceOrder {
        market_id: narrow(order, "market_id")?,
        size_steps: narrow(order, "size_steps")?,
        price_ticks: narrow(order, "price_ticks")?,
        side: narrow(order, "side")?,
        post_only: flag(order, "post_only")?,
        reduce_only: flag(order, "reduce_only")?,
        stp_mode: narrow(order, "stp_mode")?,
        order_type: narrow(order, "order_type")?,
        time_in_force: narrow(order, "time_in_force")?,
        builder_id: narrow(order, "builder_id")?,
        client_order_id: num(order, "client_order_id")?,
        ttl_units: narrow(order, "ttl_units")?,
    }
    .action_hash()
    .map_err(|e| e.to_string())
}

fn data(value: Value) -> Response {
    axum::Json(json!({"data": value, "request_id": "mock"})).into_response()
}

fn venue_error(code: &str, message: &str) -> Response {
    let body = json!({"error": {"code": code, "message": message}, "request_id": "mock"});
    (StatusCode::BAD_REQUEST, axum::Json(body)).into_response()
}

fn order_id(n: u64) -> String {
    format!("0x{:048x}", 0x1000 + n)
}

#[derive(Default)]
struct VenueState {
    gets: Vec<String>,
    posts: Vec<(String, Value)>,
    failures: Vec<String>,
    nonces: HashSet<(u64, u64)>,
    nonce_reads: usize,
    orders_placed: u64,
    reject_place: Option<(&'static str, &'static str)>,
    stale_nonce_once: bool,
    refuse_subscriptions: bool,
    refuse_positions: bool,
    /// Replaces the trades of `GET /v1/trade-history`.
    trades: Option<Value>,
    /// The order of `GET /v1/orders/by-id/{id}`; 404 without it.
    order: Option<Value>,
}

#[derive(Clone)]
struct Venue {
    signer: Address,
    account: Address,
    router: Address,
    domain: Eip712Domain,
    state: Arc<Mutex<VenueState>>,
    frames: broadcast::Sender<String>,
    drop_connection: Arc<Notify>,
    subscriptions: Arc<AtomicUsize>,
}

impl Venue {
    /// Returns the venue and its HTTP and WebSocket URLs.
    async fn start(signer: Address) -> (Self, String, String) {
        let domain = fixture("eip712_domain");
        let domain = &domain["data"];
        let venue = Self {
            signer,
            account: ACCOUNT.parse().unwrap(),
            router: address(&fixture("system_config")["data"]["addresses"], "router").unwrap(),
            domain: Eip712Domain::new(
                Some(text(domain, "name").unwrap().to_string().into()),
                Some(text(domain, "version").unwrap().to_string().into()),
                Some(U256::from(num(domain, "chain_id").unwrap())),
                Some(address(domain, "verifying_contract").unwrap()),
                None,
            ),
            state: Arc::default(),
            frames: broadcast::channel(64).0,
            drop_connection: Arc::default(),
            subscriptions: Arc::default(),
        };
        let app = Router::new()
            .route("/ws", get(upgrade))
            .fallback(http)
            .with_state(venue.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (
            venue,
            format!("http://{address}"),
            format!("ws://{address}/ws"),
        )
    }

    fn get(&self, path: &str, query: &str) -> Response {
        let mut state = self.state.lock();
        state.gets.push(path.to_string());
        let file = match path {
            "/v1/markets" => "markets",
            "/v1/system/config" => "system_config",
            "/v1/auth/eip712-domain" => "eip712_domain",
            "/v1/account/cross-margin-balance" => "cross_margin_balance",
            "/v1/account/position" => "position_aero",
            "/v1/positions" => "positions",
            "/v1/orders/open" => "orders_open",
            "/v1/orders" => "orders_history",
            "/v1/orders/tpsl" => "tpsl_orders",
            "/v1/trade-history" => "account_trades",
            path if path.starts_with("/v1/nonce-state/") => {
                state.nonce_reads += 1;
                "nonce_state"
            }
            path if path.starts_with("/v1/orders/by-id/") => {
                return match &state.order {
                    Some(order) => data(json!({"order": order})),
                    None => StatusCode::NOT_FOUND.into_response(),
                };
            }
            _ => {
                state.failures.push(format!("unexpected GET {path}"));
                return StatusCode::NOT_FOUND.into_response();
            }
        };
        let mut body = fixture(file);
        let page = &mut body["data"];
        let later_page = query
            .split('&')
            .any(|pair| pair.strip_prefix("page=").is_some_and(|page| page != "1"));
        if later_page {
            // The recorded first pages say `has_next_page`: the next one is empty.
            for rows in ["orders", "trades", "positions"] {
                if page.get(rows).is_some() {
                    page[rows] = json!([]);
                }
            }
            page["has_next_page"] = json!(false);
        } else if path == "/v1/trade-history"
            && let Some(trades) = &state.trades
        {
            page["trades"] = trades.clone();
            page["has_next_page"] = json!(false);
        } else if path == "/v1/orders/tpsl" && query.contains("ACCEPTED") {
            // Every recorded trigger order is canceled.
            page["orders"] = json!([]);
            page["total"] = json!("0");
        }
        axum::Json(body).into_response()
    }

    fn post(&self, path: &str, body: Value) -> Response {
        let verdict = self.verify(path, &body);
        let mut state = self.state.lock();
        state.posts.push((path.to_string(), body));
        if let Err(reason) = verdict {
            state.failures.push(format!("POST {path}: {reason}"));
            return venue_error("Unauthorized", &reason);
        }
        match path {
            "/v1/orders/place" if std::mem::take(&mut state.stale_nonce_once) => {
                venue_error("InvalidNonceAnchor", "nonce anchor is stale")
            }
            "/v1/orders/place" => match state.reject_place.take() {
                Some((code, message)) => venue_error(code, message),
                None => {
                    state.orders_placed += 1;
                    data(json!({"order_id": order_id(state.orders_placed), "message": "ok"}))
                }
            },
            "/v1/orders/tpsl" => data(json!({"order_id": TPSL_ORDER_ID})),
            _ => data(json!({"success": true})),
        }
    }

    /// Recomputes the digest from the body and recovers the signer from the signature.
    fn verify(&self, path: &str, body: &Value) -> Result<(), String> {
        let (digest, signed) = match path {
            "/v1/orders/tpsl" => {
                let request = PlaceTpslOrder {
                    account: address(body, "account")?,
                    marketId: num(body, "market_id")?,
                    side: narrow(body, "side")?,
                    size: text(body, "size")?.to_string(),
                    stopType: narrow(body, "stop_type")?,
                    stopPrice: text(body, "stop_price")?.to_string(),
                    limitPrice: text(body, "limit_price")?.to_string(),
                    orderType: narrow(body, "order_type")?,
                    stopPriceOption: narrow(body, "stop_price_option")?,
                    tif: narrow(body, "tif")?,
                    deadline: narrow(body, "deadline")?,
                    sizePercentBps: narrow(body, "size_percent_bps")?,
                };
                (request.eip712_signing_hash(&self.domain), body)
            }
            "/v1/orders/tpsl/cancel" => {
                let request = CancelTpslOrder {
                    account: address(body, "account")?,
                    orderId: text(body, "order_id")?.to_string(),
                    deadline: narrow(body, "deadline")?,
                };
                (request.eip712_signing_hash(&self.domain), body)
            }
            "/v1/orders/tpsl/cancel-all" => {
                let request = CancelAllTpslOrders {
                    account: address(body, "account")?,
                    marketId: num(body, "market_id")?,
                    deadline: narrow(body, "deadline")?,
                };
                (request.eip712_signing_hash(&self.domain), body)
            }
            _ => {
                let market_id = || narrow::<u16>(body, "market_id");
                let (hash, permit) = match path {
                    "/v1/orders/place" => (place_hash(body)?, &body["permit"]),
                    "/v1/orders/cancel" => {
                        // The signed hash covers the resting order id, not the body's id.
                        let id = RisexOrderId::from_hex(text(body, "order_id")?)
                            .map_err(|e| e.to_string())?;
                        let hash = cancel_order_hash(market_id()?, id.resting_order_id());
                        (hash, &body["permit"])
                    }
                    "/v1/orders/cancel-all" => {
                        (cancel_all_orders_hash(market_id()?), &body["permit"])
                    }
                    "/v1/account/leverage" => (
                        update_leverage_hash(market_id()?, narrow(body, "leverage")?),
                        &body["permit_params"],
                    ),
                    "/v1/account/margin-mode" => (
                        update_margin_mode_hash(market_id()?, narrow(body, "margin_mode")?),
                        &body["permit_params"],
                    ),
                    other => return Err(format!("unexpected POST {other}")),
                };
                let nonce = (
                    num(permit, "nonce_anchor")?,
                    num(permit, "nonce_bitmap_index")?,
                );
                if !self.state.lock().nonces.insert(nonce) {
                    return Err(format!("nonce {nonce:?} used twice"));
                }
                let witness = VerifyWitness {
                    account: address(permit, "account")?,
                    target: self.router,
                    hash,
                    nonceAnchor: U48::from(nonce.0),
                    nonceBitmap: narrow(permit, "nonce_bitmap_index")?,
                    deadline: narrow(permit, "deadline")?,
                };
                (witness.eip712_signing_hash(&self.domain), permit)
            }
        };
        if address(signed, "account")? != self.account {
            return Err(format!("wrong account {}", signed["account"]));
        }
        let (deadline, now) = (num(signed, "deadline")?, now_secs());
        if !(now..=now + 65).contains(&deadline) {
            return Err(format!("deadline {deadline} is not now + 60 s (now {now})"));
        }
        let signature = base64::engine::general_purpose::STANDARD
            .decode(text(signed, "signature")?)
            .map_err(|e| e.to_string())?;
        let recovered = Signature::from_raw(&signature)
            .map_err(|e| e.to_string())?
            .recover_address_from_prehash(&digest)
            .map_err(|e| e.to_string())?;
        if recovered != self.signer || address(signed, "signer")? != self.signer {
            return Err(format!(
                "signature recovers to {recovered}, not the session signer"
            ));
        }
        Ok(())
    }

    async fn serve(self, mut socket: WebSocket) {
        let mut frames = self.frames.subscribe();
        loop {
            tokio::select! {
                () = self.drop_connection.notified() => return,
                frame = frames.recv() => {
                    let Ok(frame) = frame else { return };
                    if socket.send(Message::Text(frame.into())).await.is_err() {
                        return;
                    }
                }
                message = socket.recv() => {
                    let Some(Ok(message)) = message else { return };
                    let Message::Text(request) = message else { continue };
                    let request: Value = serde_json::from_str(&request).unwrap_or_default();
                    if request["method"] != "subscribe" {
                        continue;
                    }
                    let mut filter = request["params"].clone();
                    let channel = filter
                        .as_object_mut()
                        .and_then(|params| params.remove("channel"))
                        .unwrap_or_default();
                    let refused = {
                        let state = self.state.lock();
                        state.refuse_subscriptions
                            || (state.refuse_positions && channel == "positions")
                    };
                    let ack = if refused {
                        json!({"type": "subscribed", "method": "subscribe", "status": "error", "message": "Failed to subscribe: unauthorized", "channel": channel, "data": {}})
                    } else {
                        json!({"type": "subscribed", "method": "subscribe", "status": "success", "message": "ok", "channel": channel, "data": filter})
                    };
                    if socket.send(Message::Text(ack.to_string().into())).await.is_err() {
                        return;
                    }
                    self.subscriptions.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    }

    fn push(&self, frame: &Value) {
        self.frames
            .send(frame.to_string())
            .expect("no WebSocket connection");
    }

    fn posts(&self, path: &str) -> Vec<Value> {
        let state = self.state.lock();
        let posts = state.posts.iter().filter(|(p, _)| p == path);
        posts.map(|(_, body)| body.clone()).collect()
    }

    async fn wait_posts(&self, path: &str, count: usize) -> Vec<Value> {
        wait_until(path, || self.posts(path).len() >= count).await;
        self.posts(path)
    }

    async fn wait_subscriptions(&self, count: usize) {
        wait_until("subscriptions", || {
            self.subscriptions.load(Ordering::SeqCst) >= count
        })
        .await;
    }
}

async fn upgrade(ws: WebSocketUpgrade, State(venue): State<Venue>) -> Response {
    ws.on_upgrade(|socket| venue.serve(socket))
}

async fn http(State(venue): State<Venue>, method: Method, uri: Uri, body: String) -> Response {
    if method == Method::GET {
        venue.get(uri.path(), uri.query().unwrap_or_default())
    } else {
        venue.post(uri.path(), serde_json::from_str(&body).unwrap_or_default())
    }
}

async fn wait_until(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..1000 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn aero() -> InstrumentId {
    InstrumentId::from("AERO-USDC-PERP.RISEX")
}

fn client_id() -> ClientId {
    ClientId::from("RISEX")
}

fn trader_id() -> TraderId {
    TraderId::from("TESTER-001")
}

fn limit(id: &str, quantity: &str, price: &str) -> OrderAny {
    OrderTestBuilder::new(OrderType::Limit)
        .instrument_id(aero())
        .client_order_id(ClientOrderId::from(id))
        .side(OrderSide::Buy)
        .quantity(Quantity::from(quantity))
        .price(Price::from(price))
        .build()
}

fn key(order: &OrderAny) -> String {
    client_order_id_to_u64(order.client_order_id().as_str()).to_string()
}

/// An `orders` channel frame with one row of the account.
fn orders_frame(order: &OrderAny, id: &VenueOrderId, filled: &str, status: &str) -> Value {
    let mut frame = ws_fixture("orders");
    frame["market_id"] = json!("15");
    let row = &mut frame["data"][0];
    row["id"] = json!(id.as_str());
    row["client_order_id"] = json!(key(order));
    row["market_id"] = json!("15");
    row["sender"] = json!(ACCOUNT);
    row["side"] = json!("BUY");
    row["post_only"] = json!(false);
    row["price"] = json!("0.5");
    row["avg_price"] = json!(if filled == "0" { "0" } else { "0.5" });
    row["size"] = json!(order.quantity().to_string());
    row["filled_size"] = json!(filled);
    row["status"] = json!(status);
    let canceled = status == "ORDER_STATUS_CANCELLED";
    row["cancel_reason"] = json!(if canceled { "cancelled" } else { "" });
    frame
}

/// The fields a `fills` frame and a REST trade share.
fn set_trade(trade: &mut Value, order: &OrderAny, id: &VenueOrderId, n: u64, size: &str) {
    trade["id"] = json!(format!("{id}-0x{:048x}", 0xf00 + n));
    trade["market_id"] = json!("15");
    trade["order_id"] = json!(id.as_str());
    trade["client_order_id"] = json!(key(order));
    trade["side"] = json!("BUY");
    trade["price"] = json!("0.5");
    trade["size"] = json!(size);
    trade["fee"] = json!("-0.0006");
    trade["liquidity_indicator"] = json!("MAKER");
    trade["time"] = json!(now_nanos());
}

/// A `fills` channel frame: trade `n` of the order.
fn fill_frame(order: &OrderAny, id: &VenueOrderId, n: u64, size: &str) -> Value {
    let mut frame = ws_fixture("fills");
    frame["market_id"] = json!("15");
    set_trade(&mut frame["data"], order, id, n, size);
    frame
}

/// The same trade as `GET /v1/trade-history` returns it.
fn rest_trade(order: &OrderAny, id: &VenueOrderId, n: u64, size: &str) -> Value {
    let mut trade = fixture("account_trades")["data"]["trades"][0].clone();
    set_trade(&mut trade, order, id, n, size);
    trade
}

async fn next_event(events: &mut UnboundedReceiver<ExecutionEvent>) -> ExecutionEvent {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("timed out waiting for an event")
        .expect("event channel closed")
}

struct Harness {
    venue: Venue,
    client: RisexExecutionClient,
    events: UnboundedReceiver<ExecutionEvent>,
    cache: Rc<RefCell<Cache>>,
    /// The account state of the REST read at connect.
    rest_account: AccountState,
}

impl Drop for Harness {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let failures = &self.venue.state.lock().failures;
            assert!(failures.is_empty(), "the mock venue refused: {failures:?}");
        }
    }
}

impl Harness {
    async fn connect() -> Self {
        Self::connect_with(|_, _| {}).await
    }

    async fn connect_with(configure: impl FnOnce(&mut RisexExecClientConfig, &Venue)) -> Self {
        // A random key that lives in this process only.
        let signer = PrivateKeySigner::random();
        let (venue, http_url, ws_url) = Venue::start(signer.address()).await;
        let mut config = RisexExecClientConfig {
            base_url_http: Some(http_url),
            base_url_ws: Some(ws_url),
            account_address: Some(ACCOUNT.to_string()),
            signer_private_key: Some(alloy_primitives::hex::encode(signer.to_bytes())),
            ..Default::default()
        };
        configure(&mut config, &venue);

        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        replace_exec_event_sender(sender);
        let cache = Rc::new(RefCell::new(Cache::default()));
        let core = ExecutionClientCore::new(
            trader_id(),
            client_id(),
            *RISEX_VENUE,
            OmsType::Netting,
            config.account_id,
            AccountType::Margin,
            None,
            cache.clone(),
        );
        let mut client = RisexExecutionClient::new(core, config).unwrap();
        client.start().unwrap();
        client.connect().await.unwrap();
        let ExecutionEvent::Account(rest_account) = next_event(&mut events).await else {
            panic!("expected the account state of the REST read");
        };
        venue.wait_subscriptions(4).await;
        Self {
            venue,
            client,
            events,
            cache,
            rest_account,
        }
    }

    async fn next_order_event(&mut self) -> OrderEventAny {
        match next_event(&mut self.events).await {
            ExecutionEvent::Order(event) => event,
            other => panic!("expected an order event, was {other:?}"),
        }
    }

    fn submit(&self, order: &OrderAny) {
        self.cache
            .borrow_mut()
            .add_order(order.clone(), None, Some(client_id()), false)
            .unwrap();
        let cmd = SubmitOrder::from_order(
            order,
            trader_id(),
            Some(client_id()),
            None,
            UUID4::new(),
            UnixNanos::default(),
        );
        self.client.submit_order(cmd).unwrap();
    }

    /// Submits and returns the venue order id of the `OrderAccepted` event.
    async fn submit_accepted(&mut self, order: &OrderAny) -> VenueOrderId {
        self.submit(order);
        assert!(matches!(
            self.next_order_event().await,
            OrderEventAny::Submitted(_)
        ));
        match self.next_order_event().await {
            OrderEventAny::Accepted(event) => event.venue_order_id,
            other => panic!("expected OrderAccepted, was {other:?}"),
        }
    }

    fn cancel(&self, order: &OrderAny) {
        let cmd = CancelOrder::new(
            trader_id(),
            Some(client_id()),
            order.strategy_id(),
            order.instrument_id(),
            order.client_order_id(),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        );
        self.client.cancel_order(cmd).unwrap();
    }

    /// Pushes an account frame and expects it as the next event: nothing else was pending.
    async fn assert_no_pending_events(&mut self) {
        self.venue.push(&ws_fixture("account"));
        let event = next_event(&mut self.events).await;
        assert!(
            matches!(event, ExecutionEvent::Account(_)),
            "unexpected event {event:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_reads_domain_router_and_nonce_from_the_venue() {
    let h = Harness::connect().await;

    assert!(h.client.is_connected());
    let state = h.venue.state.lock();
    for path in ["/v1/markets", "/v1/auth/eip712-domain", "/v1/system/config"] {
        assert!(
            state.gets.iter().any(|get| get == path),
            "{path} was not read"
        );
    }
    assert_eq!(state.nonce_reads, 1);
    assert!(state.posts.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_is_accepted_then_filled() {
    let mut h = Harness::connect().await;
    let order = limit("O-1", "25", "0.50000");

    let venue_order_id = h.submit_accepted(&order).await;

    assert_eq!(venue_order_id.as_str(), order_id(1));
    let place = h.venue.posts("/v1/orders/place");
    assert_eq!(place.len(), 1);
    let sent = &place[0];
    assert_eq!(sent["market_id"], AERO_MARKET);
    assert_eq!(sent["size_steps"], 25);
    assert_eq!(sent["price_ticks"], 50_000);
    assert_eq!(sent["side"], 0);
    assert_eq!(sent["post_only"], false);
    assert_eq!(sent["client_order_id"], key(&order));

    h.venue.push(&fill_frame(&order, &venue_order_id, 1, "25"));
    let OrderEventAny::Filled(fill) = h.next_order_event().await else {
        panic!("expected OrderFilled");
    };
    assert_eq!(fill.client_order_id, order.client_order_id());
    assert_eq!(fill.venue_order_id, venue_order_id);
    assert_eq!(fill.last_qty, Quantity::from("25"));
    assert_eq!(fill.last_px, Price::from("0.50000"));
    assert_eq!(fill.liquidity_side, LiquiditySide::Maker);
    // A negative fee is a maker rebate.
    assert_eq!(fill.commission, Some(Money::from("-0.000600 USDC")));
}

#[tokio::test(flavor = "multi_thread")]
async fn market_order_goes_as_ioc_capped_by_quote_and_slippage() {
    let mut h = Harness::connect().await;
    let quote = QuoteTick::new(
        aero(),
        Price::from("0.55290"),
        Price::from("0.55300"),
        Quantity::from("1000"),
        Quantity::from("1000"),
        UnixNanos::default(),
        UnixNanos::default(),
    );
    h.cache.borrow_mut().add_quote(quote).unwrap();
    let order = OrderTestBuilder::new(OrderType::Market)
        .instrument_id(aero())
        .client_order_id(ClientOrderId::from("O-1"))
        .side(OrderSide::Buy)
        .quantity(Quantity::from("25"))
        .build();

    h.submit_accepted(&order).await;

    let sent = &h.venue.posts("/v1/orders/place")[0];
    // 0.55300 * 1.005 = 0.555765, rounded up to the tick.
    assert_eq!(sent["price_ticks"], 55_577);
    assert_eq!(sent["time_in_force"], 3);
    assert_eq!(sent["post_only"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn submit_refused_by_the_venue_is_rejected() {
    let mut h = Harness::connect().await;
    h.venue.state.lock().reject_place = Some(("InsufficientMargin", "not enough margin"));
    let order = limit("O-1", "25", "0.50000");

    h.submit(&order);

    assert!(matches!(
        h.next_order_event().await,
        OrderEventAny::Submitted(_)
    ));
    let OrderEventAny::Rejected(rejected) = h.next_order_event().await else {
        panic!("expected OrderRejected");
    };
    assert_eq!(rejected.client_order_id, order.client_order_id());
    assert!(
        rejected.reason.contains("InsufficientMargin"),
        "{}",
        rejected.reason
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn mock_refuses_a_permit_of_another_signer() {
    // Guards the suite itself: the signer check of the mock must be able to fail.
    let mut h = Harness::connect_with(|config, _| {
        let other = PrivateKeySigner::random();
        config.signer_private_key = Some(alloy_primitives::hex::encode(other.to_bytes()));
    })
    .await;

    h.submit(&limit("O-1", "25", "0.50000"));

    assert!(matches!(
        h.next_order_event().await,
        OrderEventAny::Submitted(_)
    ));
    let OrderEventAny::Rejected(rejected) = h.next_order_event().await else {
        panic!("expected OrderRejected");
    };
    assert!(
        rejected.reason.contains("not the session signer"),
        "{}",
        rejected.reason
    );
    h.venue.state.lock().failures.clear();
}

#[tokio::test(flavor = "multi_thread")]
async fn orders_off_the_tick_or_step_grid_are_denied() {
    let mut h = Harness::connect().await;

    for (id, quantity, price, reason) in [
        ("O-1", "25", "0.500001", "not a multiple"),
        ("O-2", "25.5", "0.50000", "not a multiple"),
        ("O-3", "10", "0.50000", "below the minimum"),
    ] {
        h.submit(&limit(id, quantity, price));
        let OrderEventAny::Denied(denied) = h.next_order_event().await else {
            panic!("expected OrderDenied for {id}");
        };
        assert!(denied.reason.contains(reason), "{}", denied.reason);
    }

    // Nothing was rounded and sent: the only place request is the valid order.
    h.submit_accepted(&limit("O-4", "25", "0.50000")).await;
    assert_eq!(h.venue.posts("/v1/orders/place").len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_signs_the_resting_order_id() {
    let mut h = Harness::connect().await;
    let order = limit("O-1", "25", "0.50000");
    let venue_order_id = h.submit_accepted(&order).await;

    h.cancel(&order);

    let cancels = h.venue.wait_posts("/v1/orders/cancel", 1).await;
    assert_eq!(cancels[0]["order_id"], venue_order_id.as_str());
    assert_eq!(cancels[0]["market_id"], AERO_MARKET);

    let frame = orders_frame(&order, &venue_order_id, "0", "ORDER_STATUS_CANCELLED");
    h.venue.push(&frame);
    let OrderEventAny::Canceled(canceled) = h.next_order_event().await else {
        panic!("expected OrderCanceled");
    };
    assert_eq!(canceled.client_order_id, order.client_order_id());
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_all_posts_one_request_for_the_market() {
    let h = Harness::connect().await;
    let cmd = CancelAllOrders::new(
        trader_id(),
        Some(client_id()),
        limit("O-1", "25", "0.50000").strategy_id(),
        aero(),
        None,
        UUID4::new(),
        UnixNanos::default(),
        None,
        None,
    );

    h.client.cancel_all_orders(cmd).unwrap();

    let posts = h.venue.wait_posts("/v1/orders/cancel-all", 1).await;
    assert_eq!(posts[0]["market_id"], AERO_MARKET);
}

#[tokio::test(flavor = "multi_thread")]
async fn modify_cancels_then_places_under_the_same_client_order_id() {
    let mut h = Harness::connect().await;
    let order = limit("O-1", "25", "0.50000");
    let old = h.submit_accepted(&order).await;
    let cmd = ModifyOrder::new(
        trader_id(),
        Some(client_id()),
        order.strategy_id(),
        aero(),
        order.client_order_id(),
        Some(old),
        None,
        Some(Price::from("0.51000")),
        None,
        UUID4::new(),
        UnixNanos::default(),
        None,
        None,
    );

    h.client.modify_order(cmd).unwrap();

    let OrderEventAny::Updated(updated) = h.next_order_event().await else {
        panic!("expected OrderUpdated");
    };
    let new = updated.venue_order_id.unwrap();
    assert_eq!(new.as_str(), order_id(2));
    assert_eq!(updated.price, Some(Price::from("0.51000")));
    assert_eq!(
        h.venue.posts("/v1/orders/cancel")[0]["order_id"],
        old.as_str()
    );
    let place = h.venue.posts("/v1/orders/place");
    assert_eq!(place[1]["price_ticks"], 51_000);
    assert_eq!(place[1]["client_order_id"], key(&order));

    // The cancel row of the old id must not close the order: the fill of the new id counts.
    h.venue
        .push(&orders_frame(&order, &old, "0", "ORDER_STATUS_CANCELLED"));
    h.venue.push(&fill_frame(&order, &new, 1, "25"));
    let OrderEventAny::Filled(fill) = h.next_order_event().await else {
        panic!("expected OrderFilled, not a cancel of the replaced order");
    };
    assert_eq!(fill.venue_order_id, new);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_nonce_anchor_resyncs_and_retries_once() {
    let mut h = Harness::connect().await;
    h.venue.state.lock().stale_nonce_once = true;

    h.submit_accepted(&limit("O-1", "25", "0.50000")).await;

    // The mock refuses a nonce that is used twice, so the retry signed a new one.
    assert_eq!(h.venue.posts("/v1/orders/place").len(), 2);
    assert_eq!(h.venue.state.lock().nonce_reads, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn partial_fill_keeps_the_order_open() {
    let mut h = Harness::connect().await;
    let order = limit("O-1", "40", "0.50000");
    let venue_order_id = h.submit_accepted(&order).await;

    h.venue.push(&fill_frame(&order, &venue_order_id, 1, "15"));
    // The venue keeps a partly filled order `OPEN` with a filled size.
    h.venue.push(&orders_frame(
        &order,
        &venue_order_id,
        "15",
        "ORDER_STATUS_OPEN",
    ));
    h.venue.push(&fill_frame(&order, &venue_order_id, 2, "25"));

    for expected in ["15", "25"] {
        let OrderEventAny::Filled(fill) = h.next_order_event().await else {
            panic!("expected OrderFilled of {expected}");
        };
        assert_eq!(fill.last_qty, Quantity::from(expected));
    }
    h.assert_no_pending_events().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_does_not_repeat_fills() {
    let mut h = Harness::connect().await;
    let order = limit("O-1", "40", "0.50000");
    let venue_order_id = h.submit_accepted(&order).await;
    h.venue.push(&fill_frame(&order, &venue_order_id, 1, "15"));
    let OrderEventAny::Filled(first) = h.next_order_event().await else {
        panic!("expected OrderFilled");
    };

    // While the socket is down the venue fills the rest; REST has both trades.
    h.venue.state.lock().trades = Some(json!([
        rest_trade(&order, &venue_order_id, 2, "25"),
        rest_trade(&order, &venue_order_id, 1, "15"),
    ]));
    h.venue.drop_connection.notify_one();
    h.venue.wait_subscriptions(6).await;

    let ExecutionEvent::Report(ExecutionReport::Fill(missed)) = next_event(&mut h.events).await
    else {
        panic!("expected the fill report of the missed trade");
    };
    assert_ne!(missed.trade_id, first.trade_id);
    assert_eq!(missed.last_qty, Quantity::from("25"));
    assert_eq!(missed.client_order_id, Some(order.client_order_id()));

    // The channel sends both again: neither is a new trade.
    h.venue.push(&fill_frame(&order, &venue_order_id, 1, "15"));
    h.venue.push(&fill_frame(&order, &venue_order_id, 2, "25"));
    h.assert_no_pending_events().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn trigger_order_uses_the_signed_tpsl_endpoints() {
    let mut h = Harness::connect().await;
    let order = OrderTestBuilder::new(OrderType::StopMarket)
        .instrument_id(aero())
        .client_order_id(ClientOrderId::from("O-1"))
        .side(OrderSide::Sell)
        .quantity(Quantity::from("25"))
        .trigger_price(Price::from("0.40000"))
        .reduce_only(true)
        .build();

    let venue_order_id = h.submit_accepted(&order).await;

    assert_eq!(venue_order_id.as_str(), TPSL_ORDER_ID);
    let sent = &h.venue.posts("/v1/orders/tpsl")[0];
    assert_eq!(sent["market_id"], "15");
    assert_eq!(sent["side"], 1);
    assert_eq!(sent["size"], "25");
    assert_eq!(sent["stop_type"], 1);
    assert_eq!(sent["stop_price"], "0.4");
    assert!(h.venue.posts("/v1/orders/place").is_empty());

    h.cancel(&order);
    let cancels = h.venue.wait_posts("/v1/orders/tpsl/cancel", 1).await;
    assert_eq!(cancels[0]["order_id"], TPSL_ORDER_ID);
}

#[tokio::test(flavor = "multi_thread")]
async fn margin_mode_and_leverage_are_set_at_connect() {
    let h = Harness::connect_with(|config, _| {
        config.leverage.insert(aero(), 3);
        config.margin_mode.insert(aero(), RisexMarginMode::Isolated);
    })
    .await;

    let state = h.venue.state.lock();
    let paths: Vec<&str> = state.posts.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(paths, ["/v1/account/margin-mode", "/v1/account/leverage"]);
    assert_eq!(state.posts[0].1["market_id"], "15");
    assert_eq!(state.posts[0].1["margin_mode"], 1);
    assert_eq!(state.posts[1].1["leverage"], "3");
}

#[tokio::test(flavor = "multi_thread")]
async fn account_state_comes_from_rest_then_from_the_channel() {
    let mut h = Harness::connect().await;

    let rest = h.rest_account.balances[0];
    assert!(
        (rest.total.as_f64() - 1_179_950.284_107).abs() < 1e-3,
        "{rest:?}"
    );

    h.venue.push(&ws_fixture("account"));
    let ExecutionEvent::Account(state) = next_event(&mut h.events).await else {
        panic!("expected an account state");
    };
    let balance = state.balances[0];
    assert!(
        (balance.total.as_f64() - 1_178_044.592_563).abs() < 1e-3,
        "{balance:?}"
    );
    assert!(
        (balance.free.as_f64() - 167_250.536_706).abs() < 1e-3,
        "{balance:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_account_subscription_disconnects() {
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    replace_exec_event_sender(sender);
    let signer = PrivateKeySigner::random();
    let (venue, http_url, ws_url) = Venue::start(signer.address()).await;
    venue.state.lock().refuse_subscriptions = true;
    let config = RisexExecClientConfig {
        base_url_http: Some(http_url),
        base_url_ws: Some(ws_url),
        account_address: Some(ACCOUNT.to_string()),
        signer_private_key: Some(alloy_primitives::hex::encode(signer.to_bytes())),
        ..Default::default()
    };
    let core = ExecutionClientCore::new(
        trader_id(),
        client_id(),
        *RISEX_VENUE,
        OmsType::Netting,
        config.account_id,
        AccountType::Margin,
        None,
        Rc::new(RefCell::new(Cache::default())),
    );
    let mut client = RisexExecutionClient::new(core, config).unwrap();
    client.start().unwrap();

    // Without the account channels fills would be lost silently: the client stops instead.
    let _ = client.connect().await;
    wait_until("the disconnect", || !client.is_connected()).await;
    assert!(matches!(
        events.recv().await,
        Some(ExecutionEvent::Account(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn reconciliation_reports_come_from_the_rest_fixtures() {
    let h = Harness::connect().await;
    let ts = UnixNanos::default();

    // 20 recorded orders and 9 recorded (canceled, never triggered) trigger orders.
    let all =
        GenerateOrderStatusReports::new(UUID4::new(), ts, false, None, None, None, None, None);
    assert_eq!(
        h.client
            .generate_order_status_reports(&all)
            .await
            .unwrap()
            .len(),
        29
    );

    let open =
        GenerateOrderStatusReports::new(UUID4::new(), ts, true, None, None, None, None, None);
    let reports = h.client.generate_order_status_reports(&open).await.unwrap();
    assert_eq!(reports.len(), 635);

    let fills = GenerateFillReports::new(UUID4::new(), ts, None, None, None, None, None, None);
    assert_eq!(
        h.client.generate_fill_reports(fills).await.unwrap().len(),
        20
    );

    let positions =
        GeneratePositionStatusReports::new(UUID4::new(), ts, None, None, None, None, None);
    let reports = h
        .client
        .generate_position_status_reports(&positions)
        .await
        .unwrap();
    assert_eq!(reports.len(), 53);

    let position =
        GeneratePositionStatusReports::new(UUID4::new(), ts, Some(aero()), None, None, None, None);
    let reports = h
        .client
        .generate_position_status_reports(&position)
        .await
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].quantity, Quantity::from("99299"));

    let recorded = fixture("orders_history")["data"]["orders"][0].clone();
    let id = VenueOrderId::new(recorded["id"].as_str().unwrap());
    let single = GenerateOrderStatusReport::new(UUID4::new(), ts, None, None, Some(id), None, None);
    // unverified: that the venue answers 404 for an unknown order id.
    assert!(
        h.client
            .generate_order_status_report(&single)
            .await
            .unwrap()
            .is_none()
    );
    h.venue.state.lock().order = Some(recorded);
    let report = h
        .client
        .generate_order_status_report(&single)
        .await
        .unwrap();
    assert_eq!(report.unwrap().venue_order_id, id);
}

#[tokio::test(flavor = "multi_thread")]
async fn position_reports_come_from_the_positions_channel() {
    let mut h = Harness::connect().await;

    // Recorded: the BTC position of the account after a fill.
    let mut frame = ws_fixture("positions");
    h.venue.push(&frame);
    let ExecutionEvent::Report(ExecutionReport::Position(report)) = next_event(&mut h.events).await
    else {
        panic!("expected a position report");
    };
    assert_eq!(report.instrument_id.to_string(), "BTC-USDC-PERP.RISEX");
    assert_eq!(report.position_side, PositionSide::Long);
    assert_eq!(report.quantity, Quantity::from("4.207594"));

    // Hand-made from the docs: a closed position has size "0" and keeps its side.
    frame["data"][0]["size"] = json!("0");
    // The position of another account on the same frame is not reported.
    let mut other = frame["data"][0].clone();
    other["account"] = json!("0x0000000000000000000000000000000000000001");
    other["size"] = json!("7");
    frame["data"].as_array_mut().unwrap().insert(0, other);
    h.venue.push(&frame);
    let ExecutionEvent::Report(ExecutionReport::Position(report)) = next_event(&mut h.events).await
    else {
        panic!("expected a position report");
    };
    assert_eq!(report.position_side, PositionSide::Flat);
    h.assert_no_pending_events().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_positions_subscription_keeps_the_client_connected() {
    let mut h = Harness::connect_with(|_, venue| venue.state.lock().refuse_positions = true).await;

    // The refusal was read before this frame: the client still consumes the socket.
    h.assert_no_pending_events().await;
    assert!(h.client.is_connected());
}

#[tokio::test(flavor = "multi_thread")]
async fn leverage_and_margin_mode_can_be_set_at_run_time() {
    let h = Harness::connect().await;

    h.client
        .update_margin_mode(aero(), RisexMarginMode::Isolated)
        .unwrap();
    let sent = h.venue.wait_posts("/v1/account/margin-mode", 1).await;
    assert_eq!(sent[0]["market_id"], "15");
    assert_eq!(sent[0]["margin_mode"], 1);

    h.client.update_leverage(aero(), 3).unwrap();
    let sent = h.venue.wait_posts("/v1/account/leverage", 1).await;
    assert_eq!(sent[0]["market_id"], "15");
    assert_eq!(sent[0]["leverage"], "3");

    assert!(h.client.update_leverage(aero(), 0).is_err());
    let unknown = InstrumentId::from("NOPE-USDC-PERP.RISEX");
    assert!(h.client.update_leverage(unknown, 3).is_err());
    assert!(
        h.client
            .update_margin_mode(unknown, RisexMarginMode::Cross)
            .is_err()
    );
    assert_eq!(h.venue.state.lock().posts.len(), 2);
}
