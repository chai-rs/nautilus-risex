//! Live market data client.
//!
//! One WebSocket feeds books, quotes, trades, mark and index prices and funding rates.
//! The venue has no bar channel: every bar subscription polls REST once per bar close.
//! Instrument status has no channel either: it is polled while one is subscribed.

use std::{future::Future, sync::Arc, time::Duration};

use ahash::AHashMap;
use anyhow::Context;
use dashmap::{DashMap, DashSet};
use nautilus_common::{
    cache::InstrumentLookupError,
    clients::{DataClient, execute_bar_subscriptions, execute_instrument_subscriptions},
    live::{get_runtime, runner::get_data_event_sender, sender::EventSender},
    messages::{
        DataEvent,
        data::{
            BarsResponse, BookResponse, DataResponse, FundingRatesResponse, InstrumentResponse,
            InstrumentsResponse, RequestBars, RequestBookSnapshot, RequestFundingRates,
            RequestInstrument, RequestInstruments, RequestTrades, SubscribeBars,
            SubscribeBookDeltas, SubscribeBookDepth, SubscribeFundingRates, SubscribeIndexPrices,
            SubscribeInstrument, SubscribeInstrumentStatus, SubscribeInstruments,
            SubscribeMarkPrices, SubscribeQuotes, SubscribeTrades, TradesResponse, UnsubscribeBars,
            UnsubscribeBookDeltas, UnsubscribeBookDepth, UnsubscribeFundingRates,
            UnsubscribeIndexPrices, UnsubscribeInstrument, UnsubscribeInstrumentStatus,
            UnsubscribeInstruments, UnsubscribeMarkPrices, UnsubscribeQuotes, UnsubscribeTrades,
        },
    },
};
use nautilus_core::{
    UnixNanos,
    datetime::datetime_to_unix_nanos,
    time::{AtomicTime, get_atomic_clock_realtime},
};
use nautilus_model::{
    data::{BarType, BookOrder, Data, InstrumentStatus},
    enums::{BookType, MarketStatusAction, OrderSide},
    identifiers::{ClientId, InstrumentId, Venue},
    instruments::{Instrument, InstrumentAny},
    orderbook::OrderBook,
};
use tokio::{sync::mpsc::UnboundedReceiver, task::JoinHandle};
use ustr::Ustr;

use crate::{
    book::{
        BookLevel,
        recovery::{RecoveryAction, RecoveryConfig},
    },
    common::{
        consts::{MAX_BOOK_LIMIT, RISEX_VENUE},
        enums::RisexBarInterval,
        urls::risex_ws_url,
    },
    config::data::RisexDataClientConfig,
    http::{
        client::RisexHttpClient,
        parse::{MarketStatus, parse_market_status},
    },
    websocket::{
        RisexWebSocketClient, RisexWsChannel, RisexWsError, RisexWsMessage,
        handler::{FeedEvent, FeedHandler},
    },
};

/// The venue closes a bar some time after the interval ends.
const BAR_POLL_DELAY: Duration = Duration::from_secs(2);
const RECONNECT_RETRY_DELAY: Duration = Duration::from_secs(5);

/// What a strategy subscribed to. Several kinds share one venue channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Deltas,
    Depth,
    Quotes,
    Trades,
    MarkPrices,
    IndexPrices,
    FundingRates,
}

impl Kind {
    const ALL: [Self; 7] = [
        Self::Deltas,
        Self::Depth,
        Self::Quotes,
        Self::Trades,
        Self::MarkPrices,
        Self::IndexPrices,
        Self::FundingRates,
    ];

    const fn channel(self) -> RisexWsChannel {
        match self {
            Self::Deltas | Self::Depth | Self::Quotes => RisexWsChannel::Orderbook,
            Self::Trades => RisexWsChannel::Trades,
            Self::MarkPrices | Self::IndexPrices => RisexWsChannel::Oracle,
            Self::FundingRates => RisexWsChannel::ExpectedFundingRate,
        }
    }

