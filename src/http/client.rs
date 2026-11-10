//! Raw and domain HTTP clients for the RISEx REST endpoints.

use std::{collections::HashMap, sync::Arc};

use nautilus_core::{AtomicTime, UnixNanos, time::get_atomic_clock_realtime};
use nautilus_model::{
    data::{Bar, BarType, FundingRateUpdate, TradeTick},
    instruments::{Instrument, InstrumentAny},
};
use nautilus_network::{
    http::{
        HttpClient, HttpRedirectPolicy, HttpResponse, Method, create_standard_nautilus_headers,
        parse_retry_after,
    },
    ratelimiter::quota::Quota,
    retry::{RetryManager, create_http_retry_manager},
};
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    common::{
        consts::{MAX_BOOK_LIMIT, MAX_PAGE_LIMIT},
        enums::{RisexBarInterval, RisexEnvironment},
        rate_limit::{RISEX_REST_BUCKET, RISEX_REST_QUOTA},
        symbol::MarketRegistry,
        urls::risex_http_base_url,
    },
    http::{
        error::{
            RisexHttpError, RisexHttpResult, create_risex_http_timeout_error,
            should_retry_risex_http_error,
        },
        models::{
            RisexAccountPositionResponse, RisexAccountTrades, RisexBalance, RisexBars,
            RisexCancelAllRequest, RisexCancelOrderRequest, RisexEip712Domain, RisexErrorCodes,
            RisexErrorResponse, RisexFundingRates, RisexLeverageRequest, RisexMarginModeRequest,
            RisexMarkets, RisexNonceState, RisexOpenOrders, RisexOrder, RisexOrderBook,
            RisexOrderResponse, RisexOrders, RisexPlaceOrderRequest, RisexPlaceOrderResponse,
            RisexPositions, RisexResponse, RisexRevokeSignerRequest, RisexSessionKeyStatusResponse,
            RisexSuccessResponse, RisexSystemConfig, RisexTpslOrders, RisexTpslPlaceResponse,
            RisexTrades,
        },
        parse::{
            RisexBookSnapshot, parse_bar, parse_book_snapshot, parse_funding_rate,
            parse_instrument, parse_trade_tick,
        },
        query::{
            QueryParams, RisexAccountTradesQuery, RisexBarsQuery, RisexOpenOrdersQuery,
            RisexOrdersQuery, RisexPageQuery, RisexPositionsQuery, RisexTpslOrdersQuery,
        },
    },
    signing::tpsl::{CancelAllTpsl, CancelTpsl, PlaceTpsl, Signed},
};

const HEADER_RETRY_AFTER: &str = "retry-after";

/// Upper bound on the pages fetched for one history request.
const MAX_HISTORY_PAGES: u32 = 100;

/// Raw client: one method per endpoint, returning venue models.
#[derive(Clone, Debug)]
pub struct RisexRawHttpClient {
    base_url: String,
    environment: RisexEnvironment,
    client: HttpClient,
    retry_manager: RetryManager<RisexHttpError>,
}

impl RisexRawHttpClient {
    /// Creates a new [`RisexRawHttpClient`] with the default REST quota.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be created.
    pub fn new(
        environment: RisexEnvironment,
        base_url: Option<String>,
        timeout_secs: u64,
        proxy_url: Option<String>,
    ) -> RisexHttpResult<Self> {
        Self::new_with_quotas(
            environment,
            base_url,
            timeout_secs,
            proxy_url,
            *RISEX_REST_QUOTA,
        )
    }

    /// Creates a new [`RisexRawHttpClient`] with an explicit REST quota.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be created.
    pub fn new_with_quotas(
        environment: RisexEnvironment,
        base_url: Option<String>,
        timeout_secs: u64,
        proxy_url: Option<String>,
        default_quota: Quota,
    ) -> RisexHttpResult<Self> {
        let base_url = base_url
            .unwrap_or_else(|| risex_http_base_url(environment).to_string())
            .trim_end_matches('/')
            .to_string();

        Ok(Self {
            base_url,
            environment,
            client: HttpClient::builder()
                .redirect_policy(HttpRedirectPolicy::Reject)
                .header_keys(vec![HEADER_RETRY_AFTER.to_string()])
                .headers(create_standard_nautilus_headers().into_iter().collect())
                .default_quota(default_quota)
                .timeout_secs(timeout_secs)
                .maybe_proxy_url(proxy_url)
                .build()?,
            retry_manager: create_http_retry_manager(),
        })
    }

    /// Returns the configured REST base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    /// Returns the configured environment.
    #[must_use]
    pub const fn environment(&self) -> RisexEnvironment {
        self.environment
    }

    /// Overrides the retry manager. Intended for mock-server tests that need a shorter
    /// backoff than the default.
    pub fn set_retry_manager(&mut self, retry_manager: RetryManager<RisexHttpError>) {
        self.retry_manager = retry_manager;
    }

