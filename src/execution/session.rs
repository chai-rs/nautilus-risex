//! Signed requests of one account: nonce lease, permit, POST, one resync retry.

use std::{
    future::Future,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use alloy::signers::local::PrivateKeySigner;
use alloy_primitives::{Address, B256};
use anyhow::Context;

use crate::{
    common::{credential::RisexCredential, enums::RisexMarginMode, parse::RisexOrderId},
    http::{
        client::RisexRawHttpClient,
        error::{RisexHttpError, RisexHttpResult},
        models::{
            RisexCancelAllRequest, RisexCancelOrderRequest, RisexLeverageRequest,
            RisexMarginModeRequest, RisexPlaceOrderRequest, RisexPlaceOrderResponse,
            RisexTpslPlaceResponse,
        },
    },
    signing::{
        RisexSigner,
        action::{
            PlaceOrder, cancel_all_orders_hash, cancel_order_hash, update_leverage_hash,
            update_margin_mode_hash,
        },
        nonce::{NonceLease, NonceManager},
        permit::Permit,
        tpsl::{CancelAllTpsl, CancelTpsl, PlaceTpsl},
    },
};

/// The signing session of one account: EIP-712 domain, router and nonce state
/// are read from the venue at connect, never hard-coded.
#[derive(Debug)]
pub struct RisexSession {
    http: Arc<RisexRawHttpClient>,
    signer: RisexSigner,
    account: Address,
    account_str: String,
    router: Address,
    nonces: NonceManager,
    deadline_secs: u32,
}

impl RisexSession {
    /// Reads the EIP-712 domain, the system config and the nonce state.
    ///
    /// # Errors
    ///
    /// Returns an error if a credential value is malformed or a request fails.
    pub async fn connect(
        http: Arc<RisexRawHttpClient>,
        credential: &RisexCredential,
        deadline_secs: u32,
    ) -> anyhow::Result<Self> {
        let account_str = credential.account_address().to_string();
        let account: Address = account_str.parse().context("invalid account address")?;
        // The parse error is dropped on purpose: it must never carry key material.
        let key: PrivateKeySigner = credential
            .signer_private_key()
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid signer private key"))?;

        let domain = http.get_eip712_domain().await?;
        let system = http.get_system_config().await?;
        let nonce_state = http.get_nonce_state(&account_str).await?;

        let verifying_contract: Address = domain
            .verifying_contract
            .parse()
            .context("invalid verifying contract")?;
        let router: Address = system
            .addresses
            .router
            .parse()
            .context("invalid router address")?;
        let signer = RisexSigner::new(
            key,
            &domain.name,
            &domain.version,
            domain.chain_id,
            verifying_contract,
        );
        let nonces = NonceManager::new(nonce_state.nonce_anchor);
        nonces.resync(nonce_state.nonce_anchor, nonce_state.current_bitmap_index);

        Ok(Self {
            http,
            signer,
            account,
            account_str,
            router,
            nonces,
            deadline_secs,
        })
    }

    /// The account address as given in the credential.
    pub fn account(&self) -> &str {
        &self.account_str
    }

    /// Address of the session signer key.
    pub fn signer_address(&self) -> Address {
        self.signer.address()
    }

    /// `POST /v1/orders/place`.
    ///
    /// # Errors
    ///
    /// Returns an error if the order does not encode or the request fails.
    pub async fn place_order(
        &self,
        order: &PlaceOrder,
    ) -> RisexHttpResult<RisexPlaceOrderResponse> {
        let hash = order.action_hash().map_err(invalid)?;
        self.with_permit(hash, |permit| async move {
            let body = RisexPlaceOrderRequest {
                order: order.clone(),
                permit,
            };
            self.http.place_order(&body).await
        })
        .await
    }

    /// `POST /v1/orders/cancel`. `order_id` is the 24-byte hex order id.
    ///
    /// # Errors
    ///
    /// Returns an error if the order id is malformed or the request fails.
    pub async fn cancel_order(&self, market_id: u16, order_id: &str) -> RisexHttpResult<()> {
        // The signed hash covers the resting order id, not the id of the body.
        let resting = RisexOrderId::from_hex(order_id)
            .map_err(invalid)?
            .resting_order_id();
        // unverified: whether the API guards a cancel that reuses a resting slot.
        self.with_permit(cancel_order_hash(market_id, resting), |permit| async move {
            let body = RisexCancelOrderRequest {
                market_id,
                order_id: order_id.to_string(),
                permit,
                no_retry: false,
            };
            self.http.cancel_order(&body).await.map(drop)
        })
        .await
    }

    /// `POST /v1/orders/cancel-all` for one market.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub async fn cancel_all_orders(&self, market_id: u16) -> RisexHttpResult<()> {
        self.with_permit(cancel_all_orders_hash(market_id), |permit| async move {
            let body = RisexCancelAllRequest { market_id, permit };
            self.http.cancel_all_orders(&body).await.map(drop)
        })
        .await
    }

    /// `POST /v1/account/leverage`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub async fn update_leverage(&self, market_id: u16, leverage: u8) -> RisexHttpResult<()> {
        // unverified: leverage signature against the venue.
        self.with_permit(
            update_leverage_hash(market_id, leverage),
            |permit| async move {
                let body = RisexLeverageRequest {
                    market_id: market_id.to_string(),
                    leverage: leverage.to_string(),
                    permit_params: permit,
                };
                self.http.update_leverage(&body).await
            },
        )
        .await
    }

    /// `POST /v1/account/margin-mode`.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails.
    pub async fn update_margin_mode(
        &self,
        market_id: u16,
        margin_mode: RisexMarginMode,
    ) -> RisexHttpResult<()> {
        let code = margin_mode.code();
        // unverified: margin-mode signature against the venue.
        self.with_permit(
            update_margin_mode_hash(market_id, code),
            |permit| async move {
                let body = RisexMarginModeRequest {
                    market_id: market_id.to_string(),
                    margin_mode: code,
                    permit_params: permit,
                };
                self.http.update_margin_mode(&body).await
            },
        )
        .await
    }

    /// `POST /v1/orders/tpsl`. `account` and `deadline` of the request are filled here.
    ///
    /// # Errors
    ///
    /// Returns an error if signing or the request fails.
    pub async fn place_tpsl(
        &self,
        mut request: PlaceTpsl,
    ) -> RisexHttpResult<RisexTpslPlaceResponse> {
        request.account = self.account;
        request.deadline = self.deadline();
        // unverified: TP/SL signature and enum codes against the venue.
        let body = self.signer.place_tpsl(request).map_err(invalid)?;
        self.http.place_tpsl(&body).await
    }

    /// `POST /v1/orders/tpsl/cancel`.
    ///
    /// # Errors
    ///
    /// Returns an error if signing or the request fails.
    pub async fn cancel_tpsl(&self, order_id: &str) -> RisexHttpResult<()> {
        let request = CancelTpsl {
            order_id: order_id.to_string(),
            account: self.account,
            deadline: self.deadline(),
        };
        let body = self.signer.cancel_tpsl(request).map_err(invalid)?;
        self.http.cancel_tpsl(&body).await.map(drop)
    }

    /// `POST /v1/orders/tpsl/cancel-all` for one market.
    ///
    /// # Errors
    ///
    /// Returns an error if signing or the request fails.
    pub async fn cancel_all_tpsl(&self, market_id: u16) -> RisexHttpResult<()> {
        let request = CancelAllTpsl {
            account: self.account,
            market_id: u64::from(market_id),
            deadline: self.deadline(),
        };
        let body = self.signer.cancel_all_tpsl(request).map_err(invalid)?;
        self.http.cancel_all_tpsl(&body).await.map(drop)
    }

    // unverified: the accepted range of `deadline`.
    fn deadline(&self) -> u32 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        u32::try_from(now + u64::from(self.deadline_secs)).unwrap_or(u32::MAX)
    }

    async fn resync(&self) -> RisexHttpResult<()> {
        let state = self.http.get_nonce_state(&self.account_str).await?;
        self.nonces
            .resync(state.nonce_anchor, state.current_bitmap_index);
        Ok(())
    }

    async fn lease(&self) -> RisexHttpResult<NonceLease> {
        if let Ok(lease) = self.nonces.next().await {
            return Ok(lease);
        }
        self.resync().await?;
        self.nonces.next().await.map_err(invalid)
    }

    /// Signs a permit over `hash` and runs `call`. The nonce lease is held
    /// until the response; `InvalidNonceAnchor` gives one resync and one retry.
    async fn with_permit<T, F, Fut>(&self, hash: B256, call: F) -> RisexHttpResult<T>
    where
        F: Fn(Permit) -> Fut,
        Fut: Future<Output = RisexHttpResult<T>>,
    {
        let mut retried = false;
        loop {
            let lease = self.lease().await?;
            let permit = self
                .signer
                .permit(
                    self.account,
                    self.router,
                    hash,
                    lease.nonce,
                    self.deadline(),
                )
                .map_err(invalid)?;
            let result = call(permit).await;
            drop(lease);
            match result {
                Err(e) if !retried && e.to_string().contains("InvalidNonceAnchor") => {
                    log::warn!("RISEx nonce anchor rejected, resyncing: {e}");
                    retried = true;
                    self.resync().await?;
                }
                other => return other,
            }
        }
    }
}

fn invalid(e: impl std::fmt::Display) -> RisexHttpError {
    RisexHttpError::InvalidRequest(e.to_string())
}