    fn of(data: &Data) -> Option<(Self, InstrumentId)> {
        match data {
            Data::BookDeltas(deltas) => Some((Self::Deltas, deltas.instrument_id)),
            Data::BookDepth(depth) => Some((Self::Depth, depth.instrument_id)),
            Data::Quote(quote) => Some((Self::Quotes, quote.instrument_id)),
            Data::Trade(trade) => Some((Self::Trades, trade.instrument_id)),
            Data::MarkPrice(mark) => Some((Self::MarkPrices, mark.instrument_id)),
            Data::IndexPrice(index) => Some((Self::IndexPrices, index.instrument_id)),
            Data::FundingRate(rate) => Some((Self::FundingRates, rate.instrument_id)),
            _ => None,
        }
    }
}

/// Returns the first bar close after `now` for epoch-aligned bars of `interval` nanoseconds.
const fn next_bar_close(now: u64, interval: u64) -> u64 {
    (now / interval + 1) * interval
}

/// State shared by the client and its tasks.
#[derive(Debug, Clone)]
struct Feed {
    http: RisexHttpClient,
    // `connect` and `disconnect` of the WebSocket client need `&mut self`
    ws: Arc<tokio::sync::Mutex<RisexWebSocketClient>>,
    handler: Arc<parking_lot::Mutex<FeedHandler>>,
    instruments: Arc<DashMap<InstrumentId, InstrumentAny>>,
    subscriptions: Arc<DashSet<(Kind, InstrumentId)>>,
    /// The status last sent for each instrument whose status is subscribed.
    statuses: Arc<DashMap<InstrumentId, Option<MarketStatus>>>,
    sender: EventSender<DataEvent>,
    clock: &'static AtomicTime,
}

impl Feed {
    fn send(&self, data: Data) {
        if let Err(e) = self.sender.send(DataEvent::Data(data)) {
            log::error!("Failed to send RISEx data: {e}");
        }
    }

    fn wants_channel(&self, channel: RisexWsChannel, instrument_id: InstrumentId) -> bool {
        Kind::ALL.iter().any(|kind| {
            kind.channel() == channel && self.subscriptions.contains(&(*kind, instrument_id))
        })
    }

    async fn set_channel(
        &self,
        channel: RisexWsChannel,
        market_id: u16,
        subscribe: bool,
    ) -> Result<(), RisexWsError> {
        let ws = self.ws.lock().await;
        let market = [u32::from(market_id)];
        match (channel, subscribe) {
            (RisexWsChannel::Orderbook, true) => ws.subscribe_orderbook(&market).await,
            (RisexWsChannel::Orderbook, false) => ws.unsubscribe_orderbook(&market).await,
            (RisexWsChannel::Trades, true) => ws.subscribe_trades(&market).await,
            (RisexWsChannel::Trades, false) => ws.unsubscribe_trades(&market).await,
            (RisexWsChannel::Oracle, true) => ws.subscribe_oracle(&market).await,
            (RisexWsChannel::Oracle, false) => ws.unsubscribe_oracle(&market).await,
            (RisexWsChannel::ExpectedFundingRate, true) => {
                ws.subscribe_expected_funding_rate(&market).await
            }
            (RisexWsChannel::ExpectedFundingRate, false) => {
                ws.unsubscribe_expected_funding_rate(&market).await
            }
            _ => Ok(()),
        }
    }

    /// Consumes the WebSocket stream until it ends.
    async fn run(self, mut rx: UnboundedReceiver<RisexWsMessage>) {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        loop {
            let events = tokio::select! {
                message = rx.recv() => match message {
                    Some(message) => self.handler.lock().handle(message, self.clock.get_time_ns()),
                    None => break,
                },
                _ = timer.tick() => self.handler.lock().on_timer(self.clock.get_time_ns()),
            };

            for event in events {
                match event {
                    FeedEvent::Data(data) => {
                        if Kind::of(&data).is_some_and(|key| self.subscriptions.contains(&key)) {
                            self.send(data);
                        }
                    }
                    FeedEvent::Recover { market_id, action } => {
                        if let Some(new_rx) = self.recover(market_id, action).await {
                            rx = new_rx;
                        }
                    }
                }
            }
        }
        log::debug!("RISEx WebSocket stream ended");
    }

