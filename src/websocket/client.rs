//! RiseX WebSocket transport: connection, heartbeat, reconnect and subscription state.

use std::{collections::BTreeMap, fmt::Debug, sync::Arc};

use nautilus_common::live::get_runtime;
use nautilus_network::{
    RECONNECTED,
    websocket::{WebSocketClient, WebSocketConfig, channel_message_handler},
};
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::Message;

use super::{
    enums::RisexWsChannel,
    error::RisexWsError,
    messages::{RisexWsMessage, RisexWsRequest, RisexWsSubscription},
};

/// The venue closes a connection after 60 s without inbound traffic; any frame resets that.
const HEARTBEAT_INTERVAL_SECS: u64 = 20;
const HEARTBEAT_PAYLOAD: &str = r#"{"method":"ping"}"#;
const RECONNECT_DELAY_INITIAL_MS: u64 = 250;
const RECONNECT_DELAY_MAX_MS: u64 = 30_000;
const RECONNECT_BACKOFF_FACTOR: f64 = 2.0;
const RECONNECT_JITTER_MS: u64 = 200;

/// The subscriptions the caller wants, re-sent in full after every reconnect.
type Subscriptions = Arc<Mutex<BTreeMap<RisexWsChannel, RisexWsSubscription>>>;

/// WebSocket client for the RiseX feed (no authentication: account channels are
/// read through the `makers` filter).
///
/// The venue replaces a channel's filters on every subscribe, so the client keeps
/// the whole wanted filter set per channel and always sends all of it.
/// Subscribing before [`Self::connect`] only records the subscription.
/// A subscribe whose send fails stays recorded and is sent on the next reconnect.
pub struct RisexWebSocketClient {
    url: String,
    subscriptions: Subscriptions,
    client: Option<Arc<WebSocketClient>>,
    task: Option<JoinHandle<()>>,
}

impl Debug for RisexWebSocketClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct(stringify!(RisexWebSocketClient))
            .field("url", &self.url)
            .field("active", &self.is_active())
            .finish_non_exhaustive()
    }
}

impl RisexWebSocketClient {
    /// `url`: mainnet `wss://ws.rise.trade/ws`, testnet `wss://api.testnet.rise.trade/ws/`.
    #[must_use]
    pub fn new(url: String) -> Self {
        Self {
            url,
            subscriptions: Subscriptions::default(),
            client: None,
            task: None,
        }
    }

    /// Connects and returns the stream of inbound frames.
    ///
    /// After each automatic reconnect the stream yields [`RisexWsMessage::Reconnected`]
    /// and every recorded subscription is sent again.
    ///
    /// # Errors
    ///
    /// Returns an error if the URL is invalid or the connection cannot be established.
    pub async fn connect(
        &mut self,
    ) -> Result<mpsc::UnboundedReceiver<RisexWsMessage>, RisexWsError> {
        self.disconnect().await;

        let config = WebSocketConfig::builder()
            .url(self.url.clone())
            .heartbeat_interval_secs(HEARTBEAT_INTERVAL_SECS)
            .heartbeat_payload(HEARTBEAT_PAYLOAD.to_string())
            .reconnect_delay_initial_ms(RECONNECT_DELAY_INITIAL_MS)
            .reconnect_delay_max_ms(RECONNECT_DELAY_MAX_MS)
            .reconnect_backoff_factor(RECONNECT_BACKOFF_FACTOR)
            .reconnect_jitter_ms(RECONNECT_JITTER_MS)
            .build()
            .map_err(|e| RisexWsError::Client(e.to_string()))?;
        let (handler, raw_rx) = channel_message_handler();
        let client = WebSocketClient::builder()
            .config(config)
            .message_handler(handler)
            .connect()
            .await
            .map_err(|e| RisexWsError::Network(e.to_string()))?;
        let client = Arc::new(client);

        // Subscriptions recorded before connecting. The read task starts after
        // them so a reconnect cannot interleave with this first send.
        if let Err(e) = send_subscriptions(&client, &self.subscriptions).await {
            client.disconnect().await;
            return Err(e);
        }
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        self.client = Some(client.clone());
        self.task = Some(get_runtime().spawn(read_loop(
            raw_rx,
            out_tx,
            client,
            self.subscriptions.clone(),
        )));

        Ok(out_rx)
    }

