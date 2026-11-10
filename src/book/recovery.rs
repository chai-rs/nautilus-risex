//! Recovery policy for a book that lost sync.
//!
//! Pure state: no I/O and no timers. The caller passes the current time and carries out the
//! returned action. One [`BookRecovery`] per market.

use std::time::Duration;

use nautilus_core::UnixNanos;

/// What the caller must do next for the market.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Unsubscribe the book channel, then subscribe again. The venue sends no new snapshot for
    /// a subscribe to a market that is still subscribed.
    Resubscribe,
    /// Close the socket and connect again.
    ReconnectSocket,
    /// Stop: log an error and leave the market without book data.
    Halt,
    /// The WebSocket snapshot did not arrive in time: read the book over REST.
    FetchRestSnapshot,
}

/// Limits of the recovery policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryConfig {
    /// Resubscribes allowed inside `resubscribe_window` before the socket is reconnected.
    pub max_resubscribes: usize,
    pub resubscribe_window: Duration,
    /// Wait for a snapshot after a subscribe; zero disables the REST fallback.
    pub snapshot_timeout: Duration,
}

impl Default for RecoveryConfig {
    /// Eight attempts in 180 seconds and a 10 second snapshot wait, the budget that
    /// `nautilus-live` gives the Lighter adapter.
    fn default() -> Self {
        Self {
            max_resubscribes: 8,
            resubscribe_window: Duration::from_secs(180),
            snapshot_timeout: Duration::from_secs(10),
        }
    }
}

/// Escalation state of one market: resubscribe, then reconnect the socket, then halt.
#[derive(Debug)]
pub struct BookRecovery {
    config: RecoveryConfig,
    // Times are Unix nanoseconds
    resubscribes: Vec<u64>,
    reconnected_at: Option<u64>,
    snapshot_deadline: Option<u64>,
    halted: bool,
}

impl BookRecovery {
    #[must_use]
    pub const fn new(config: RecoveryConfig) -> Self {
        Self {
            config,
            resubscribes: Vec::new(),
            reconnected_at: None,
            snapshot_deadline: None,
            halted: false,
        }
    }

    /// Returns whether the market is stopped for good. Make a new [`BookRecovery`] to start
    /// the market again.
    #[must_use]
    pub const fn is_halted(&self) -> bool {
        self.halted
    }

    /// Starts the snapshot wait. Call when a subscribe request is sent.
    pub fn expect_snapshot(&mut self, now: UnixNanos) {
        if !self.halted {
            self.arm(now.as_u64());
        }
    }

    /// Ends the snapshot wait. Call when a snapshot was applied, from WebSocket or REST.
    pub const fn on_snapshot(&mut self) {
        self.snapshot_deadline = None;
    }

    /// Decides the action after a checksum mismatch or a backward sequence.
    ///
    /// [`RecoveryAction::Resubscribe`] and [`RecoveryAction::ReconnectSocket`] also start the
    /// snapshot wait, because both end in a new subscribe.
    pub fn on_desync(&mut self, now: UnixNanos) -> RecoveryAction {
        if self.halted {
            return RecoveryAction::Halt;
        }

        let now = now.as_u64();
        let window = nanos(self.config.resubscribe_window);
        let recent = |at: &u64| now.saturating_sub(*at) < window;
        self.resubscribes.retain(recent);
        self.reconnected_at = self.reconnected_at.filter(recent);

        // A new socket did not help either
        if self.reconnected_at.is_some() {
            self.halted = true;
            self.snapshot_deadline = None;
            return RecoveryAction::Halt;
        }

        self.arm(now);
        if self.resubscribes.len() < self.config.max_resubscribes {
            self.resubscribes.push(now);
            RecoveryAction::Resubscribe
        } else {
            self.reconnected_at = Some(now);
            RecoveryAction::ReconnectSocket
        }
    }