    /// Carries out a recovery action. Returns the new stream after a reconnect.
    async fn recover(
        &self,
        market_id: u16,
        action: RecoveryAction,
    ) -> Option<UnboundedReceiver<RisexWsMessage>> {
        match action {
            RecoveryAction::Resubscribe => {
                // Unsubscribed while the action was on its way
                if !self.handler.lock().tracks(market_id) {
                    return None;
                }
                // The venue sends a snapshot only as the answer to a subscribe
                for subscribe in [false, true] {
                    if let Err(e) = self
                        .set_channel(RisexWsChannel::Orderbook, market_id, subscribe)
                        .await
                    {
                        log::error!("Failed to resubscribe book of market {market_id}: {e}");
                    }
                }
            }
            RecoveryAction::ReconnectSocket => loop {
                let result = self.ws.lock().await.connect().await;
                match result {
                    Ok(rx) => {
                        self.handler.lock().on_reconnected(self.clock.get_time_ns());
                        return Some(rx);
                    }
                    Err(e) => {
                        log::error!("Failed to reconnect RISEx WebSocket: {e}");
                        tokio::time::sleep(RECONNECT_RETRY_DELAY).await;
                    }
                }
            },
            RecoveryAction::FetchRestSnapshot => {
                // shortcut: REST serves at most 250 levels per side, so a deeper book fails
                // the next checksum and goes back to a resubscribe; nothing better exists
                // until the venue serves the full book over REST.
                match self
                    .http
                    .raw()
                    .get_order_book(market_id, Some(MAX_BOOK_LIMIT))
                    .await
                {
                    Ok(book) => {
                        let levels = |side: Vec<crate::http::models::RisexBookLevel>| {
                            side.into_iter()
                                .map(|level| BookLevel {
                                    price: level.price,
                                    quantity: level.quantity,
                                    order_count: level.order_count,
                                })
                                .collect::<Vec<_>>()
                        };
                        let (bids, asks) = (levels(book.bids), levels(book.asks));
                        let events = self.handler.lock().on_rest_snapshot(
                            market_id,
                            &bids,
                            &asks,
                            self.clock.get_time_ns(),
                        );
                        for event in events {
                            if let FeedEvent::Data(data) = event
                                && Kind::of(&data)
                                    .is_some_and(|key| self.subscriptions.contains(&key))
                            {
                                self.send(data);
                            }
                        }
                    }
                    Err(e) => log::warn!("REST book snapshot of market {market_id} failed: {e}"),
                }
            }
            RecoveryAction::Halt => log::error!(
                "Book of market {market_id} halted after repeated desyncs: subscribe again to restart it"
            ),
        }
        None
    }

    /// Emits each closed bar of `bar_type`: one REST request per bar close.
    async fn poll_bars(self, instrument: InstrumentAny, bar_type: BarType, interval: u64) {
        loop {
            let now = self.clock.get_time_ns().as_u64();
            let close = next_bar_close(now, interval);
            tokio::time::sleep(Duration::from_nanos(close - now) + BAR_POLL_DELAY).await;

            // The venue stamps a bar with its open time
            let open = UnixNanos::from(close - interval);
            match self
                .http
                .request_bars(
                    &instrument,
                    bar_type,
                    Some(open),
                    Some(UnixNanos::from(close)),
                    None,
                )
                .await
            {
                Ok(bars) => match bars.into_iter().find(|bar| bar.ts_event == open) {
                    Some(bar) => self.send(Data::Bar(bar)),
                    None => log::warn!("RISEx returned no {bar_type} bar opened at {open}"),
                },
                Err(e) => log::error!("RISEx bar poll failed for {bar_type}: {e}"),
            }
        }
    }

    /// Reads the market flags and the maintenance state, and sends the status of each
    /// subscribed instrument whose status is new or changed.
    async fn poll_status(self) {
        if self.statuses.is_empty() {
            return;
        }
        // shortcut: a change shows after up to one poll interval, plus the 5 minutes the venue
        // caches `/v1/markets`; read the WS `system` channel if maintenance must show at once.
        let raw = self.http.raw();
        let (markets, system) =
            match tokio::try_join!(raw.get_markets(&[]), raw.get_system_config()) {
                Ok((markets, system)) => (markets.markets, system),
                Err(e) => {
                    log::warn!("RISEx instrument status poll failed: {e}");
                    return;
                }
            };
        let maintenance = system.is_maintenance_mode || system.maintenance_phase == "active";
        let registry = self.http.market_registry();
        let ts = self.clock.get_time_ns();
        for market in &markets {
            let Some(instrument_id) = registry.instrument_id(market.market_id) else {
                continue;
            };
            let Some(mut last) = self.statuses.get_mut(&instrument_id) else {
                continue;
            };
            let status = parse_market_status(market, maintenance);
            if *last == Some(status) {
                continue;
            }
            *last = Some(status);
            let (action, reason) = status;
            let trading = action == MarketStatusAction::Trading;
            self.send(Data::InstrumentStatus(InstrumentStatus::new(
                instrument_id,
                action,
                ts,
                ts,
                reason.map(Ustr::from),
                None,
                Some(trading),
                // A paused market still takes resting orders
                Some(trading || action == MarketStatusAction::Pause),
                None,
            )));
        }
    }