    /// Closes the connection. Recorded subscriptions are kept for the next [`Self::connect`].
    pub async fn disconnect(&mut self) {
        if let Some(client) = self.client.take() {
            client.disconnect().await;
        }
        // The read task holds the client, which holds the task's input sender,
        // so the task never ends on its own.
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    /// Whether the connection is up (false while reconnecting).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.client.as_ref().is_some_and(|c| c.is_active())
    }

    /// # Errors
    ///
    /// Returns an error if `market_ids` is empty or the request cannot be sent.
    pub async fn subscribe_orderbook(&self, market_ids: &[u32]) -> Result<(), RisexWsError> {
        self.subscribe_markets(RisexWsChannel::Orderbook, market_ids, None)
            .await
    }

    /// # Errors
    ///
    /// Returns an error if `market_ids` is empty or the request cannot be sent.
    pub async fn subscribe_trades(&self, market_ids: &[u32]) -> Result<(), RisexWsError> {
        self.subscribe_markets(RisexWsChannel::Trades, market_ids, None)
            .await
    }

    /// Index and mark prices.
    ///
    /// # Errors
    ///
    /// Returns an error if `market_ids` is empty or the request cannot be sent.
    pub async fn subscribe_oracle(&self, market_ids: &[u32]) -> Result<(), RisexWsError> {
        self.subscribe_markets(RisexWsChannel::Oracle, market_ids, None)
            .await
    }

    /// # Errors
    ///
    /// Returns an error if `market_ids` is empty or the request cannot be sent.
    pub async fn subscribe_expected_funding_rate(
        &self,
        market_ids: &[u32],
    ) -> Result<(), RisexWsError> {
        self.subscribe_markets(RisexWsChannel::ExpectedFundingRate, market_ids, None)
            .await
    }

    /// Funding payments of every account on these markets; filter on `account`.
    ///
    /// # Errors
    ///
    /// Returns an error if `market_ids` is empty or the request cannot be sent.
    pub async fn subscribe_funding(&self, market_ids: &[u32]) -> Result<(), RisexWsError> {
        self.subscribe_markets(RisexWsChannel::Funding, market_ids, None)
            .await
    }