    /// Calls `GET /v1/markets`. An empty `market_ids` returns every market.
    pub async fn get_markets(&self, market_ids: &[u16]) -> RisexHttpResult<RisexMarkets> {
        let params = market_ids
            .iter()
            .map(|id| ("market_ids", id.to_string()))
            .collect();
        self.send_get_request("/v1/markets", params).await
    }

    /// Calls `GET /v1/system/config`.
    pub async fn get_system_config(&self) -> RisexHttpResult<RisexSystemConfig> {
        self.send_get_request("/v1/system/config", Vec::new()).await
    }

    /// Calls `GET /v1/auth/eip712-domain`.
    pub async fn get_eip712_domain(&self) -> RisexHttpResult<RisexEip712Domain> {
        self.send_get_request("/v1/auth/eip712-domain", Vec::new())
            .await
    }

    /// Calls `GET /v1/error-codes`.
    pub async fn get_error_codes(&self) -> RisexHttpResult<RisexErrorCodes> {
        self.send_get_request("/v1/error-codes", Vec::new()).await
    }

    /// Calls `GET /v1/orderbook`. The venue defaults `limit` to 50 levels per side.
    pub async fn get_order_book(
        &self,
        market_id: u16,
        limit: Option<u32>,
    ) -> RisexHttpResult<RisexOrderBook> {
        let mut params = vec![("market_id", market_id.to_string())];
        params.extend(limit.map(|limit| ("limit", limit.to_string())));
        self.send_get_request("/v1/orderbook", params).await
    }

    /// Calls `GET /v1/markets/id/{market_id}/trade-history`.
    pub async fn get_market_trades(
        &self,
        market_id: u16,
        query: &RisexPageQuery,
    ) -> RisexHttpResult<RisexTrades> {
        let endpoint = format!("/v1/markets/id/{market_id}/trade-history");
        self.send_get_request(&endpoint, query.params()).await
    }

    /// Calls `GET /v1/markets/id/{market_id}/trading-view-data`.
    pub async fn get_bars(
        &self,
        market_id: u16,
        query: &RisexBarsQuery,
    ) -> RisexHttpResult<RisexBars> {
        let endpoint = format!("/v1/markets/id/{market_id}/trading-view-data");
        self.send_get_request(&endpoint, query.params()).await
    }

    /// Calls `GET /v1/markets/id/{market_id}/funding-rate-history`.
    pub async fn get_funding_rates(
        &self,
        market_id: u16,
        query: &RisexPageQuery,
    ) -> RisexHttpResult<RisexFundingRates> {
        let endpoint = format!("/v1/markets/id/{market_id}/funding-rate-history");
        self.send_get_request(&endpoint, query.params()).await
    }

    /// Calls `GET /v1/trade-history`.
    pub async fn get_account_trades(
        &self,
        query: &RisexAccountTradesQuery,
    ) -> RisexHttpResult<RisexAccountTrades> {
        self.send_get_request("/v1/trade-history", query.params())
            .await
    }

    /// Calls `GET /v1/orders`.
    pub async fn get_orders(&self, query: &RisexOrdersQuery) -> RisexHttpResult<RisexOrders> {
        self.send_get_request("/v1/orders", query.params()).await
    }

    /// Calls `GET /v1/orders/open`.
    pub async fn get_open_orders(
        &self,
        query: &RisexOpenOrdersQuery,
    ) -> RisexHttpResult<RisexOpenOrders> {
        self.send_get_request("/v1/orders/open", query.params())
            .await
    }

    /// Calls `GET /v1/orders/by-id/{order_id}` with a composite order id (hex).
    pub async fn get_order_by_id(&self, order_id: &str) -> RisexHttpResult<RisexOrder> {
        let endpoint = format!("/v1/orders/by-id/{order_id}");
        let response: RisexOrderResponse = self.send_get_request(&endpoint, Vec::new()).await?;
        Ok(response.order)
    }

    /// Calls `GET /v1/orders/tpsl`.
    pub async fn get_tpsl_orders(
        &self,
        query: &RisexTpslOrdersQuery,
    ) -> RisexHttpResult<RisexTpslOrders> {
        self.send_get_request("/v1/orders/tpsl", query.params())
            .await
    }

    /// Calls `GET /v1/positions`.
    pub async fn get_positions(
        &self,
        query: &RisexPositionsQuery,
    ) -> RisexHttpResult<RisexPositions> {
        self.send_get_request("/v1/positions", query.params()).await
    }

    /// Calls `GET /v1/account/position`.
    pub async fn get_account_position(
        &self,
        account: &str,
        market_id: u16,
    ) -> RisexHttpResult<RisexAccountPositionResponse> {
        let params = vec![
            ("account", account.to_string()),
            ("market_id", market_id.to_string()),
        ];
        self.send_get_request("/v1/account/position", params).await
    }

