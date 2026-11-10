//! Base URLs and chain ids per RISEx environment.

use crate::common::{
    consts::{RISEX_MAINNET_CHAIN_ID, RISEX_TESTNET_CHAIN_ID},
    enums::RisexEnvironment,
};

/// Returns the REST base URL for the environment.
#[must_use]
pub const fn risex_http_base_url(environment: RisexEnvironment) -> &'static str {
    match environment {
        RisexEnvironment::Mainnet => "https://api.rise.trade",
        RisexEnvironment::Testnet => "https://api.testnet.rise.trade",
    }
}

/// Returns the WebSocket URL for the environment.
#[must_use]
pub const fn risex_ws_url(environment: RisexEnvironment) -> &'static str {
    match environment {
        RisexEnvironment::Mainnet => "wss://ws.rise.trade/ws",
        RisexEnvironment::Testnet => "wss://api.testnet.rise.trade/ws/",
    }
}

/// Returns the chain id used in the EIP-712 domain for the environment.
#[must_use]
pub const fn risex_chain_id(environment: RisexEnvironment) -> u64 {
    match environment {
        RisexEnvironment::Mainnet => RISEX_MAINNET_CHAIN_ID,
        RisexEnvironment::Testnet => RISEX_TESTNET_CHAIN_ID,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(
        RisexEnvironment::Mainnet,
        "https://api.rise.trade",
        "wss://ws.rise.trade/ws",
        4153
    )]
    #[case(
        RisexEnvironment::Testnet,
        "https://api.testnet.rise.trade",
        "wss://api.testnet.rise.trade/ws/",
        11_155_931
    )]
    fn test_urls_per_environment(
        #[case] environment: RisexEnvironment,
        #[case] http: &str,
        #[case] ws: &str,
        #[case] chain_id: u64,
    ) {
        assert_eq!(risex_http_base_url(environment), http);
        assert_eq!(risex_ws_url(environment), ws);
        assert_eq!(risex_chain_id(environment), chain_id);
    }
}