    /// Maintenance and market warnings.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn subscribe_system(&self) -> Result<(), RisexWsError> {
        self.subscribe(RisexWsChannel::System, &[], None).await
    }

    /// # Errors
    ///
    /// Returns an error if `market_ids` is empty or the request cannot be sent.
    pub async fn subscribe_orders(
        &self,
        market_ids: &[u32],
        account: &str,
    ) -> Result<(), RisexWsError> {
        self.subscribe_markets(RisexWsChannel::Orders, market_ids, Some(account))
            .await
    }

    /// # Errors
    ///
    /// Returns an error if `market_ids` is empty or the request cannot be sent.
    pub async fn subscribe_fills(
        &self,
        market_ids: &[u32],
        account: &str,
    ) -> Result<(), RisexWsError> {
        self.subscribe_markets(RisexWsChannel::Fills, market_ids, Some(account))
            .await
    }

    /// # Errors
    ///
    /// Returns an error if `market_ids` is empty or the request cannot be sent.
    pub async fn subscribe_positions(
        &self,
        market_ids: &[u32],
        account: &str,
    ) -> Result<(), RisexWsError> {
        self.subscribe_markets(RisexWsChannel::Positions, market_ids, Some(account))
            .await
    }

    /// Balances and margin of one account.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn subscribe_account(&self, account: &str) -> Result<(), RisexWsError> {
        self.subscribe(RisexWsChannel::Account, &[], Some(account))
            .await
    }

    /// Removes `market_ids` from the channel's filter; unsubscribes it when no market is left.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn unsubscribe_orderbook(&self, market_ids: &[u32]) -> Result<(), RisexWsError> {
        self.unsubscribe(RisexWsChannel::Orderbook, market_ids)
            .await
    }

    /// Removes `market_ids` from the channel's filter; unsubscribes it when no market is left.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn unsubscribe_trades(&self, market_ids: &[u32]) -> Result<(), RisexWsError> {
        self.unsubscribe(RisexWsChannel::Trades, market_ids).await
    }

    /// Removes `market_ids` from the channel's filter; unsubscribes it when no market is left.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn unsubscribe_oracle(&self, market_ids: &[u32]) -> Result<(), RisexWsError> {
        self.unsubscribe(RisexWsChannel::Oracle, market_ids).await
    }

    /// Removes `market_ids` from the channel's filter; unsubscribes it when no market is left.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn unsubscribe_expected_funding_rate(
        &self,
        market_ids: &[u32],
    ) -> Result<(), RisexWsError> {
        self.unsubscribe(RisexWsChannel::ExpectedFundingRate, market_ids)
            .await
    }

    /// Removes `market_ids` from the channel's filter; unsubscribes it when no market is left.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn unsubscribe_funding(&self, market_ids: &[u32]) -> Result<(), RisexWsError> {
        self.unsubscribe(RisexWsChannel::Funding, market_ids).await
    }

    /// Removes `market_ids` from the channel's filter; unsubscribes it when no market is left.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn unsubscribe_orders(&self, market_ids: &[u32]) -> Result<(), RisexWsError> {
        self.unsubscribe(RisexWsChannel::Orders, market_ids).await
    }

    /// Removes `market_ids` from the channel's filter; unsubscribes it when no market is left.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn unsubscribe_fills(&self, market_ids: &[u32]) -> Result<(), RisexWsError> {
        self.unsubscribe(RisexWsChannel::Fills, market_ids).await
    }

    /// Removes `market_ids` from the channel's filter; unsubscribes it when no market is left.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn unsubscribe_positions(&self, market_ids: &[u32]) -> Result<(), RisexWsError> {
        self.unsubscribe(RisexWsChannel::Positions, market_ids)
            .await
    }

    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn unsubscribe_system(&self) -> Result<(), RisexWsError> {
        self.unsubscribe(RisexWsChannel::System, &[]).await
    }

    /// # Errors
    ///
    /// Returns an error if the request cannot be sent.
    pub async fn unsubscribe_account(&self) -> Result<(), RisexWsError> {
        self.unsubscribe(RisexWsChannel::Account, &[]).await
    }

    /// Removes `market_ids` from a channel's filter and re-sends what is left;
    /// unsubscribes the channel when no market is left.
    async fn unsubscribe(
        &self,
        channel: RisexWsChannel,
        market_ids: &[u32],
    ) -> Result<(), RisexWsError> {
        let mut subscriptions = self.subscriptions.lock().await;
        let Some(subscription) = subscriptions.get_mut(&channel) else {
            return Ok(());
        };
        for market_id in market_ids {
            subscription.market_ids.remove(market_id);
        }
        let request = if subscription.market_ids.is_empty() {
            subscriptions.remove(&channel);
            RisexWsRequest::Unsubscribe { channel }
        } else {
            RisexWsRequest::Subscribe(subscription.clone())
        };
        match &self.client {
            Some(client) => send(client, &request).await,
            None => Ok(()),
        }
    }

    async fn subscribe_markets(
        &self,
        channel: RisexWsChannel,
        market_ids: &[u32],
        account: Option<&str>,
    ) -> Result<(), RisexWsError> {
        // The venue reads an empty filter as "every market".
        if market_ids.is_empty() {
            return Err(RisexWsError::Client(format!(
                "{channel:?}: market_ids is empty"
            )));
        }
        self.subscribe(channel, market_ids, account).await
    }

    async fn subscribe(
        &self,
        channel: RisexWsChannel,
        market_ids: &[u32],
        account: Option<&str>,
    ) -> Result<(), RisexWsError> {
        // Held across the send so concurrent subscribes reach the venue in the
        // order they were merged: the last frame sent is the full set.
        let mut subscriptions = self.subscriptions.lock().await;
        let subscription = subscriptions
            .entry(channel)
            .or_insert_with(|| RisexWsSubscription {
                channel,
                market_ids: Default::default(),
                makers: Default::default(),
            });
        subscription.market_ids.extend(market_ids);
        // The venue echoes makers in lower case; that is the form known to match.
        subscription.makers.extend(account.map(str::to_lowercase));
        match &self.client {
            Some(client) => send(client, &RisexWsRequest::Subscribe(subscription.clone())).await,
            None => Ok(()),
        }
    }
}

async fn send(client: &WebSocketClient, request: &RisexWsRequest) -> Result<(), RisexWsError> {
    let text = serde_json::to_string(request).map_err(|e| RisexWsError::Client(e.to_string()))?;
    Ok(client.send_text(text, None).await?)
}

async fn send_subscriptions(
    client: &WebSocketClient,
    subscriptions: &Subscriptions,
) -> Result<(), RisexWsError> {
    for subscription in subscriptions.lock().await.values() {
        send(client, &RisexWsRequest::Subscribe(subscription.clone())).await?;
    }
    Ok(())
}

