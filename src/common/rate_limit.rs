//! REST rate-limit quota for RISEx.

use std::{num::NonZeroU32, sync::LazyLock, time::Duration};

use nautilus_network::ratelimiter::quota::Quota;

/// Rate-limit bucket key shared by every REST request.
pub const RISEX_REST_BUCKET: &str = "risex:rest";

/// Window the venue counts REST requests over.
pub const RISEX_REST_WINDOW: Duration = Duration::from_secs(10);

/// Venue REST limit: requests per [`RISEX_REST_WINDOW`].
pub const RISEX_REST_QUOTA_PER_10S: NonZeroU32 = NonZeroU32::new(200).unwrap();

/// Default REST quota (200 requests per 10 seconds).
pub static RISEX_REST_QUOTA: LazyLock<Quota> = LazyLock::new(|| resolve_quota(None));

/// Returns the REST quota for `per_10s` requests per 10 seconds, or the venue default when
/// the value is unset or zero.
#[must_use]
pub fn resolve_quota(per_10s: Option<u32>) -> Quota {
    let burst = per_10s
        .and_then(NonZeroU32::new)
        .unwrap_or(RISEX_REST_QUOTA_PER_10S);
    // shortcut: a full burst plus the refill can pass up to 2x the limit in the first window
    // (same as the Lighter quota); halve `rest_quota_per_10s` if the venue starts answering 429.
    Quota::with_period(RISEX_REST_WINDOW / burst.get())
        // Unreachable: 10 s divided by any u32 is never zero.
        .unwrap_or_else(|| Quota::per_minute(burst))
        .allow_burst(burst)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(None)]
    #[case(Some(0))]
    fn test_resolve_quota_defaults_when_unset_or_zero(#[case] per_10s: Option<u32>) {
        let quota = resolve_quota(per_10s);
        assert_eq!(quota.burst_size().get(), 200);
        assert_eq!(quota.replenish_interval(), Duration::from_millis(50));
    }

    #[rstest]
    fn test_resolve_quota_uses_override() {
        let quota = resolve_quota(Some(50));
        assert_eq!(quota.burst_size().get(), 50);
        assert_eq!(quota.replenish_interval(), Duration::from_millis(200));
    }
}