    /// Calls `GET /v1/account/balance`. `token` is required: use the `usdc` address of
    /// [`RisexSystemConfig`].
    pub async fn get_account_balance(
        &self,
        account: &str,
        token: &str,
    ) -> RisexHttpResult<RisexBalance> {
        let params = vec![
            ("account", account.to_string()),
            ("token", token.to_string()),
        ];
        self.send_get_request("/v1/account/balance", params).await
    }

    /// Calls `GET /v1/account/cross-margin-balance`.
    pub async fn get_cross_margin_balance(&self, account: &str) -> RisexHttpResult<RisexBalance> {
        let params = vec![("account", account.to_string())];
        self.send_get_request("/v1/account/cross-margin-balance", params)
            .await
    }

    /// Calls `GET /v1/nonce-state/{account}`.
    pub async fn get_nonce_state(&self, account: &str) -> RisexHttpResult<RisexNonceState> {
        let endpoint = format!("/v1/nonce-state/{account}");
        self.send_get_request(&endpoint, Vec::new()).await
    }

    /// Calls `GET /v1/auth/session-key-status`.
    pub async fn get_session_key_status(
        &self,
        account: &str,
        signer: &str,
    ) -> RisexHttpResult<RisexSessionKeyStatusResponse> {
        let params = vec![
            ("account", account.to_string()),
            ("signer", signer.to_string()),
        ];
        self.send_get_request("/v1/auth/session-key-status", params)
            .await
    }

    /// Sends one `POST` with a JSON body and returns the `data` of the response.
    ///
    /// Not retried: a signed request could be applied twice if it reached the venue and only
    /// the response was lost.
    pub async fn post_json<B, T>(&self, endpoint: &str, body: &B) -> RisexHttpResult<T>
    where
        B: Serialize,
        T: DeserializeOwned,
    {
        let headers = HashMap::from([("Content-Type".to_string(), "application/json".to_string())]);
        let response = self
            .client
            .request(
                Method::POST,
                self.url(endpoint),
                None,
                Some(headers),
                Some(serde_json::to_vec(body)?),
                None,
                Some(Self::rate_limit_keys()),
            )
            .await?;

        // unverified: the docs show POST responses without the `data` envelope of the GET
        // endpoints, so both forms are accepted.
        Self::parse_response(&response).or_else(|e| match e {
            RisexHttpError::Parse(_) => serde_json::from_slice(&response.body).map_err(|_| e),
            e => Err(e),
        })
    }

    /// `POST /v1/orders/place`.
    pub async fn place_order(
        &self,
        body: &RisexPlaceOrderRequest,
    ) -> RisexHttpResult<RisexPlaceOrderResponse> {
        self.post_json("/v1/orders/place", body).await
    }

    /// `POST /v1/orders/cancel`.
    pub async fn cancel_order(
        &self,
        body: &RisexCancelOrderRequest,
    ) -> RisexHttpResult<RisexSuccessResponse> {
        self.post_json("/v1/orders/cancel", body).await
    }

    /// `POST /v1/orders/cancel-all`: every open order of one market.
    pub async fn cancel_all_orders(
        &self,
        body: &RisexCancelAllRequest,
    ) -> RisexHttpResult<RisexSuccessResponse> {
        self.post_json("/v1/orders/cancel-all", body).await
    }

    /// `POST /v1/account/leverage`.
    pub async fn update_leverage(&self, body: &RisexLeverageRequest) -> RisexHttpResult<()> {
        self.post_json::<_, serde::de::IgnoredAny>("/v1/account/leverage", body)
            .await
            .map(drop)
    }

    /// `POST /v1/account/margin-mode`.
    pub async fn update_margin_mode(&self, body: &RisexMarginModeRequest) -> RisexHttpResult<()> {
        self.post_json::<_, serde::de::IgnoredAny>("/v1/account/margin-mode", body)
            .await
            .map(drop)
    }

    /// `POST /v1/orders/tpsl`.
    pub async fn place_tpsl(
        &self,
        body: &Signed<PlaceTpsl>,
    ) -> RisexHttpResult<RisexTpslPlaceResponse> {
        self.post_json("/v1/orders/tpsl", body).await
    }

    /// `POST /v1/orders/tpsl/cancel`.
    pub async fn cancel_tpsl(
        &self,
        body: &Signed<CancelTpsl>,
    ) -> RisexHttpResult<RisexSuccessResponse> {
        self.post_json("/v1/orders/tpsl/cancel", body).await
    }

    /// `POST /v1/orders/tpsl/cancel-all`: every TP/SL order of one market.
    pub async fn cancel_all_tpsl(
        &self,
        body: &Signed<CancelAllTpsl>,
    ) -> RisexHttpResult<RisexSuccessResponse> {
        self.post_json("/v1/orders/tpsl/cancel-all", body).await
    }

