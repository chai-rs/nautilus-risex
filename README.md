# nautilus-risex

RISEx perpetuals exchange adapter for NautilusTrader 0.67.0 (Rust). The parity target is
`nautilus-lighter` 0.67.0.

**Status: not run against the venue with a key.** All tests run offline against fixtures and a
local mock server. Signing is confirmed for place and cancel only, on public mainnet payloads.
Read `docs/STATUS.md` before you connect an account.

## Warnings

- `examples/exec_tester.rs` has `DRY_RUN = true`. With `false` it submits orders on **mainnet**
  with real funds.
- `risex-flatten` cancels every order and closes every position of the account. It waits for
  Enter before it sends.
- `risex-signer-revoke` revokes the session signer. The execution client cannot trade with that
  signer afterwards. It waits for Enter before it sends.
- The Python function `revoke_risex_signer` does the same revocation **without a prompt**.
- The session signer key signs orders. The account (wallet) key is necessary for
  `risex-signer-revoke` only. Do not put the account key in the config of the client.

## Run

```bash
cargo test --features examples
```

```bash
cargo run --example data_tester --features examples
```

```bash
cargo run --bin risex-trades-probe -- AERO-USDC-PERP.RISEX 20
```

```bash
cargo run --example exec_tester --features examples
```

```bash
cargo bench --bench signing
```

`data_tester` and `risex-trades-probe` read public mainnet data and use no key. `exec_tester`,
`risex-flatten` and `risex-signer-revoke` need the environment variables below. Pass `testnet`
as the first argument of the two binaries to target testnet.

| Variable | Use |
|---|---|
| `RISEX_ACCOUNT_ADDRESS` | Account (wallet) address |
| `RISEX_SIGNER_PRIVATE_KEY` | Key of the registered session signer |
| `RISEX_ACCOUNT_PRIVATE_KEY` | Wallet key; `risex-signer-revoke` and `revoke_risex_signer` only |
| `RISEX_TESTNET_*` | The same three for testnet |

The process reads the keys at run time only and does not print them. The `Debug` output of the
config redacts the signer key.

## Parity with `nautilus-lighter` 0.67.0

- **Direct**: the venue has an endpoint or a channel for the same job.
- **Emulated**: the adapter builds the feature from other calls. The limits are in the next
  section.
- **Not on venue**: the adapter refuses the command with a reason.
- **Not done**: the venue permits it, but this crate does not implement it.

### Data

| Lighter feature | RISEx | Result |
|---|---|---|
| Instruments | `GET /v1/markets` | Direct |
| Book deltas (`L2_MBP`) | WS `orderbook` + CRC32 check | Direct |
| Book snapshot | `GET /v1/orderbook`, 250 levels per side | Direct |
| Book depth, 10 levels | Cut from the local book | Emulated |
| Quotes | Top of the local book | Emulated |
| Trades | WS `trades` | Direct |
| `request_trades` | `trade-history`, paged | Direct |
| Bars (live) | REST poll at each bar close | Emulated |
| `request_bars` | `trading-view-data`: 1m, 5m, 15m, 1h, 4h, 1d | Direct; 1w, 30m and 12h fail |
| Mark price, index price | WS `oracle` | Direct |
| Funding rate (live) | WS `expected_funding_rate` | Direct |
| `request_funding_rates` | `funding-rate-history` | Direct |
| Book recovery | CRC32 mismatch → unsubscribe, subscribe, bounded retries | Direct |
| Instrument status | — | Not done |
| Spot markets | — | Not on venue |

### Orders