    /// Disconnects the socket and forgets every subscription.
    async fn close(self, ws_url: String) {
        let mut ws = self.ws.lock().await;
        ws.disconnect().await;
        // The client keeps its recorded subscriptions for the next connect: start a new one
        *ws = RisexWebSocketClient::new(ws_url);
        self.subscriptions.clear();
        self.statuses.clear();
        self.handler.lock().clear();
    }
}

/// Market data client for RISEx.
#[derive(Debug)]
pub struct RisexDataClient {
    client_id: ClientId,
    config: RisexDataClientConfig,
    feed: Feed,
    feed_task: Option<JoinHandle<()>>,
    status_task: Option<JoinHandle<()>>,
    bar_tasks: AHashMap<BarType, JoinHandle<()>>,
    close_task: Option<JoinHandle<()>>,
    is_connected: bool,
}

impl RisexDataClient {
    /// Creates a new [`RisexDataClient`]. Nothing is connected until [`DataClient::connect`].
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be built.
    pub fn new(client_id: ClientId, config: RisexDataClientConfig) -> anyhow::Result<Self> {
        let http = RisexHttpClient::new(
            config.environment,
            config.base_url_http.clone(),
            config.http_timeout_secs,
            None,
        )?;
        let instruments = Arc::new(DashMap::new());
        let handler = FeedHandler::new(
            http.market_registry(),
            instruments.clone(),
            RecoveryConfig::default(),
        );
        let feed = Feed {
            http,
            ws: Arc::new(tokio::sync::Mutex::new(RisexWebSocketClient::new(ws_url(
                &config,
            )))),
            handler: Arc::new(parking_lot::Mutex::new(handler)),
            instruments,
            subscriptions: Arc::new(DashSet::new()),
            statuses: Arc::new(DashMap::new()),
            sender: get_data_event_sender(),
            clock: get_atomic_clock_realtime(),
        };

        Ok(Self {
            client_id,
            config,
            feed,
            feed_task: None,
            status_task: None,
            bar_tasks: AHashMap::new(),
            close_task: None,
            is_connected: false,
        })
    }

    fn instrument(&self, instrument_id: &InstrumentId) -> anyhow::Result<InstrumentAny> {
        self.feed
            .instruments
            .get(instrument_id)
            .map(|instrument| instrument.clone())
            .ok_or_else(|| InstrumentLookupError::not_found(*instrument_id).into())
    }

    fn market_id(&self, instrument_id: &InstrumentId) -> anyhow::Result<u16> {
        self.feed
            .http
            .market_registry()
            .market_id(instrument_id)
            .ok_or_else(|| InstrumentLookupError::not_found(*instrument_id).into())
    }

    fn spawn_channel(&self, channel: RisexWsChannel, market_id: u16, subscribe: bool) {
        let feed = self.feed.clone();
        get_runtime().spawn(async move {
            if let Err(e) = feed.set_channel(channel, market_id, subscribe).await {
                log::error!(
                    "Failed to set RISEx {channel:?} subscription of market {market_id} to {subscribe}: {e}"
                );
            }
        });
    }

    fn subscribe_kind(&self, kind: Kind, instrument_ids: &[InstrumentId]) -> anyhow::Result<()> {
        execute_instrument_subscriptions(instrument_ids, |instrument_id| {
            let market_id = self.market_id(&instrument_id)?;
            let channel = kind.channel();
            let first = !self.feed.wants_channel(channel, instrument_id);
            self.feed.subscriptions.insert((kind, instrument_id));

            if first {
                if channel == RisexWsChannel::Orderbook {
                    let now = self.feed.clock.get_time_ns();
                    self.feed.handler.lock().expect_snapshot(market_id, now);
                }
                self.spawn_channel(channel, market_id, true);
            }
            Ok(())
        })
    }