    /// `POST /v1/auth/revoke-signer`.
    pub async fn revoke_signer(
        &self,
        body: &RisexRevokeSignerRequest,
    ) -> RisexHttpResult<RisexSuccessResponse> {
        self.post_json("/v1/auth/revoke-signer", body).await
    }

    async fn send_get_request<T>(&self, endpoint: &str, params: QueryParams) -> RisexHttpResult<T>
    where
        T: DeserializeOwned,
    {
        let url = self.url(endpoint);
        let params = &params;
        self.retry_manager
            .invocation(
                endpoint,
                || {
                    let url = url.clone();

                    async move {
                        let response = self
                            .client
                            .request_with_params(
                                Method::GET,
                                url,
                                Some(params),
                                None,
                                None,
                                None,
                                Some(Self::rate_limit_keys()),
                            )
                            .await?;
                        Self::parse_response(&response)
                    }
                },
                should_retry_risex_http_error,
                |e| create_risex_http_timeout_error(e.to_string()),
            )
            .retry_delay(&RisexHttpError::retry_after)
            .execute()
            .await
    }

    fn parse_response<T>(response: &HttpResponse) -> RisexHttpResult<T>
    where
        T: DeserializeOwned,
    {
        let venue_error = || {
            let body = serde_json::from_slice::<RisexErrorResponse>(&response.body).ok()?;
            Some(RisexHttpError::Venue {
                code: body.error.code,
                message: body.error.message,
            })
        };

        if !response.status.is_success() {
            let retry_after = response.headers.get(HEADER_RETRY_AFTER).and_then(|value| {
                let now = get_atomic_clock_realtime().get_time_ns().to_datetime_utc();
                parse_retry_after(value, now)
            });
            let status = response.status.as_u16();
            let body = String::from_utf8_lossy(&response.body).to_string();

            // Status first: an error body must not override the retry decision for 5xx
            // and 429.
            if status >= 500 {
                return Err(RisexHttpError::Http {
                    status,
                    body,
                    retry_after,
                });
            }

            if status == 429 {
                return Err(RisexHttpError::RateLimit {
                    message: body,
                    retry_after,
                });
            }

            return Err(venue_error().unwrap_or(RisexHttpError::Http {
                status,
                body,
                retry_after,
            }));
        }

        match serde_json::from_slice::<RisexResponse<T>>(&response.body) {
            Ok(envelope) => Ok(envelope.data),
            Err(e) => Err(venue_error().unwrap_or_else(|| e.into())),
        }
    }

    fn url(&self, endpoint: &str) -> String {
        format!("{}{}", self.base_url, endpoint)
    }

    // The venue limit is per client, not per route, so every request draws on one bucket.
    fn rate_limit_keys() -> Vec<String> {
        vec![RISEX_REST_BUCKET.to_string()]
    }
}

/// Domain client: requests venue data and returns Nautilus types.
#[derive(Clone, Debug)]
pub struct RisexHttpClient {
    pub(crate) inner: Arc<RisexRawHttpClient>,
    market_registry: Arc<MarketRegistry>,
    clock: &'static AtomicTime,
}

impl RisexHttpClient {
    /// Creates a new [`RisexHttpClient`].
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying HTTP client cannot be created.
    pub fn new(
        environment: RisexEnvironment,
        base_url: Option<String>,
        timeout_secs: u64,
        proxy_url: Option<String>,
    ) -> RisexHttpResult<Self> {
        RisexRawHttpClient::new(environment, base_url, timeout_secs, proxy_url).map(Self::from_raw)
    }

    /// Wraps a raw client with a new market registry.
    #[must_use]
    pub fn from_raw(raw: RisexRawHttpClient) -> Self {
        Self::from_raw_with_registry(raw, Arc::new(MarketRegistry::new()))
    }

    /// Wraps a raw client with a market registry shared with other clients.
    #[must_use]
    pub fn from_raw_with_registry(
        raw: RisexRawHttpClient,
        market_registry: Arc<MarketRegistry>,
    ) -> Self {
        Self {
            inner: Arc::new(raw),
            market_registry,
            clock: get_atomic_clock_realtime(),
        }
    }

    /// Returns the raw client, for the endpoints that have no domain method.
    #[must_use]
    pub fn raw(&self) -> &RisexRawHttpClient {
        &self.inner
    }

    /// Returns the registry that maps venue market ids to instrument ids. It is filled by
    /// [`Self::request_instruments`].
    #[must_use]
    pub fn market_registry(&self) -> Arc<MarketRegistry> {
        self.market_registry.clone()
    }

    /// Requests every active market as an instrument and registers its market id.
    ///
    /// Inactive markets and markets that cannot be parsed are skipped.
    pub async fn request_instruments(&self) -> RisexHttpResult<Vec<InstrumentAny>> {
        let markets = self.inner.get_markets(&[]).await?.markets;
        let ts_init = self.generate_ts_init();
        let mut instruments = Vec::with_capacity(markets.len());

        for market in markets.iter().filter(|market| market.active) {
            match parse_instrument(market, ts_init) {
                Ok(instrument) => {
                    self.market_registry
                        .insert(market.market_id, instrument.id());
                    instruments.push(instrument);
                }
                Err(e) => log::warn!(
                    "Skipping market {} ({}): {e}",
                    market.market_id,
                    market.config.name
                ),
            }
        }

        Ok(instruments)
    }

