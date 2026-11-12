//! `RisexDataClient` against a local mock of the venue (REST and WebSocket). No network.

use std::{
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    routing::get,
};
use nautilus_common::{
    clients::DataClient,
    live::runner::replace_data_event_sender,
    messages::{
        DataEvent,
        data::{
            DataResponse, RequestBookSnapshot, SubscribeBookDeltas, SubscribeInstrumentStatus,
            UnsubscribeBookDeltas,
        },
    },
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::{
    data::Data,
    enums::{BookAction, BookType, MarketStatusAction},
    identifiers::{ClientId, InstrumentId},
};
use nautilus_risex::{config::data::RisexDataClientConfig, data::RisexDataClient};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;

const MARKETS: &str = include_str!("fixtures/http/markets.json");
const SYSTEM_CONFIG: &str = include_str!("fixtures/http/system_config.json");
const ORDERBOOK: &str = include_str!("fixtures/http/orderbook_aero.json");
const WS_FRAMES: &str = include_str!("fixtures/ws/public_aero_btc.jsonl");
/// Lines of the AERO book in `WS_FRAMES`.
const AERO_SNAPSHOT: usize = 1;
const AERO_UPDATE: usize = 5;

/// Requests the client sent on the WebSocket, as `(method, channel)`.
type Requests = Arc<Mutex<Vec<(String, String)>>>;

fn ws_frame(line: usize) -> Value {
    serde_json::from_str(WS_FRAMES.lines().nth(line).unwrap()).unwrap()
}

/// Answers every `orderbook` subscribe with an ack and the book snapshot. The first subscribe
/// is followed by an update whose checksum cannot match.
async fn serve_ws(mut socket: WebSocket, requests: Requests) {
    while let Some(Ok(message)) = socket.recv().await {
        let Message::Text(text) = message else {
            continue;
        };
        let request: Value = serde_json::from_str(&text).unwrap();
        let method = request["method"].as_str().unwrap().to_string();
        if method == "ping" {
            continue;
        }
        let channel = request["params"]["channel"].as_str().unwrap().to_string();
        let first = {
            let mut requests = requests.lock().unwrap();
            requests.push((method.clone(), channel.clone()));
            requests.len() == 1
        };
        if method != "subscribe" {
            continue;
        }

        let ack = json!({
            "type": "subscribed", "method": "subscribe", "status": "success", "message": "ok",
            "channel": channel, "data": {"market_ids": [15]},
        });
        let mut frames = vec![ack, ws_frame(AERO_SNAPSHOT)];
        if first {
            let mut update = ws_frame(AERO_UPDATE);
            update["checksum"] = json!(update["checksum"].as_u64().unwrap() ^ 1);
            frames.push(update);
        }
        for frame in frames {
            socket
                .send(Message::Text(frame.to_string().into()))
                .await
                .unwrap();
        }
    }
}

/// The venue serves `markets` as `GET /v1/markets`: a test changes it to change a market.
async fn start_venue() -> (RisexDataClientConfig, Requests, Arc<Mutex<Value>>) {
    let requests = Requests::default();
    let markets = Arc::new(Mutex::new(serde_json::from_str::<Value>(MARKETS).unwrap()));
    let served = markets.clone();
    let app = Router::new()
        .route(
            "/v1/markets",
            get(move || async move { Json(served.lock().unwrap().clone()) }),
        )
        .route(
            "/v1/system/config",
            get(|| async { Json(serde_json::from_str::<Value>(SYSTEM_CONFIG).unwrap()) }),
        )
        .route(
            "/v1/orderbook",
            get(|| async { Json(serde_json::from_str::<Value>(ORDERBOOK).unwrap()) }),
        )
        .route(
            "/ws",
            get(
                |ws: WebSocketUpgrade, State(requests): State<Requests>| async move {
                    ws.on_upgrade(|socket| serve_ws(socket, requests))
                },
            ),
        )
        .with_state(requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let config = RisexDataClientConfig {
        base_url_http: Some(format!("http://{addr}")),
        base_url_ws: Some(format!("ws://{addr}/ws")),
        instrument_status_poll_interval_secs: 1,
        ..Default::default()
    };
    (config, requests, markets)
}

async fn next_event(events: &mut UnboundedReceiver<DataEvent>) -> DataEvent {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("no data event within 5 s")
        .expect("data event channel closed")
}

/// Returns the next book deltas and fails on any other event.
async fn next_snapshot(events: &mut UnboundedReceiver<DataEvent>, instrument_id: InstrumentId) {
    match next_event(events).await {
        DataEvent::Data(Data::BookDeltas(deltas)) => {
            assert_eq!(deltas.instrument_id, instrument_id);
            assert_eq!(deltas.deltas[0].action, BookAction::Clear);
        }
        other => panic!("expected a book snapshot, was {other:?}"),
    }
}

async fn wait_for_requests(requests: &Requests, count: usize) -> Vec<(String, String)> {
    for _ in 0..500 {
        let seen = requests.lock().unwrap().clone();
        if seen.len() >= count {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "venue saw {:?}, expected {count} requests",
        requests.lock().unwrap()
    );
}

#[tokio::test]
async fn connect_subscribe_desync_resubscribe_request_and_disconnect() {
    let (config, requests, _) = start_venue().await;
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    replace_data_event_sender(sender);
    let client_id = ClientId::from("RISEX");
    let aero = InstrumentId::from("AERO-USDC-PERP.RISEX");
    let mut client = RisexDataClient::new(client_id, config).unwrap();

    // Connect loads every active market of the fixture
    client.connect().await.unwrap();
    assert!(client.is_connected());
    for _ in 0..53 {
        assert!(matches!(
            next_event(&mut events).await,
            DataEvent::Data(Data::Instrument(_))
        ));
    }

    // The snapshot arrives, the bad update is dropped and the client asks for the book
    // again: unsubscribe, then subscribe. Depth and quotes are not subscribed, so the
    // deltas are the only data.
    client
        .subscribe_book_deltas(SubscribeBookDeltas::new(
            aero,
            BookType::L2_MBP,
            Some(client_id),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            false,
            None,
            None,
        ))
        .unwrap();
    next_snapshot(&mut events, aero).await;
    next_snapshot(&mut events, aero).await;
    let orderbook = |method: &str| (method.to_string(), "orderbook".to_string());
    assert_eq!(
        wait_for_requests(&requests, 3).await,
        [
            orderbook("subscribe"),
            orderbook("unsubscribe"),
            orderbook("subscribe")
        ]
    );

    client
        .request_book_snapshot(RequestBookSnapshot::new(
            aero,
            None,
            Some(client_id),
            UUID4::new(),
            UnixNanos::default(),
            None,
        ))
        .unwrap();
    match next_event(&mut events).await {
        DataEvent::Response(DataResponse::Book(response)) => {
            let fixture: Value = serde_json::from_str(ORDERBOOK).unwrap();
            let best_bid = fixture["data"]["bids"][0]["price"].as_str().unwrap();
            assert_eq!(response.instrument_id, aero);
            assert_eq!(
                response.data.best_bid_price().unwrap().as_decimal(),
                Decimal::from_str(best_bid).unwrap()
            );
            assert!(
                response.data.best_ask_price().unwrap() > response.data.best_bid_price().unwrap()
            );
        }
        other => panic!("expected a book response, was {other:?}"),
    }

    client
        .unsubscribe_book_deltas(&UnsubscribeBookDeltas::new(
            aero,
            Some(client_id),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        ))
        .unwrap();
    assert_eq!(
        wait_for_requests(&requests, 4).await[3],
        orderbook("unsubscribe")
    );

    client.disconnect().await.unwrap();
    assert!(client.is_disconnected());
}

#[tokio::test]
async fn instrument_status_is_sent_on_subscribe_and_on_change() {
    let (config, _, markets) = start_venue().await;
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    replace_data_event_sender(sender);
    let client_id = ClientId::from("RISEX");
    let aero = InstrumentId::from("AERO-USDC-PERP.RISEX");
    let mut client = RisexDataClient::new(client_id, config).unwrap();
    client.connect().await.unwrap();
    for _ in 0..53 {
        next_event(&mut events).await;
    }

    client
        .subscribe_instrument_status(SubscribeInstrumentStatus::new(
            aero,
            Some(client_id),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        ))
        .unwrap();
    let DataEvent::Data(Data::InstrumentStatus(status)) = next_event(&mut events).await else {
        panic!("expected the current status");
    };
    assert_eq!(status.instrument_id, aero);
    assert_eq!(status.action, MarketStatusAction::Trading);
    assert_eq!(status.reason, None);
    assert_eq!(status.is_trading, Some(true));

    // Hand-made change: the recorded AERO market is never post-only. Polls that see no
    // change in between send nothing, so the next event is the change.
    {
        let mut body = markets.lock().unwrap();
        let rows = body["data"]["markets"].as_array_mut().unwrap();
        let row = rows.iter_mut().find(|m| m["market_id"] == "15").unwrap();
        row["post_only"] = json!(true);
    }
    let DataEvent::Data(Data::InstrumentStatus(status)) = next_event(&mut events).await else {
        panic!("expected the changed status");
    };
    assert_eq!(status.action, MarketStatusAction::Pause);
    assert_eq!(status.reason.unwrap().as_str(), "post_only");
    assert_eq!(status.is_trading, Some(false));
    assert_eq!(status.is_quoting, Some(true));

    client.disconnect().await.unwrap();
}