    fn unsubscribe_kind(&self, kind: Kind, instrument_ids: &[InstrumentId]) -> anyhow::Result<()> {
        execute_instrument_subscriptions(instrument_ids, |instrument_id| {
            let channel = kind.channel();
            if self
                .feed
                .subscriptions
                .remove(&(kind, instrument_id))
                .is_none()
                || self.feed.wants_channel(channel, instrument_id)
            {
                return Ok(());
            }

            let market_id = self.market_id(&instrument_id)?;
            if channel == RisexWsChannel::Orderbook {
                self.feed.handler.lock().remove_book(market_id);
            }
            self.spawn_channel(channel, market_id, false);
            Ok(())
        })
    }

    fn send_cached_instruments(&self, instrument_id: Option<&InstrumentId>) {
        for entry in self.feed.instruments.iter() {
            if instrument_id.is_none_or(|id| id == entry.key()) {
                self.feed.send(entry.value().clone().into());
            }
        }
    }

    fn spawn_request(
        &self,
        what: &'static str,
        request: impl Future<Output = anyhow::Result<DataResponse>> + Send + 'static,
    ) {
        let sender = self.feed.sender.clone();
        get_runtime().spawn(async move {
            match request.await {
                Ok(response) => {
                    if let Err(e) = sender.send(DataEvent::Response(response)) {
                        log::error!("Failed to send RISEx {what} response: {e}");
                    }
                }
                Err(e) => log::error!("RISEx {what} request failed: {e:?}"),
            }
        });
    }

    fn abort_tasks(&mut self) {
        for task in [self.feed_task.take(), self.status_task.take()]
            .into_iter()
            .flatten()
        {
            task.abort();
        }
        for (_, task) in self.bar_tasks.drain() {
            task.abort();
        }
        self.is_connected = false;
    }

    fn stop_in_background(&mut self) {
        self.abort_tasks();
        self.close_task = Some(get_runtime().spawn(self.feed.clone().close(ws_url(&self.config))));
    }
}

fn ws_url(config: &RisexDataClientConfig) -> String {
    config
        .base_url_ws
        .clone()
        .unwrap_or_else(|| risex_ws_url(config.environment).to_string())
}

fn validate_book_type(book_type: BookType) -> anyhow::Result<()> {
    anyhow::ensure!(
        book_type == BookType::L2_MBP,
        "RISEx serves L2_MBP books only, was {book_type}"
    );
    Ok(())
}

#[async_trait::async_trait(?Send)]
impl DataClient for RisexDataClient {
    fn client_id(&self) -> ClientId {
        self.client_id
    }

    fn venue(&self) -> Option<Venue> {
        Some(*RISEX_VENUE)
    }

    fn start(&mut self) -> anyhow::Result<()> {
        log::info!(
            "Starting RISEx data client {} ({})",
            self.client_id,
            self.config.environment
        );
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.stop_in_background();
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        self.stop_in_background();
        Ok(())
    }

    fn dispose(&mut self) -> anyhow::Result<()> {
        self.stop_in_background();
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.is_connected
    }