    /// Requests public trades in `[start, end]`, oldest first. With a `limit`, the most
    /// recent `limit` trades of the range are returned.
    ///
    /// Without `start` and `limit` only the most recent page is returned, because the
    /// history is not bounded.
    pub async fn request_trades(
        &self,
        instrument: &InstrumentAny,
        start: Option<UnixNanos>,
        end: Option<UnixNanos>,
        limit: Option<u32>,
    ) -> RisexHttpResult<Vec<TradeTick>> {
        let market_id = self.market_id(instrument)?;
        // Pages are newest first: a fixed end keeps them stable while new trades arrive.
        let end = end.unwrap_or_else(|| self.generate_ts_init());
        let total = match (start, limit) {
            (_, Some(limit)) => limit as usize,
            (Some(_), None) => usize::MAX,
            (None, None) => MAX_PAGE_LIMIT as usize,
        };
        let page_size = total.min(MAX_PAGE_LIMIT as usize);

        let trades = collect_pages("trade", total, |page| {
            let query = RisexPageQuery {
                start_time: start.map(|start| start.as_u64()),
                end_time: Some(end.as_u64()),
                page: Some(page),
                limit: Some(page_size as u32),
            };
            async move {
                let batch = self.inner.get_market_trades(market_id, &query).await?;
                // The response has no `has_next_page`: a short page is the last one.
                let has_next = batch.trades.len() >= page_size;
                Ok((batch.trades, has_next))
            }
        })
        .await?;

        let ts_init = self.generate_ts_init();
        let ticks: anyhow::Result<Vec<_>> = trades
            .iter()
            .rev()
            .map(|trade| parse_trade_tick(trade, instrument, ts_init))
            .collect();
        Ok(ticks?)
    }

    /// Requests bars, oldest first. With a `limit`, the most recent `limit` bars of the
    /// range are returned. Without `start` and `limit` the venue returns the last hour.
    ///
    /// The venue supports 1m, 5m, 15m, 1h, 4h, 1d and 1w bars. Any other bar type is
    /// rejected here, because the venue would answer it with 1m bars.
    pub async fn request_bars(
        &self,
        instrument: &InstrumentAny,
        bar_type: BarType,
        start: Option<UnixNanos>,
        end: Option<UnixNanos>,
        limit: Option<u32>,
    ) -> RisexHttpResult<Vec<Bar>> {
        let interval = RisexBarInterval::try_from(&bar_type)
            .map_err(|e| RisexHttpError::InvalidRequest(e.to_string()))?;
        let market_id = self.market_id(instrument)?;
        let from = match (start, limit) {
            (Some(start), _) => Some(start.as_u64()),
            (None, Some(limit)) => {
                let end = end.unwrap_or_else(|| self.generate_ts_init()).as_u64();
                Some(end.saturating_sub(u64::from(limit) * interval.as_nanos()))
            }
            (None, None) => None,
        };
        let query = RisexBarsQuery {
            interval,
            from,
            to: end.map(|end| end.as_u64()),
        };

        // shortcut: one request, the venue cap on bars per response is not known; split the
        // range into several requests if long ranges come back truncated.
        let bars = self.inner.get_bars(market_id, &query).await?.data;
        if let Some(bar) = bars.iter().find(|bar| bar.interval != interval.as_str()) {
            return Err(RisexHttpError::Parse(format!(
                "requested {interval} bars, venue returned {} bars",
                bar.interval
            )));
        }

        let skip = limit.map_or(0, |limit| bars.len().saturating_sub(limit as usize));
        let ts_init = self.generate_ts_init();
        let bars: anyhow::Result<Vec<_>> = bars[skip..]
            .iter()
            .map(|bar| parse_bar(bar, bar_type, instrument, ts_init))
            .collect();
        Ok(bars?)
    }

    /// Requests settled funding rates in `[start, end]`, oldest first. With a `limit`, the
    /// most recent `limit` rates of the range are returned. The venue defaults the range to
    /// the last seven days.
    pub async fn request_funding_rates(
        &self,
        instrument: &InstrumentAny,
        start: Option<UnixNanos>,
        end: Option<UnixNanos>,
        limit: Option<u32>,
    ) -> RisexHttpResult<Vec<FundingRateUpdate>> {
        let market_id = self.market_id(instrument)?;
        let total = limit.map_or(usize::MAX, |limit| limit as usize);
        let page_size = total.min(MAX_PAGE_LIMIT as usize) as u32;

        let records = collect_pages("funding rate", total, |page| {
            let query = RisexPageQuery {
                start_time: start.map(|start| start.as_u64()),
                end_time: end.map(|end| end.as_u64()),
                page: Some(page),
                limit: Some(page_size),
            };
            async move {
                let batch = self.inner.get_funding_rates(market_id, &query).await?;
                Ok((batch.records, batch.has_next_page))
            }
        })
        .await?;

        let ts_init = self.generate_ts_init();
        Ok(records
            .iter()
            .rev()
            .map(|record| parse_funding_rate(record, instrument, ts_init))
            .collect())
    }

