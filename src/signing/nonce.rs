//! Permit nonces: an anchor plus a bitmap index in `0..208`.
//!
//! The contract accepts the account's current anchor or current + 1, and each
//! `(anchor, index)` pair once. We start on `chain anchor + 1` from index 0
//! (integration guide, rule 1), one pair per request, and realign from nonce
//! state when the anchor is exhausted or rejected.

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

/// Indexes per anchor; index 208 and above revert with `InvalidNonceIndex`.
pub const NONCE_SLOTS: u8 = 208;

/// One permit nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Nonce {
    pub anchor: u64,
    pub index: u8,
}

/// All indexes of the anchor are used: read `GET /v1/account/nonce-state` and
/// call [`NonceManager::resync`], then retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("nonce anchor exhausted, resync from nonce state")]
pub struct NonceExhausted;

#[derive(Debug)]
struct State {
    anchor: u64,
    next_index: u8,
    in_flight: usize,
    /// The anchor moved while requests on the previous one were still in flight.
    draining: bool,
}

#[derive(Debug)]
struct Inner {
    state: Mutex<State>,
    drained: Notify,
}

/// Hands out unique nonces. Clones share state.
#[derive(Debug, Clone)]
pub struct NonceManager(Arc<Inner>);

/// A nonce reserved for one request. Hold it until the response (or timeout)
/// of that request: dropping it marks the request as no longer in flight.
#[derive(Debug)]
pub struct NonceLease {
    pub nonce: Nonce,
    inner: Arc<Inner>,
}

impl NonceManager {
    /// `chain_anchor` is `nonce_anchor` from the nonce-state endpoint.
    pub fn new(chain_anchor: u64) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                anchor: chain_anchor.saturating_add(1),
                next_index: 0,
                in_flight: 0,
                draining: false,
            }),
            drained: Notify::new(),
        }))
    }

    /// Reserves the next nonce.
    ///
    /// After a move to a new anchor this waits until every request signed on
    /// the old one has finished: a new-anchor request landing first would
    /// advance the chain anchor and invalidate those still in flight.
    pub async fn next(&self) -> Result<NonceLease, NonceExhausted> {
        loop {
            let drained = self.0.drained.notified();
            tokio::pin!(drained);
            // Register before the check so a lease dropped in between still wakes us.
            drained.as_mut().enable();
            {
                let mut state = self.0.state.lock();
                if !state.draining {
                    if state.next_index >= NONCE_SLOTS {
                        return Err(NonceExhausted);
                    }
                    let nonce = Nonce {
                        anchor: state.anchor,
                        index: state.next_index,
                    };
                    state.next_index += 1;
                    state.in_flight += 1;
                    return Ok(NonceLease {
                        nonce,
                        inner: Arc::clone(&self.0),
                    });
                }
            }
            drained.await;
        }
    }

    /// Realigns with `GET /v1/account/nonce-state` after [`NonceExhausted`] or an
    /// `InvalidNonceAnchor` reply. Both values must be read after the error, never guessed.
    ///
    /// The first free pair on chain is `(nonce_anchor, current_bitmap_index)`, or
    /// `(nonce_anchor + 1, 0)` when the index is 208 (anchor full). The manager only moves
    /// when its own position is unusable or behind that pair, so concurrent or stale calls
    /// are harmless.
    pub fn resync(&self, nonce_anchor: u64, current_bitmap_index: u32) {
        let next_anchor = nonce_anchor.saturating_add(1);
        let chain = match u8::try_from(current_bitmap_index) {
            Ok(index) if index < NONCE_SLOTS => (nonce_anchor, index),
            _ => (next_anchor, 0),
        };
        let mut state = self.0.state.lock();
        if state.anchor > next_anchor {
            // A read older than our last move.
            return;
        }
        let local = (state.anchor, state.next_index);
        let target = if state.anchor >= nonce_anchor && state.next_index < NONCE_SLOTS {
            // Our anchor is still accepted: only skip pairs another client used.
            local.max(chain)
        } else if chain.0 == state.anchor {
            // Bits still free on an exhausted anchor are holes between used ones, so take
            // the next anchor, which is unused by definition.
            // shortcut: when that is our own anchor again (nothing of it landed) it restarts
            // from index 0; a pair that lands late gives `NonceUsed` on the reuse. Track
            // handed-out pairs against the nonce-state bitmap if that shows up.
            (next_anchor, 0)
        } else {
            chain
        };
        if target.0 != state.anchor {
            state.draining = state.in_flight > 0;
        }
        (state.anchor, state.next_index) = target;
    }
}