    fn is_disconnected(&self) -> bool {
        !self.is_connected
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        if self.is_connected {
            return Ok(());
        }
        // A stop still closing the socket must not close the new connection
        if let Some(task) = self.close_task.take() {
            let _ = task.await;
        }

        let instruments = self
            .feed
            .http
            .request_instruments()
            .await
            .context("failed to load RISEx instruments")?;
        let count = instruments.len();
        for instrument in instruments {
            self.feed
                .instruments
                .insert(instrument.id(), instrument.clone());
            self.feed.send(instrument.into());
        }

        let rx = self
            .feed
            .ws
            .lock()
            .await
            .connect()
            .await
            .context("failed to connect RISEx WebSocket")?;
        self.feed_task = Some(get_runtime().spawn(self.feed.clone().run(rx)));
        let interval = Duration::from_secs(self.config.instrument_status_poll_interval_secs.max(1));
        let feed = self.feed.clone();
        self.status_task = Some(get_runtime().spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                feed.clone().poll_status().await;
            }
        }));
        self.is_connected = true;
        log::info!(
            "Connected RISEx data client {}: {count} instruments",
            self.client_id
        );
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.abort_tasks();
        if let Some(task) = self.close_task.take() {
            let _ = task.await;
        }
        self.feed.clone().close(ws_url(&self.config)).await;
        log::info!("Disconnected RISEx data client {}", self.client_id);
        Ok(())
    }

    fn subscribe_instruments(&mut self, _cmd: SubscribeInstruments) -> anyhow::Result<()> {
        // Instruments are loaded at connect: replay them
        self.send_cached_instruments(None);
        Ok(())
    }

    fn subscribe_instrument(&mut self, cmd: SubscribeInstrument) -> anyhow::Result<()> {
        execute_instrument_subscriptions(&cmd.instrument_ids, |instrument_id| {
            self.instrument(&instrument_id)?;
            self.send_cached_instruments(Some(&instrument_id));
            Ok(())
        })
    }

    fn subscribe_instrument_status(
        &mut self,
        cmd: SubscribeInstrumentStatus,
    ) -> anyhow::Result<()> {
        let result = execute_instrument_subscriptions(&cmd.instrument_ids, |instrument_id| {
            self.market_id(&instrument_id)?;
            // Nothing sent yet: the poll below sends the current status
            self.feed.statuses.insert(instrument_id, None);
            Ok(())
        });
        get_runtime().spawn(self.feed.clone().poll_status());
        result
    }

    fn subscribe_book_deltas(&mut self, cmd: SubscribeBookDeltas) -> anyhow::Result<()> {
        validate_book_type(cmd.book_type)?;
        self.subscribe_kind(Kind::Deltas, &cmd.instrument_ids)
    }

    fn subscribe_book_depth(&mut self, cmd: SubscribeBookDepth) -> anyhow::Result<()> {
        validate_book_type(cmd.book_type)?;
        self.subscribe_kind(Kind::Depth, &cmd.instrument_ids)
    }

    fn subscribe_quotes(&mut self, cmd: SubscribeQuotes) -> anyhow::Result<()> {
        self.subscribe_kind(Kind::Quotes, &cmd.instrument_ids)
    }

    fn subscribe_trades(&mut self, cmd: SubscribeTrades) -> anyhow::Result<()> {
        self.subscribe_kind(Kind::Trades, &cmd.instrument_ids)
    }

    fn subscribe_mark_prices(&mut self, cmd: SubscribeMarkPrices) -> anyhow::Result<()> {
        self.subscribe_kind(Kind::MarkPrices, &cmd.instrument_ids)
    }

    fn subscribe_index_prices(&mut self, cmd: SubscribeIndexPrices) -> anyhow::Result<()> {
        self.subscribe_kind(Kind::IndexPrices, &cmd.instrument_ids)
    }

    fn subscribe_funding_rates(&mut self, cmd: SubscribeFundingRates) -> anyhow::Result<()> {
        self.subscribe_kind(Kind::FundingRates, &cmd.instrument_ids)
    }

    fn subscribe_bars(&mut self, cmd: SubscribeBars) -> anyhow::Result<()> {
        execute_bar_subscriptions(&cmd.bar_types, |bar_type| {
            let interval = RisexBarInterval::try_from(&bar_type)?;
            // Epoch-aligned weeks start on a Thursday; where the venue starts its week is
            // not known, so a weekly poll could ask for a bar that does not exist
            anyhow::ensure!(
                interval != RisexBarInterval::Week1,
                "RISEx weekly bars cannot be subscribed, request them instead: {bar_type}"
            );
            let instrument = self.instrument(&bar_type.instrument_id())?;

            let task = get_runtime().spawn(self.feed.clone().poll_bars(
                instrument,
                bar_type,
                interval.as_nanos(),
            ));
            if let Some(old) = self.bar_tasks.insert(bar_type, task) {
                old.abort();
            }
            Ok(())
        })
    }

    fn unsubscribe_instruments(&mut self, _cmd: &UnsubscribeInstruments) -> anyhow::Result<()> {
        Ok(())
    }

    fn unsubscribe_instrument(&mut self, _cmd: &UnsubscribeInstrument) -> anyhow::Result<()> {
        Ok(())
    }

    fn unsubscribe_instrument_status(
        &mut self,
        cmd: &UnsubscribeInstrumentStatus,
    ) -> anyhow::Result<()> {
        for instrument_id in &cmd.instrument_ids {
            self.feed.statuses.remove(instrument_id);
        }
        Ok(())
    }

    fn unsubscribe_book_deltas(&mut self, cmd: &UnsubscribeBookDeltas) -> anyhow::Result<()> {
        self.unsubscribe_kind(Kind::Deltas, &cmd.instrument_ids)
    }

    fn unsubscribe_book_depth(&mut self, cmd: &UnsubscribeBookDepth) -> anyhow::Result<()> {
        self.unsubscribe_kind(Kind::Depth, &cmd.instrument_ids)
    }

    fn unsubscribe_quotes(&mut self, cmd: &UnsubscribeQuotes) -> anyhow::Result<()> {
        self.unsubscribe_kind(Kind::Quotes, &cmd.instrument_ids)
    }

    fn unsubscribe_trades(&mut self, cmd: &UnsubscribeTrades) -> anyhow::Result<()> {
        self.unsubscribe_kind(Kind::Trades, &cmd.instrument_ids)
    }

    fn unsubscribe_mark_prices(&mut self, cmd: &UnsubscribeMarkPrices) -> anyhow::Result<()> {
        self.unsubscribe_kind(Kind::MarkPrices, &cmd.instrument_ids)
    }

    fn unsubscribe_index_prices(&mut self, cmd: &UnsubscribeIndexPrices) -> anyhow::Result<()> {
        self.unsubscribe_kind(Kind::IndexPrices, &cmd.instrument_ids)
    }

    fn unsubscribe_funding_rates(&mut self, cmd: &UnsubscribeFundingRates) -> anyhow::Result<()> {
        self.unsubscribe_kind(Kind::FundingRates, &cmd.instrument_ids)
    }

    fn unsubscribe_bars(&mut self, cmd: &UnsubscribeBars) -> anyhow::Result<()> {
        execute_bar_subscriptions(&cmd.bar_types, |bar_type| {
            if let Some(task) = self.bar_tasks.remove(&bar_type) {
                task.abort();
            }
            Ok(())
        })
    }

    fn request_instruments(&self, request: RequestInstruments) -> anyhow::Result<()> {
        let feed = self.feed.clone();
        let client_id = request.client_id.unwrap_or(self.client_id);

        self.spawn_request("instruments", async move {
            let instruments = feed.http.request_instruments().await?;
            for instrument in &instruments {
                feed.instruments.insert(instrument.id(), instrument.clone());
            }
            Ok(DataResponse::Instruments(InstrumentsResponse::new(
                request.request_id,
                client_id,
                *RISEX_VENUE,
                instruments,
                datetime_to_unix_nanos(request.start),
                datetime_to_unix_nanos(request.end),
                feed.clock.get_time_ns(),
                request.params,
            )))
        });
        Ok(())
    }

    fn request_instrument(&self, request: RequestInstrument) -> anyhow::Result<()> {
        let instrument_id = request.single_instrument_id()?;
        let feed = self.feed.clone();
        let client_id = request.client_id.unwrap_or(self.client_id);

        self.spawn_request("instrument", async move {
            // The venue filter takes market ids, which are only known from this list
            let instrument = feed
                .http
                .request_instruments()
                .await?
                .into_iter()
                .find(|instrument| instrument.id() == instrument_id)
                .ok_or_else(|| InstrumentLookupError::not_found(instrument_id))?;
            feed.instruments.insert(instrument_id, instrument.clone());
            Ok(DataResponse::Instrument(Box::new(InstrumentResponse::new(
                request.request_id,
                client_id,
                instrument_id,
                instrument,
                datetime_to_unix_nanos(request.start),
                datetime_to_unix_nanos(request.end),
                feed.clock.get_time_ns(),
                request.params,
            ))))
        });
        Ok(())
    }

    fn request_book_snapshot(&self, request: RequestBookSnapshot) -> anyhow::Result<()> {
        let instrument_id = request.single_instrument_id()?;
        let instrument = self.instrument(&instrument_id)?;
        let feed = self.feed.clone();
        let client_id = request.client_id.unwrap_or(self.client_id);
        let depth = request.depth.map(|depth| depth.get() as u32);

        self.spawn_request("book snapshot", async move {
            let snapshot = feed.http.request_book_snapshot(&instrument, depth).await?;
            let ts_init = feed.clock.get_time_ns();
            let mut book = OrderBook::new(instrument_id, BookType::L2_MBP);
            for (side, levels) in [
                (OrderSide::Buy, snapshot.bids),
                (OrderSide::Sell, snapshot.asks),
            ] {
                for (price, size) in levels {
                    book.add(BookOrder::new(side, price, size, 0), 0, 0, ts_init);
                }
            }
            Ok(DataResponse::Book(BookResponse::new(
                request.request_id,
                client_id,
                instrument_id,
                book,
                None,
                None,
                ts_init,
                request.params,
            )))
        });
        Ok(())
    }

    fn request_trades(&self, request: RequestTrades) -> anyhow::Result<()> {
        let instrument_id = request.single_instrument_id()?;
        let instrument = self.instrument(&instrument_id)?;
        let feed = self.feed.clone();
        let client_id = request.client_id.unwrap_or(self.client_id);
        let start = datetime_to_unix_nanos(request.start);
        let end = datetime_to_unix_nanos(request.end);
        let limit = request.limit.map(|limit| limit.get() as u32);

        self.spawn_request("trades", async move {
            let trades = feed
                .http
                .request_trades(&instrument, start, end, limit)
                .await?;
            Ok(DataResponse::Trades(TradesResponse::new(
                request.request_id,
                client_id,
                instrument_id,
                trades,
                start,
                end,
                feed.clock.get_time_ns(),
                request.params,
            )))
        });
        Ok(())
    }

    fn request_funding_rates(&self, request: RequestFundingRates) -> anyhow::Result<()> {
        let instrument_id = request.single_instrument_id()?;
        let instrument = self.instrument(&instrument_id)?;
        let feed = self.feed.clone();
        let client_id = request.client_id.unwrap_or(self.client_id);
        let start = datetime_to_unix_nanos(request.start);
        let end = datetime_to_unix_nanos(request.end);
        let limit = request.limit.map(|limit| limit.get() as u32);

        self.spawn_request("funding rates", async move {
            let funding_rates = feed
                .http
                .request_funding_rates(&instrument, start, end, limit)
                .await?;
            Ok(DataResponse::FundingRates(FundingRatesResponse::new(
                request.request_id,
                client_id,
                instrument_id,
                funding_rates,
                start,
                end,
                feed.clock.get_time_ns(),
                request.params,
            )))
        });
        Ok(())
    }

    fn request_bars(&self, request: RequestBars) -> anyhow::Result<()> {
        let bar_type = request.single_bar_type()?;
        RisexBarInterval::try_from(&bar_type)?;
        let instrument = self.instrument(&bar_type.instrument_id())?;
        let feed = self.feed.clone();
        let client_id = request.client_id.unwrap_or(self.client_id);
        let start = datetime_to_unix_nanos(request.start);
        let end = datetime_to_unix_nanos(request.end);
        let limit = request.limit.map(|limit| limit.get() as u32);

        self.spawn_request("bars", async move {
            let bars = feed
                .http
                .request_bars(&instrument, bar_type, start, end, limit)
                .await?;
            Ok(DataResponse::Bars(BarsResponse::new(
                request.request_id,
                client_id,
                bar_type,
                bars,
                start,
                end,
                feed.clock.get_time_ns(),
                request.params,
            )))
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const MINUTE: u64 = 60_000_000_000;

    #[rstest]
    #[case(0, MINUTE, MINUTE)]
    #[case(MINUTE - 1, MINUTE, MINUTE)]
    // On the boundary the bar that just closed is not asked for again
    #[case(MINUTE, MINUTE, 2 * MINUTE)]
    #[case(7 * MINUTE + 1, 5 * MINUTE, 10 * MINUTE)]
    fn next_bar_close_is_the_next_interval_boundary(
        #[case] now: u64,
        #[case] interval: u64,
        #[case] expected: u64,
    ) {
        assert_eq!(next_bar_close(now, interval), expected);
    }

    #[rstest]
    fn kinds_of_one_book_share_the_orderbook_channel() {
        let channels: Vec<_> = Kind::ALL.iter().map(|kind| kind.channel()).collect();
        assert_eq!(
            channels,
            [
                RisexWsChannel::Orderbook,
                RisexWsChannel::Orderbook,
                RisexWsChannel::Orderbook,
                RisexWsChannel::Trades,
                RisexWsChannel::Oracle,
                RisexWsChannel::Oracle,
                RisexWsChannel::ExpectedFundingRate,
            ]
        );
    }
}