    /// Requests an order book snapshot with at most `depth` levels per side (venue default
    /// 50, maximum 250).
    pub async fn request_book_snapshot(
        &self,
        instrument: &InstrumentAny,
        depth: Option<u32>,
    ) -> RisexHttpResult<RisexBookSnapshot> {
        let market_id = self.market_id(instrument)?;
        let depth = depth.map(|depth| depth.min(MAX_BOOK_LIMIT));
        let book = self.inner.get_order_book(market_id, depth).await?;
        Ok(parse_book_snapshot(&book, instrument)?)
    }

    fn market_id(&self, instrument: &InstrumentAny) -> RisexHttpResult<u16> {
        self.market_registry
            .market_id(&instrument.id())
            .ok_or_else(|| {
                RisexHttpError::InvalidRequest(format!(
                    "market id not registered for instrument {}",
                    instrument.id()
                ))
            })
    }

    fn generate_ts_init(&self) -> UnixNanos {
        self.clock.get_time_ns()
    }
}

/// Fetches pages (1-based, newest first) until the venue has no more or `total` rows are
/// collected. `fetch` returns the rows of a page and whether another page follows.
async fn collect_pages<T, F, Fut>(
    data_type: &'static str,
    total: usize,
    mut fetch: F,
) -> RisexHttpResult<Vec<T>>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = RisexHttpResult<(Vec<T>, bool)>>,
{
    let mut rows = Vec::new();

    for page in 1..=MAX_HISTORY_PAGES {
        let (batch, has_next) = fetch(page).await?;
        let last = batch.is_empty() || !has_next;
        rows.extend(batch);

        if rows.len() >= total {
            rows.truncate(total);
            return Ok(rows);
        }

        if last {
            return Ok(rows);
        }
    }

    Err(RisexHttpError::HistoryIncomplete {
        data_type,
        pages: MAX_HISTORY_PAGES as usize,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use axum::{
        Router,
        extract::{Query, RawQuery},
        http::StatusCode,
        routing::{get, post},
    };
    use nautilus_network::retry::RetryConfig;
    use rstest::rstest;
    use serde_json::json;

    use super::*;
    use crate::http::parse::tests::instrument;

    const AERO: u16 = 15;
    const FUNDING_PATH: &str = "/v1/markets/id/15/funding-rate-history";
    const TRADES_PATH: &str = "/v1/markets/id/15/trade-history";
    const BARS_PATH: &str = "/v1/markets/id/15/trading-view-data";

    fn fixture_json(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/http/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(path).unwrap()
    }

    async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{address}")
    }

    /// Client for the mock server, with AERO registered and a backoff of one millisecond.
    fn client(base_url: String, max_retries: u32) -> RisexHttpClient {
        let mut raw =
            RisexRawHttpClient::new(RisexEnvironment::Mainnet, Some(base_url), 5, None).unwrap();
        raw.set_retry_manager(RetryManager::new(RetryConfig {
            max_retries,
            initial_delay_ms: 1,
            max_delay_ms: 1,
            backoff_factor: 1.0,
            jitter_ms: 0,
            operation_timeout_ms: Some(5_000),
            immediate_first: false,
            max_elapsed_ms: None,
        }));
        let client = RisexHttpClient::from_raw(raw);
        client.market_registry.insert(AERO, instrument(AERO).id());
        client
    }

    /// Route that counts its calls and always answers with `status` and `body`.
    fn counted(
        status: StatusCode,
        body: &'static str,
    ) -> (Arc<AtomicUsize>, axum::routing::MethodRouter) {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let handler = move || {
            counter.fetch_add(1, Ordering::SeqCst);
            async move { (status, [(HEADER_RETRY_AFTER, "2")], body) }
        };
        (hits, get(handler.clone()).post(handler))
    }

    #[tokio::test]
    async fn test_envelope_is_unwrapped_and_list_params_are_repeated() {
        let queries = Arc::new(Mutex::new(Vec::new()));
        let seen = queries.clone();
        let router = Router::new().route(
            "/v1/markets",
            get(move |RawQuery(query): RawQuery| {
                seen.lock().unwrap().push(query);
                async { fixture_json("markets") }
            }),
        );
        let client = client(serve(router).await, 0);

        let markets = client.raw().get_markets(&[15, 1]).await.unwrap();
        let instruments = client.request_instruments().await.unwrap();

        assert_eq!(markets.markets.len(), 54);
        assert_eq!(instruments.len(), 53);
        assert_eq!(client.market_registry().len(), 53);
        assert_eq!(
            client.market_registry().market_id(&instrument(1).id()),
            Some(1)
        );
        assert_eq!(
            *queries.lock().unwrap(),
            vec![Some("market_ids=15&market_ids=1".to_string()), None]
        );
    }

    #[rstest]
    #[case(
        StatusCode::BAD_REQUEST,
        r#"{"error":{"code":"InvalidArgument","message":"bad market"},"request_id":"r"}"#,
        "venue error InvalidArgument: bad market"
    )]
    #[case(
        StatusCode::OK,
        r#"{"error":{"code":"E0001","message":"Please log in to continue."}}"#,
        "venue error E0001: Please log in to continue."
    )]
    #[case(StatusCode::NOT_FOUND, "not found", "HTTP 404: not found")]
    #[case(
        StatusCode::TOO_MANY_REQUESTS,
        "slow down",
        "rate limit exceeded: slow down"
    )]
    #[case(
        StatusCode::BAD_GATEWAY,
        r#"{"error":{"code":"Internal","message":"x"}}"#,
        r#"HTTP 502: {"error":{"code":"Internal","message":"x"}}"#
    )]
    #[tokio::test]
    async fn test_error_mapping(
        #[case] status: StatusCode,
        #[case] body: &'static str,
        #[case] expected: &str,
    ) {
        let (hits, route) = counted(status, body);
        let client = client(
            serve(Router::new().route("/v1/system/config", route)).await,
            0,
        );

        let error = client.raw().get_system_config().await.unwrap_err();

        assert_eq!(error.to_string(), expected);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // Only the status-based errors carry the `retry-after` of the mock.
        let retry_after =
            matches!(status.as_u16(), 404 | 429 | 502).then_some(Duration::from_secs(2));
        assert_eq!(error.retry_after(), retry_after);
    }

    #[tokio::test]
    async fn test_get_is_retried_on_server_error_and_post_is_not() {
        let (get_hits, get_route) = counted(StatusCode::SERVICE_UNAVAILABLE, "down");
        let (venue_hits, venue_route) = counted(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"InvalidArgument","message":"m"}}"#,
        );
        let (post_hits, post_route) = counted(StatusCode::SERVICE_UNAVAILABLE, "down");
        let router = Router::new()
            .route("/v1/error-codes", get_route)
            .route("/v1/system/config", venue_route)
            .route("/v1/orders/place", post_route);
        let mut client = client(serve(router).await, 2);
        // The mock sends `retry-after: 2`: do not wait for it here.
        Arc::get_mut(&mut client.inner).unwrap().client = HttpClient::builder().build().unwrap();

        assert!(client.raw().get_error_codes().await.is_err());
        assert!(client.raw().get_system_config().await.is_err());
        let posted: RisexHttpResult<serde_json::Value> =
            client.raw().post_json("/v1/orders/place", &json!({})).await;

        assert_eq!(get_hits.load(Ordering::SeqCst), 3);
        assert_eq!(venue_hits.load(Ordering::SeqCst), 1);
        assert!(matches!(
            posted,
            Err(RisexHttpError::Http { status: 503, .. })
        ));
        assert_eq!(post_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_post_json_sends_body_and_unwraps_envelope() {
        let router = Router::new().route(
            "/v1/echo",
            post(
                |axum::Json(body): axum::Json<serde_json::Value>| async move {
                    axum::Json(json!({"data": body, "request_id": "r"}))
                },
            ),
        );
        let client = client(serve(router).await, 0);

        let echoed: serde_json::Value = client
            .raw()
            .post_json("/v1/echo", &json!({"market_id": 15}))
            .await
            .unwrap();

        assert_eq!(echoed, json!({"market_id": 15}));
    }

    #[tokio::test]
    async fn test_request_funding_rates_follows_pages() {
        let record = |end_time: u64| {
            json!({
                "funding_rate": "0.0001",
                "start_time": (end_time - 3_600_000_000_000).to_string(),
                "end_time": end_time.to_string(),
            })
        };
        let pages = Arc::new(Mutex::new(Vec::new()));
        let seen = pages.clone();
        let router = Router::new().route(
            FUNDING_PATH,
            get(move |Query(query): Query<HashMap<String, String>>| {
                let page: u64 = query["page"].parse().unwrap();
                seen.lock().unwrap().push(page);
                // Newest first: page 1 holds hours 6 and 5, page 2 hours 4 and 3, page 3 hour 2.
                let newest = (8 - 2 * page) * 3_600_000_000_000;
                let records = match page {
                    1 | 2 => vec![record(newest), record(newest - 3_600_000_000_000)],
                    _ => vec![record(newest)],
                };
                async move {
                    axum::Json(json!({"data": {
                        "market_id": "15", "records": records, "page": page, "has_next_page": page < 3,
                    }}))
                }
            }),
        );
        let client = client(serve(router).await, 0);
        let aero = instrument(AERO);

        let all = client
            .request_funding_rates(&aero, None, None, None)
            .await
            .unwrap();
        let hours = |updates: &[FundingRateUpdate]| -> Vec<u64> {
            updates
                .iter()
                .map(|update| update.ts_event.as_u64() / 3_600_000_000_000)
                .collect()
        };
        assert_eq!(hours(&all), vec![2, 3, 4, 5, 6]);
        assert_eq!(*pages.lock().unwrap(), vec![1, 2, 3]);

        // The most recent three: stops after the second page and drops the oldest row.
        pages.lock().unwrap().clear();
        let latest = client
            .request_funding_rates(&aero, None, None, Some(3))
            .await
            .unwrap();
        assert_eq!(hours(&latest), vec![4, 5, 6]);
        assert_eq!(*pages.lock().unwrap(), vec![1, 2]);
    }

    #[tokio::test]
    async fn test_request_trades_returns_latest_in_ascending_order() {
        let queries = Arc::new(Mutex::new(Vec::new()));
        let seen = queries.clone();
        let router = Router::new().route(
            TRADES_PATH,
            get(move |Query(query): Query<HashMap<String, String>>| {
                seen.lock().unwrap().push(query);
                async { fixture_json("trades_aero") }
            }),
        );
        let client = client(serve(router).await, 0);

        let ticks = client
            .request_trades(&instrument(AERO), None, None, Some(10))
            .await
            .unwrap();

        assert_eq!(ticks.len(), 10);
        assert!(ticks.is_sorted_by_key(|tick| tick.ts_event));
        assert_eq!(
            ticks[9].ts_event,
            UnixNanos::from(1_794_280_444_000_000_000)
        );
        let queries = queries.lock().unwrap();
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0]["limit"], "10");
        assert_eq!(queries[0]["page"], "1");
        assert!(queries[0].contains_key("end_time"));
        assert!(!queries[0].contains_key("start_time"));
    }

    #[tokio::test]
    async fn test_request_bars_validates_interval() {
        let queries = Arc::new(Mutex::new(Vec::new()));
        let seen = queries.clone();
        let router = Router::new().route(
            BARS_PATH,
            get(move |Query(query): Query<HashMap<String, String>>| {
                seen.lock().unwrap().push(query);
                async { fixture_json("bars_aero_1m") }
            }),
        );
        let client = client(serve(router).await, 0);
        let aero = instrument(AERO);
        let bar_type = |spec: &str| BarType::from(format!("AERO-USDC-PERP.RISEX-{spec}").as_str());
        let end = Some(UnixNanos::from(1_794_280_560_000_000_000));

        // Not supported by the venue: rejected before any call.
        for spec in ["3-MINUTE-LAST-EXTERNAL", "1-MINUTE-BID-EXTERNAL"] {
            let error = client
                .request_bars(&aero, bar_type(spec), None, None, None)
                .await
                .unwrap_err();
            assert!(
                matches!(error, RisexHttpError::InvalidRequest(_)),
                "{error}"
            );
        }
        assert!(queries.lock().unwrap().is_empty());

        // The mock answers every interval with 1m bars, as the venue does for unknown ones.
        let error = client
            .request_bars(&aero, bar_type("5-MINUTE-LAST-EXTERNAL"), None, None, None)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "parse error: requested 5m bars, venue returned 1m bars"
        );

        let bars = client
            .request_bars(
                &aero,
                bar_type("1-MINUTE-LAST-EXTERNAL"),
                None,
                end,
                Some(5),
            )
            .await
            .unwrap();
        assert_eq!(bars.len(), 5);
        assert!(bars.is_sorted_by_key(|bar| bar.ts_event));
        assert_eq!(bars[4].ts_event, UnixNanos::from(1_794_280_500_000_000_000));
        let queries = queries.lock().unwrap();
        assert_eq!(queries[1]["interval"], "60000000000");
        assert_eq!(queries[1]["from"], "1794280260000000000");
        assert_eq!(queries[1]["to"], "1794280560000000000");
    }

    #[tokio::test]
    async fn test_unregistered_instrument_is_rejected() {
        let client = client("http://127.0.0.1:9".to_string(), 0);

        let error = client
            .request_book_snapshot(&instrument(1), None)
            .await
            .unwrap_err();

        assert!(
            matches!(error, RisexHttpError::InvalidRequest(_)),
            "{error}"
        );
    }

    #[tokio::test]
    async fn test_collect_pages_stops_at_the_page_cap() {
        let result = collect_pages(
            "trade",
            usize::MAX,
            |page| async move { Ok((vec![page], true)) },
        )
        .await;

        assert!(matches!(
            result,
            Err(RisexHttpError::HistoryIncomplete {
                data_type: "trade",
                pages: 100
            })
        ));
    }
}