    /// Returns [`RecoveryAction::FetchRestSnapshot`] when the snapshot wait is over. The wait
    /// starts again, so a failed REST read is tried again after another timeout.
    pub fn on_timer(&mut self, now: UnixNanos) -> Option<RecoveryAction> {
        let now = now.as_u64();
        if self.snapshot_deadline? > now {
            return None;
        }

        self.arm(now);
        Some(RecoveryAction::FetchRestSnapshot)
    }

    fn arm(&mut self, now: u64) {
        let timeout = self.config.snapshot_timeout;
        self.snapshot_deadline = (!timeout.is_zero()).then(|| now.saturating_add(nanos(timeout)));
    }
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::{RecoveryAction::*, *};

    fn secs(seconds: u64) -> UnixNanos {
        UnixNanos::from(seconds * 1_000_000_000)
    }

    fn recovery(max_resubscribes: usize, window_secs: u64, timeout_secs: u64) -> BookRecovery {
        BookRecovery::new(RecoveryConfig {
            max_resubscribes,
            resubscribe_window: Duration::from_secs(window_secs),
            snapshot_timeout: Duration::from_secs(timeout_secs),
        })
    }

    #[rstest]
    fn escalates_from_resubscribe_to_reconnect_to_halt() {
        let mut recovery = recovery(3, 180, 10);

        let actions: Vec<_> = (0..7).map(|t| recovery.on_desync(secs(t))).collect();

        assert_eq!(
            actions,
            [
                Resubscribe,
                Resubscribe,
                Resubscribe,
                ReconnectSocket,
                Halt,
                Halt,
                Halt
            ]
        );
        assert!(recovery.is_halted());
    }

    #[rstest]
    fn halted_market_stays_halted_and_waits_for_nothing() {
        let mut recovery = recovery(0, 180, 10);
        assert_eq!(recovery.on_desync(secs(0)), ReconnectSocket);
        assert_eq!(recovery.on_desync(secs(1)), Halt);

        recovery.expect_snapshot(secs(2));
        recovery.on_snapshot();

        assert_eq!(recovery.on_timer(secs(10_000)), None);
        assert_eq!(recovery.on_desync(secs(10_000)), Halt);
    }

    #[rstest]
    fn budget_comes_back_when_the_window_passes() {
        let mut recovery = recovery(2, 10, 10);
        assert_eq!(recovery.on_desync(secs(0)), Resubscribe);
        assert_eq!(recovery.on_desync(secs(1)), Resubscribe);
        // The attempt at 0 s left the window, the one at 1 s did not
        assert_eq!(recovery.on_desync(secs(10)), Resubscribe);
        assert_eq!(recovery.on_desync(secs(10)), ReconnectSocket);

        // A desync after the reconnect left the window starts from the first step
        assert_eq!(recovery.on_desync(secs(20)), Resubscribe);
        assert!(!recovery.is_halted());
    }

    #[rstest]
    fn snapshot_timeout_asks_for_a_rest_snapshot_and_starts_again() {
        let mut recovery = recovery(8, 180, 10);
        assert_eq!(recovery.on_timer(secs(100)), None);

        recovery.expect_snapshot(secs(100));
        assert_eq!(recovery.on_timer(secs(109)), None);
        assert_eq!(recovery.on_timer(secs(110)), Some(FetchRestSnapshot));
        assert_eq!(recovery.on_timer(secs(111)), None);
        assert_eq!(recovery.on_timer(secs(120)), Some(FetchRestSnapshot));

        recovery.on_snapshot();
        assert_eq!(recovery.on_timer(secs(1_000)), None);
    }

    #[rstest]
    fn resubscribe_starts_the_snapshot_wait() {
        let mut recovery = recovery(8, 180, 10);
        assert_eq!(recovery.on_desync(secs(5)), Resubscribe);

        assert_eq!(recovery.on_timer(secs(14)), None);
        assert_eq!(recovery.on_timer(secs(15)), Some(FetchRestSnapshot));
    }

    #[rstest]
    fn zero_timeout_disables_the_rest_fallback() {
        let mut recovery = recovery(8, 180, 0);
        recovery.expect_snapshot(secs(0));
        recovery.on_desync(secs(1));

        assert_eq!(recovery.on_timer(secs(10_000)), None);
    }
}