impl Drop for NonceLease {
    fn drop(&mut self) {
        let mut state = self.inner.state.lock();
        state.in_flight -= 1;
        if state.in_flight == 0 && state.draining {
            state.draining = false;
            self.inner.drained.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    use super::*;

    async fn take(manager: &NonceManager) -> Nonce {
        manager.next().await.unwrap().nonce
    }

    #[tokio::test]
    async fn indexes_0_to_207_then_needs_new_anchor() {
        let manager = NonceManager::new(10);
        for index in 0..NONCE_SLOTS {
            assert_eq!(take(&manager).await, Nonce { anchor: 11, index });
        }
        assert_eq!(manager.next().await.unwrap_err(), NonceExhausted);

        // A stale read (chain anchor behind ours by more than one) changes nothing.
        manager.resync(5, 0);
        assert!(manager.next().await.is_err());

        // Our requests landed: the chain anchor is now ours and full, move one past it.
        manager.resync(11, 208);
        assert_eq!(
            take(&manager).await,
            Nonce {
                anchor: 12,
                index: 0
            }
        );
        // A second caller that saw the same exhaustion must not skip pairs.
        manager.resync(11, 208);
        assert_eq!(
            take(&manager).await,
            Nonce {
                anchor: 12,
                index: 1
            }
        );
    }

    #[tokio::test]
    async fn resync_handles_chain_ahead_and_unmoved() {
        let manager = NonceManager::new(10);
        assert_eq!(
            take(&manager).await,
            Nonce {
                anchor: 11,
                index: 0
            }
        );
        // Chain on our anchor and indexes left: keep going.
        manager.resync(11, 1);
        assert_eq!(
            take(&manager).await,
            Nonce {
                anchor: 11,
                index: 1
            }
        );
        // Chain past our anchor (another client signed): ours is rejected.
        manager.resync(20, 3);
        assert_eq!(
            take(&manager).await,
            Nonce {
                anchor: 20,
                index: 3
            }
        );
        // The same, but that anchor is full.
        manager.resync(30, 208);
        assert_eq!(
            take(&manager).await,
            Nonce {
                anchor: 31,
                index: 0
            }
        );
        // Another client used pairs on our anchor: skip them.
        manager.resync(31, 5);
        assert_eq!(
            take(&manager).await,
            Nonce {
                anchor: 31,
                index: 5
            }
        );

        // Nothing landed on an exhausted anchor: reuse it from index 0.
        let manager = NonceManager::new(10);
        for _ in 0..NONCE_SLOTS {
            take(&manager).await;
        }
        manager.resync(10, 208);
        assert_eq!(
            take(&manager).await,
            Nonce {
                anchor: 11,
                index: 0
            }
        );
    }

    #[tokio::test]
    async fn resync_on_exhausted_anchor_uses_both_nonce_state_values() {
        let exhausted = || async {
            let manager = NonceManager::new(10);
            for _ in 0..NONCE_SLOTS {
                take(&manager).await;
            }
            manager
        };

        // Chain on our anchor with free bits left (failed requests): those are holes, so
        // go to the next anchor instead of restarting this one.
        let manager = exhausted().await;
        manager.resync(11, 200);
        assert_eq!(
            take(&manager).await,
            Nonce {
                anchor: 12,
                index: 0
            }
        );

        // Nothing of ours landed and the chain anchor has room: use its first free pair.
        let manager = exhausted().await;
        manager.resync(10, 7);
        assert_eq!(
            take(&manager).await,
            Nonce {
                anchor: 10,
                index: 7
            }
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn concurrent_callers_never_share_a_pair() {
        const TASKS: usize = 16;
        const PER_TASK: usize = 200;
        let manager = NonceManager::new(0);
        // Stand-in for the chain: the highest anchor a request has landed on.
        let chain = Arc::new(AtomicU64::new(0));
        let tasks: Vec<_> = (0..TASKS)
            .map(|_| {
                let (manager, chain) = (manager.clone(), Arc::clone(&chain));
                tokio::spawn(async move {
                    let mut seen = Vec::with_capacity(PER_TASK);
                    while seen.len() < PER_TASK {
                        match manager.next().await {
                            Ok(lease) => {
                                tokio::task::yield_now().await;
                                chain.fetch_max(lease.nonce.anchor, Ordering::SeqCst);
                                seen.push(lease.nonce);
                            }
                            Err(NonceExhausted) => {
                                manager.resync(chain.load(Ordering::SeqCst), 208);
                            }
                        }
                    }
                    seen
                })
            })
            .collect();

        let mut all = HashSet::new();
        for task in tasks {
            for nonce in task.await.unwrap() {
                assert!(nonce.index < NONCE_SLOTS);
                assert!(all.insert(nonce), "duplicate {nonce:?}");
            }
        }
        assert_eq!(all.len(), TASKS * PER_TASK);
    }

    #[tokio::test]
    async fn new_anchor_waits_for_old_anchor_requests() {
        let manager = NonceManager::new(10);
        let first = manager.next().await.unwrap();
        let second = manager.next().await.unwrap();
        manager.resync(15, 208);

        let waiter = tokio::spawn({
            let manager = manager.clone();
            async move { take(&manager).await }
        });
        drop(first);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiter.is_finished(),
            "issued a new-anchor nonce with an old request in flight"
        );

        drop(second);
        assert_eq!(
            waiter.await.unwrap(),
            Nonce {
                anchor: 16,
                index: 0
            }
        );
    }
}