async fn read_loop(
    mut raw_rx: mpsc::UnboundedReceiver<Message>,
    out_tx: mpsc::UnboundedSender<RisexWsMessage>,
    client: Arc<WebSocketClient>,
    subscriptions: Subscriptions,
) {
    while let Some(message) = raw_rx.recv().await {
        let Message::Text(text) = message else {
            continue;
        };
        let parsed = if text.as_str() == RECONNECTED {
            // Marker first: the consumer must drop its books before the new snapshots.
            if out_tx.send(RisexWsMessage::Reconnected).is_err() {
                return;
            }
            // A failure here means the connection dropped again; the next reconnect retries.
            if let Err(e) = send_subscriptions(&client, &subscriptions).await {
                log::warn!("RiseX resubscribe failed: {e}");
            }
            continue;
        } else {
            serde_json::from_str::<RisexWsMessage>(&text)
        };
        match parsed {
            Ok(message) => {
                if out_tx.send(message).is_err() {
                    return;
                }
            }
            Err(e) => log::warn!("RiseX frame is not JSON: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use axum::{
        Router,
        extract::{
            State,
            ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
        },
        response::Response,
        routing::get,
    };
    use serde_json::{Value, json};
    use tokio::sync::Notify;

    use super::*;
    use crate::websocket::enums::{RisexWsDataKind, RisexWsStatus};

    const ACCOUNT: &str = "0x36DC0D99dcaEE443CEbaA5Fb8c11877d157C26dF";
    const TRADE: &str = r#"{"channel":"trades","type":"update","market_id":"1","data":{"id":"a-b","maker_order_id":"0x1","taker_order_id":"0x2","maker":"0xa","taker":"0xb","maker_side":1,"price":"74690","size":"0.000213","fee_maker":"-0.0007954485","fee_taker":"0.004772691","fee_liquidation":"0"},"tx_hash":"0xc","block_number":7,"log_index":3,"worker_timestamp":"1794280485033622807"}"#;

    /// A venue stand-in: acks every subscribe (plus one trade on `trades`),
    /// pings each new connection, and drops the connection on demand.
    #[derive(Clone, Default)]
    struct MockVenue {
        connections: Arc<AtomicUsize>,
        /// (connection number, frame) for every text frame received.
        received: Arc<parking_lot::Mutex<Vec<(usize, Value)>>>,
        pongs: Arc<parking_lot::Mutex<Vec<Vec<u8>>>>,
        drop_connection: Arc<Notify>,
    }

    impl MockVenue {
        async fn start() -> (Self, String) {
            let venue = Self::default();
            let app = Router::new()
                .route(
                    "/ws",
                    get(
                        |ws: WebSocketUpgrade, State(venue): State<Self>| async move {
                            let response: Response = ws.on_upgrade(|socket| venue.serve(socket));
                            response
                        },
                    ),
                )
                .with_state(venue.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}/ws", listener.local_addr().unwrap());
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            (venue, url)
        }

        async fn serve(self, mut socket: WebSocket) {
            let connection = self.connections.fetch_add(1, Ordering::SeqCst) + 1;
            socket
                .send(WsMessage::Ping(b"venue".as_slice().into()))
                .await
                .unwrap();
            loop {
                let message = tokio::select! {
                    () = self.drop_connection.notified() => return,
                    message = socket.recv() => message,
                };
                match message {
                    Some(Ok(WsMessage::Text(text))) => {
                        let frame: Value = serde_json::from_str(&text).unwrap();
                        self.received.lock().push((connection, frame.clone()));
                        if frame["method"] != "subscribe" {
                            continue;
                        }
                        let mut data = frame["params"].clone();
                        let channel = data.as_object_mut().unwrap().remove("channel").unwrap();
                        let ack = json!({"type": "subscribed", "method": "subscribe", "status": "success", "message": "ok", "channel": channel, "data": data});
                        socket
                            .send(WsMessage::Text(ack.to_string().into()))
                            .await
                            .unwrap();
                        if channel == "trades" {
                            socket.send(WsMessage::Text(TRADE.into())).await.unwrap();
                        }
                    }
                    Some(Ok(WsMessage::Pong(payload))) => self.pongs.lock().push(payload.to_vec()),
                    Some(Ok(_)) => {}
                    _ => return,
                }
            }
        }

        /// Subscribe / unsubscribe frames received on one connection.
        fn requests(&self, connection: usize) -> Vec<Value> {
            self.received
                .lock()
                .iter()
                .filter(|(c, frame)| *c == connection && frame["method"] != "ping")
                .map(|(_, frame)| frame.clone())
                .collect()
        }
    }

    async fn next(rx: &mut mpsc::UnboundedReceiver<RisexWsMessage>) -> RisexWsMessage {
        tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timed out waiting for a frame")
            .expect("stream closed")
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

    #[tokio::test]
    async fn subscribe_delivers_ack_then_data() {
        let (_venue, url) = MockVenue::start().await;
        let mut client = RisexWebSocketClient::new(url);
        assert!(!client.is_active());
        let mut rx = client.connect().await.unwrap();
        assert!(client.is_active());

        client.subscribe_trades(&[1]).await.unwrap();

        let RisexWsMessage::Ack(ack) = next(&mut rx).await else {
            panic!("expected the ack first")
        };
        assert_eq!(ack.status, RisexWsStatus::Success);
        assert_eq!(ack.channel.as_deref(), Some("trades"));
        assert_eq!(ack.data.market_ids, [1]);
        let RisexWsMessage::Trades(trade) = next(&mut rx).await else {
            panic!("expected a trade")
        };
        assert_eq!(trade.kind, RisexWsDataKind::Update);
        assert_eq!(trade.data.price, "74690");

        client.disconnect().await;
        assert!(!client.is_active());
    }

    #[tokio::test]
    async fn server_ping_is_answered_with_pong() {
        let (venue, url) = MockVenue::start().await;
        let mut client = RisexWebSocketClient::new(url);
        let _rx = client.connect().await.unwrap();

        wait_until("pong", || !venue.pongs.lock().is_empty()).await;
        assert_eq!(venue.pongs.lock()[0], b"venue");

        client.disconnect().await;
    }

    #[tokio::test]
    async fn resubscribe_and_unsubscribe_send_the_whole_filter_set() {
        let (venue, url) = MockVenue::start().await;
        let mut client = RisexWebSocketClient::new(url);
        // Recorded before connecting, sent by connect.
        client.subscribe_orderbook(&[15]).await.unwrap();
        let _rx = client.connect().await.unwrap();

        client.subscribe_orderbook(&[1]).await.unwrap();
        client.unsubscribe_orderbook(&[15]).await.unwrap();
        client.unsubscribe_orderbook(&[1]).await.unwrap();
        assert!(client.subscribe_orderbook(&[]).await.is_err());

        wait_until("4 requests", || venue.requests(1).len() == 4).await;
        assert_eq!(
            venue.requests(1),
            [
                json!({"method": "subscribe", "params": {"channel": "orderbook", "market_ids": [15]}}),
                json!({"method": "subscribe", "params": {"channel": "orderbook", "market_ids": [1, 15]}}),
                json!({"method": "subscribe", "params": {"channel": "orderbook", "market_ids": [1]}}),
                json!({"method": "unsubscribe", "params": {"channel": "orderbook"}}),
            ]
        );

        client.disconnect().await;
    }

    #[tokio::test]
    async fn reconnect_resends_every_subscription() {
        let (venue, url) = MockVenue::start().await;
        let mut client = RisexWebSocketClient::new(url);
        let mut rx = client.connect().await.unwrap();

        client.subscribe_orderbook(&[1]).await.unwrap();
        client.subscribe_orderbook(&[15]).await.unwrap();
        client.subscribe_orders(&[1], ACCOUNT).await.unwrap();
        client.subscribe_account(ACCOUNT).await.unwrap();
        client.subscribe_system().await.unwrap();
        client.subscribe_trades(&[1]).await.unwrap();
        client.unsubscribe_trades(&[1]).await.unwrap();
        wait_until("7 requests", || venue.requests(1).len() == 7).await;

        venue.drop_connection.notify_one();

        loop {
            if next(&mut rx).await == RisexWsMessage::Reconnected {
                break;
            }
        }
        wait_until("4 resubscribes", || venue.requests(2).len() == 4).await;
        let account = ACCOUNT.to_lowercase();
        assert_eq!(
            venue.requests(2),
            [
                json!({"method": "subscribe", "params": {"channel": "orderbook", "market_ids": [1, 15]}}),
                json!({"method": "subscribe", "params": {"channel": "system"}}),
                json!({"method": "subscribe", "params": {"channel": "orders", "market_ids": [1], "makers": [account]}}),
                json!({"method": "subscribe", "params": {"channel": "account", "makers": [account]}}),
            ]
        );
        assert!(client.is_active());

        client.disconnect().await;
    }
}