| Lighter feature | RISEx | Result |
|---|---|---|
| `MARKET` | IOC (or FOK) with a price cap = opposite quote ± slippage | Direct |
| `LIMIT`, `post_only`, `reduce_only` | `POST /v1/orders/place` | Direct |
| TIF `GTC`, `IOC`, `FOK` | Same | Direct |
| TIF `GTD` | `GTT` + `ttl_units` of 5 minutes | Direct, 5-minute grain |
| `STOP_MARKET`, `STOP_LIMIT`, `MARKET_IF_TOUCHED`, `LIMIT_IF_TOUCHED` | `POST /v1/orders/tpsl` | Direct, reduce-only, `GTC` only |
| Trigger on mark or last price | `stop_price_option` | Direct |
| Modify | Cancel, then place | Emulated |
| Batch submit, batch cancel | One request after the other | Emulated |
| Cancel-all per instrument | `cancel-all` of the market; one cancel per order when a side is given | Direct / emulated |
| Leverage, margin mode | Set at connect from the config | Direct, no runtime command |
| OTO, OCO, OTOCO, bracket | Orders of a list go out in sequence, with no link | Not done |
| Dead man's switch | — | Not on venue |
| Native batch, native modify, STP `reduce` | — | Not on venue |

### Account and reconciliation

| Lighter feature | RISEx | Result |
|---|---|---|
| Query order | `GET /v1/orders/by-id/{id}` | Direct |
| Query account | `cross-margin-balance` + WS `account` | Direct |
| Order, fill and position reports | REST, paged | Direct |
| Live order and fill events | WS `orders`, `fills` with the `makers` filter | Direct, no WS auth |
| Live position reports from the channel | — | Not done (REST only) |

### Around the adapter

| Lighter | RISEx |
|---|---|
| `lighter-flatten` | `risex-flatten` |
| `lighter-integrator-revoke` | `risex-signer-revoke` |
| `lighter-trades-probe` | `risex-trades-probe` |
| Data and exec tester examples | `data_tester`, `exec_tester` |
| Python config, enum, factories, revoke function | Same list, feature `python` (see below) |
| Mock-server integration tests | `tests/data_client.rs`, `tests/exec_client.rs`, `tests/python.rs` |
| Benches | `data`, `exec`, `micros`, `signing` |
| Fuzz targets | `pack_order`, `action_hash`, `http_parse`, `ws_parse`, `book_checksum` |
| `tests/stress/book_stress.rs` | Not done |

## Limits of the emulated features

- **Modify** is not atomic. The order is off the book between the cancel and the place. The
  adapter reads the open size before the cancel, so a fill in that gap makes the replacement too
  large. Only limit orders can be modified.
- **Batch submit and batch cancel** are not atomic. Some orders can succeed while others fail.
- **Cancel-all without a side** cancels every order of the account in that market, whichever
  strategy sent it.
- **Quotes and depth** come from the local book. They stop while the book recovers.
- **REST book snapshot** has 250 levels per side. A deeper book fails the checksum on that
  fallback path; the WS snapshot does not have this limit.
- **Live bars** cost one REST request per bar close per subscription.
- **Market orders** need a quote of the instrument in the cache. Subscribe to quotes first.
- **TP/SL orders** are polled (`GET /v1/orders/tpsl`), not pushed. The default interval is in
  `RisexExecClientConfig::tpsl_poll_interval_secs`.

## Python

The `python` feature has `RisexEnvironment`, the two configs, the two factories and
`revoke_risex_signer`, registered with the Nautilus PyO3 registry as in `nautilus-lighter`.

The published `nautilus-core` 0.67.0 does not build its `python` feature from crates.io: it
needs `pyo3-stub-gen =0.20.0`, which does not compile with `pyo3` 0.30.1. NautilusTrader patches
that crate in its own workspace (`patches/pyo3-stub-gen`). Get that directory from the
NautilusTrader repository, then give its path to cargo:

```bash
cargo test --features python --test python --config 'patch.crates-io.pyo3-stub-gen.path="/path/to/nautilus_trader/patches/pyo3-stub-gen"'
```

With the patch, the feature compiles, clippy is clean, and the test passes (10 Nov 2026, patch
from commit `d53849b216e20f2c122ca01b4dd0da377638b186`).

Not confirmed: a wheel that a Python `TradingNode` can load. This crate has no `cdylib` target,
and a separate extension module has its own copy of the Nautilus registry. Use the Rust
`LiveNode` (see `examples/`).

## Fuzz

The `fuzz/` crate needs a nightly toolchain and `cargo-fuzz`:

```bash
cargo +nightly fuzz run pack_order
```

## License

LGPL-3.0-only.
