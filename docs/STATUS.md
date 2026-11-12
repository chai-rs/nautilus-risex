# Status

Date: 13 Nov 2026 (first version: 10 Nov 2026). Plan:
`deltanuetral/.strategy/research/05-risex-adapter-plan.md`, steps 0 to 8. Step 9 (mainnet) is not
started.

## Summary

The crate builds and all offline tests pass. **No signed request went to RISEx, on testnet or on
mainnet.** The only venue contact was public, read-only data: the fixtures in `tests/fixtures/`
and two runs of `data_tester` (10 Nov and 13 Nov 2026).

Signing is confirmed for two actions only: place order and cancel order. The proof is a set of
real mainnet transactions whose hashes and signers the tests reproduce. Each other signed body
follows the documentation alone.

The update of 12 Nov 2026 (next section) made no request to the venue at all. It uses the
fixtures recorded on 10 Nov 2026.

## Added on 13 Nov 2026

Tooling only. The adapter itself did not change.

| Item | What changed | Verified by | Not verified |
|---|---|---|---|
| Offline tests of the binaries | `risex-flatten`: the plan (`Plan`) and the closing order (`closing_order`) are functions with tests. `risex-trades-probe`: the arguments, the market lookup and the printed rows are functions with tests. `risex-signer-revoke`: the request body is the function `revoke_request` in `src/execution/revoke.rs`, with tests there. The two tested binaries are test targets now (`test = false` removed in `Cargo.toml`). | 9 new tests on the fixtures and the Anvil key, no network: 4 in `bin/flatten.rs`, 3 in `bin/trades_probe.rs`, 2 in `src/execution/revoke.rs`. The revocation signature recovers to the Anvil address over the Python reference digest. | `main` of each binary: the prompt, the choice of the environment from the first argument, and the paging loops of `risex-flatten`. No binary ran against a venue. |
| CI | `check` job: clippy and test also with `--features examples`. New `fuzz` job: nightly and `cargo-fuzz` on the runner, each target of `fuzz/fuzz_targets` for 30 seconds. | The YAML parses. The same clippy and test commands pass here. | The workflow never ran: nothing is pushed. The `fuzz` job in particular is untested. |
| `data_tester` | Subscribes to the instrument status of AERO and BTC. | One keyless run of 72 seconds on public mainnet data: one `InstrumentStatus` per instrument on subscribe, both `Trading`. | A status change, and each state other than `Trading`. |

The printed output of the binaries and the order of their requests did not change. `risex-flatten`
still prints the plan first and opens the signing session only after Enter.

## Added on 12 Nov 2026

Four gaps against `nautilus-lighter` were examined. Three are closed; the fourth needed no change.

| Feature | What changed | Verified by | Not verified |
|---|---|---|---|
| Instrument status | `subscribe_instrument_status` / `unsubscribe_instrument_status` on the data client. While a status is subscribed, the client polls `GET /v1/markets` and `GET /v1/system/config` and sends `InstrumentStatus` on subscribe and on each change. New config field `instrument_status_poll_interval_secs` (default 60), also in the Python binding. | Unit test on the recorded `markets.json` (52 trading, 1 post-only, 1 inactive). Mock-server test: status on subscribe, then a hand-made flag change. | Locked, reduce-only and maintenance states: hand-made inputs only. No live run. |
| Live position reports | The execution client subscribes to the `positions` channel and sends a `PositionStatusReport` per row of its account. REST stays for reconciliation. A refused `positions` subscription is logged and the client stays connected. | Unit test and mock-server test on a recorded long row. Mock-server test of the refusal. | Short and closed rows: hand-made from the docs. |
| Leverage and margin mode at run time | `RisexExecutionClient::update_leverage` and `update_margin_mode`. They send the same signed requests as connect does. | Mock-server test: the mock recomputes the digest and recovers the signer. | The signatures against the venue (as before). |
| Contingent order lists | No change. See below. | Read of `nautilus-execution` 0.67.0 and of the adapter. No new test. | — |

Status values: inactive market → `NotAvailableForTrading`; locked → `Halt`; maintenance or
post-only → `Pause` with `is_quoting = true`; reduce-only → `Trading` with reason `reduce_only`;
else `Trading`. The reason field has the cause.

Contingent order lists: the venue has no group endpoint, so the orders of a list still go out one
after the other with no link. The adapter loses nothing that Nautilus needs. The engine links an
OTO child to the position by the `OrderFilled` event of the parent, and the order emulator acts
on `OrderDenied`, `OrderRejected`, `OrderCanceled`, `OrderExpired`, `OrderUpdated`,
`OrderModifyRejected` and `OrderFilled`. For every order it submits, the adapter sends each of
these with the Nautilus client order id: as a typed event from the `orders` and `fills` channels,
and as a report that the engine reconciles for a canceled TP/SL order and after a reconnect. Two
limits stay:

- The adapter sends each child at once. It does not wait for the fill of the parent, and it does
  not cancel the children when the parent is denied or rejected.
- Nothing cancels the other leg of an OCO pair when one leg fills. For contingent behavior, give
  the child orders an `emulation_trigger` so that the Nautilus order emulator holds them. That
  route has no test in this crate.

## Verification

| Check | Result |
|---|---|
| `cargo fmt --check` (root and `fuzz/`) | Clean |
| `cargo clippy --all-targets -- -D warnings` | Clean |
| `cargo clippy --all-targets --features examples -- -D warnings` | Clean |
| `cargo test --features examples` | 210 passed: 181 unit, 4 `risex-flatten`, 3 `risex-trades-probe`, 2 data client, 20 execution client |
| `cargo bench --no-run` | Builds 4 benches |
| `cargo check --manifest-path fuzz/Cargo.toml` | Passes (type check only) |
| `cargo clippy --features python` (with the patch, see Python) | Clean |
| `cargo test --features python --test python` (with the patch) | 1 passed |

The tests use the well-known Anvil test key and a mock server on `127.0.0.1`. They open no
connection to the venue.

`data_tester` ran once against public mainnet data on 10 Nov 2026: 53 instruments, 908 book
updates, 238 quotes, 24 trades, 152 mark prices, 152 index prices, 32 funding rates, 2 bars, and
no checksum failure. The venue refused the weekly bar subscription.

`data_tester` ran a second time on 13 Nov 2026, for 72 seconds, with the instrument status
subscription: 53 instruments, 2 instrument statuses (AERO and BTC, both `Trading`), 1076 book
depths, 335 quotes, 60 trades, 134 mark prices, 134 index prices, 26 funding rates, 2 bars, and
no warning or error in the log. The status did not change during the run, so the poll sent no
second status.

## Done, by plan step

| Step | Content | State |
|---|---|---|
| 0 | Crate, toolchain 1.99.1, CI workflow, fixtures | Done |
| 1 | EIP-712 signing, nonce manager, permit | Done; place and cancel confirmed on mainnet payloads |
| 2 | HTTP client, models, rate limit, instruments | Done |
| 3 | WebSocket client, local book, CRC32, recovery | Done; one live run of public data |
| 4 | Data client | Done; instrument status: mock server, and one live run that showed `Trading` only |
| 5 | Execution client: place, cancel, cancel-all, events | Done against the mock server only |
| 6 | Modify, batch, TP/SL, leverage, margin mode (connect and run time), reconciliation | Done against the mock server only |
| 7 | Binaries, examples, Python binding | Done; offline tests of the decision logic of the binaries, not of their `main` |
| 8 | Benches, fuzz targets, README, this file | Done; no fuzz target ran (the CI job for it has not run yet) |
| 9 | Mainnet with a small size | Not started; the owner of the account runs it |

Many "done when" conditions of the plan need testnet. None of those conditions is met.

## The testnet gate

Do these before any mainnet order. Each needs a registered session signer and its key, so the
owner of the account runs them.

1. Run `exec_tester` on testnet with `DRY_RUN = false` and the minimum size.
2. Compare each request body and each response with `src/http/models.rs`.
3. Go through the list in the next section and remove each `// unverified:` marker that the
   run settles.
4. Run `risex-flatten testnet` and confirm that the account has no order and no position.

## Not verified against the venue

Code markers (`// unverified:`):

| Location | Open point |
|---|---|
| `src/execution/mod.rs:91` | `scOrderId` of the place response = `wide_order_id` of the hex id |
| `src/execution/session.rs:142` | The API guard for a cancel that reuses a resting slot |
| `src/execution/session.rs:174` | Leverage signature |
| `src/execution/session.rs:200` | Margin-mode signature |
| `src/execution/session.rs:226` | TP/SL signature and enum codes |
| `src/execution/session.rs:261` | Accepted range of `deadline` |
| `src/execution/order.rs:203` | `uint8` codes of TP/SL, inferred from the OpenAPI enum order |
| `src/execution/revoke.rs:128` | Body and signature of the signer revocation |
| `src/http/client.rs:330` | POST responses: with or without the `data` envelope |
| `src/http/models.rs:608` | Shapes of all signed request and response bodies |
| `src/execution/mod.rs:206`, `src/websocket/dispatch.rs:273` | Reject reason of a post-only order that crosses |
| `src/execution/mod.rs:273` | Reuse of the client order id after a modify |
| `src/execution/mod.rs:476`, `:1198` | Unit of the REST time filters (the adapter filters locally) |
| `src/execution/mod.rs:621` | Link from a triggered TP/SL order to its fills |
| `src/http/parse.rs:87` | Meaning of `config.unlocked`, `reduce_only` and maintenance for the instrument status |
| `src/websocket/account_state.rs:45` | `positions` rows of a short and of a closed position |

Other open points, with no marker:

- Reconnect and the REST fallback on a live socket. Tests cover them on the mock server only.
- Bar `time`: open or close of the bar. The server cap on bars per response.
- `margin_maint = 1 / maintenance_margin_factor` is an inference.
- The account channels deliver without WS auth on the `makers` filter. That is observed
  behavior, not an API contract: the docs of the `positions` channel say that authentication is
  required. The adapter has no WS auth. A refused `orders`, `fills` or `account` subscription
  disconnects the client. A refused `positions` subscription does not.
- The run-time leverage and margin-mode methods use the unverified signatures of
  `src/execution/session.rs:174` and `:200`.
- The `positions` channel had no live run. Instrument status had one (13 Nov 2026) that showed
  `Trading` only: no change and no other state. The venue caches `GET /v1/markets` for 5 minutes
  (from the docs), so a market flag can show that late.
- Testnet payload shapes. All fixtures are from mainnet.
- HTTP 404 on an unknown order id. The on-chain flag bit `0x20`.

## Not done

Compared with `nautilus-lighter`:

- WS auth.
- Native batch and native modify, dead man's switch, STP `reduce`, spot: not on the venue.
- Transaction quota per address: the venue has one; the adapter only limits REST requests.

Compared with the plan:

- Contingent orders (OTO, OCO, bracket): orders of a list go out in sequence with no link. The
  venue has no group endpoint; see "Added on 12 Nov 2026".
- Refresh of the instrument definitions on an interval: the status poll sends statuses only.
- Modify of a trigger order. `OrderTriggered` event. GTD for TP/SL.
- TP/SL pre-check of the position, and the `allowance-status` check.
- `PlaceStopOrder`. `builder_fee_bps`.
- 30m and 12h bars. Weekly bars fail at the venue.
- `tests/stress/book_stress.rs`.
- Tests of `main` of the three binaries (prompt, environment argument) and of the paging loops
  of `risex-flatten`. The decision logic has offline tests; see "Added on 13 Nov 2026".

## Shortcuts in the code

Each has a `// shortcut:` comment with the limit and the upgrade condition.

| Location | Limit |
|---|---|
| `src/data/mod.rs:241` | REST snapshot has 250 levels per side; a deeper book fails the checksum on that path |
| `src/data/mod.rs:321` | Instrument status is polled: a change shows after up to one interval plus the venue cache |
| `src/execution/mod.rs:811` | Modify reads the open size before the cancel |
| `src/execution/mod.rs:1006` | No WS auth |
| `src/execution/mod.rs:177` | REST balance shows as all free until the `account` channel reports |
| `src/signing/action.rs:83` | No `builder_fee_bps` |
| `src/signing/nonce.rs:128` | Nonce restart when the venue returns the same anchor |
| `src/common/rate_limit.rs:26` | The first window can pass up to 2 times the limit |
| `src/http/client.rs:662` | One bars request; the server cap is not known |
| `src/book/sync.rs:28` | 20 bits for the log index |
| `src/websocket/dispatch.rs:112` | Closed orders stay tracked until the map is full |
| `src/websocket/messages.rs:67` | Two decode passes per frame |
| `bin/flatten.rs:136` | One IOC order per position; a thin book leaves a rest |

## Python

The `python` feature compiles and its test passes, with one condition. The published
`nautilus-core` 0.67.0 needs `pyo3-stub-gen =0.20.0`, and that crate does not compile with
`pyo3` 0.30.1. NautilusTrader carries a local patch for it (`patches/pyo3-stub-gen`, root
`Cargo.toml`, commit `d53849b216e20f2c122ca01b4dd0da377638b186`, read on 10 Nov 2026). The
published crates do not carry the patch, so the build needs it on the command line. The command
is in `README.md`.

### Can a separate wheel give the adapter to a Python node? No.

Read on 13 Nov 2026. Nothing was built for this answer; it comes from the 0.67.0 sources. Paths
that start with `nautilus-` or `pyo3-` are under `~/.cargo/registry/src/index.crates.io-*/`.
"Lighter" is the official `nautilus-lighter` 0.67.0 source.

**A wheel built from this crate alone cannot give its factories to the node of an installed
`nautilus_trader`. The adapter must be compiled into the same extension module as
`nautilus-live`.** The reasons:

1. The Python node is `nautilus_trader.live.LiveNode`
   (`nautilus-live-0.67.0/src/python/node.rs:108`), built by `LiveNodeBuilder` (`:2228`). The
   name `TradingNode` is not in `nautilus-live`, `nautilus-system` or `nautilus-common` 0.67.0.
2. `LiveNodeBuilder.add_data_client` and `add_exec_client` change the Python factory object into
   a Rust trait object through the registry of their own binary (`node.rs:2411`, `:2455-2456`,
   `:2502`, `:2546-2547`).
3. That registry is a Rust `static` (`nautilus-system-0.67.0/src/python/registry.rs:255-262`).
   Its entries are Rust function pointers (`:28-40`), looked up by the text that
   `factory.name()` returns (`:149-167`). `nautilus-system` shows nothing of it to Python: its
   only Python items are the controller classes (`src/python/controller.rs:37-76`). So only Rust
   code in the same binary can add an entry.
4. `nautilus-system`, `nautilus-live` and `nautilus-common` are `rlib` only
   (`nautilus-system-0.67.0/Cargo.toml:88-91`, `nautilus-live-0.67.0/Cargo.toml:128-131`,
   `nautilus-common-0.67.0/Cargo.toml:101-104`). A second extension module links its own copy
   of each, with its own registry. `risex()` (`src/python/mod.rs:98-110`) fills that copy. The
   node of the installed package then answers `No factory extractor registered for 'RISEX'`
   (`registry.rs:164-166`).
5. The registry is not the only state of this kind. A client sends its events through
   thread-local senders that the runner of the node sets
   (`nautilus-common-0.67.0/src/live/runner.rs:228-232`, set in
   `nautilus-live-0.67.0/src/runner.rs:346`; the getter panics when the sender is not set,
   `runner.rs:38-45` of `nautilus-common`) and runs its tasks on a static Tokio
   runtime (`src/live/runtime.rs:57`, `:145`). This adapter uses both (`src/data/mod.rs:417`,
   `src/execution/mod.rs:925`). In a second extension these are other copies, which the node
   never sets. So a way to pass the factory across would not be enough.
6. `nautilus-lighter` and `nautilus-hyperliquid` register as this crate does: in Rust, inside
   their `#[pymodule]` function (lighter `src/python/mod.rs:185-243`;
   `nautilus-hyperliquid-0.67.0/src/python/mod.rs:209-280`). Both are `rlib` only (lighter
   `Cargo.toml:94-96`; `nautilus-hyperliquid-0.67.0/Cargo.toml:91-94`), and the lighter module
   is "Exposed through `nautilus_trader.adapters.lighter`" (`mod.rs:184`). They are parts of
   the one NautilusTrader extension, not wheels of their own.

A build that Python can load needs:

- One `cdylib` that contains this crate and `nautilus-live`: a small new crate, or
  `crate-type = ["cdylib", "rlib"]` here. It needs the feature `extension-module` of this crate
  and one top-level `#[pymodule]` that adds the module functions in use: at least
  `nautilus_live::python::live` (`nautilus-live-0.67.0/src/python/mod.rs:54-55`),
  `nautilus_model::python::model` (`nautilus-model-0.67.0/src/python/mod.rs:87-88`),
  `nautilus_common::python::common` (`nautilus-common-0.67.0/src/python/mod.rs:70-71`) and
  `nautilus_risex::python::risex`.
- The `pyo3-stub-gen` patch (above) and a wheel builder such as maturin. Maturin is not
  installed here.
- Strategies that import the node and the model types from that extension, not from an
  installed `nautilus_trader`. The two do not mix in one process: each `#[pyclass]` has one
  type object per binary (`pyo3-macros-backend-0.30.1/src/pyclass.rs:3091-3093`), so a
  `LiveNode` or an `InstrumentId` of one extension is not that of the other. The node works
  when the Python package `nautilus_trader` is absent (`node.rs:2748-2750`, `:2762-2764`).

That is a private build of the NautilusTrader extension with one more adapter. The other form of
the same thing is to add this crate to the NautilusTrader workspace and build `nautilus_trader`
from source. The crate that assembles the official module is not on crates.io and was not read.

One route needs no shared binary, but it does not use the clients of this crate.
`add_data_client` and `add_exec_client` also take a factory that subclasses the Python class
`nautilus_trader.live.clients.DataClientFactory` (or `ExecutionClientFactory`); the node then
drives a client that is written in Python (`node.rs:2412-2453`, `:2755-2773`;
`nautilus-live-0.67.0/src/python/client/mod.rs:1085-1113`, `:1186-1190`). A separate wheel can
use that route with a new Python client. This crate has no such client.

Not verified: nothing was built or loaded. The package `nautilus_trader` is not installed on
this machine, so its Python side (`nautilus_trader.live.clients`) was not read.

The Rust `LiveNode` does not have this problem (see `examples/`).

`revoke_risex_signer` has no confirmation prompt and no test.

The module function `risex` registers its extractors in a process-wide registry. A second call
in one process fails with `Factory extractor 'RISEX' is already registered`.

## Benches

One short run on an Apple-silicon Mac, 10 Nov 2026. The numbers are indicative only.

| Bench | Time |
|---|---|
| `signing/permit` (full EIP-712 signature) | 48.0 µs |
| `signing/action_hash` | 462 ns |
| `signing/cancel_order_hash` | 317 ns |
| `decode/book_snapshot_517` (29.6 KB frame) | 156 µs |
| `decode/book_update` | 1.35 µs |
| `book/checksum_517` | 26.6 µs |
| `book/apply_snapshot_517` | 24.4 µs |
| `exec/place_order` (build the request, no signature) | 193 ns |

`book/apply_update` repeats one delete, so it measures the removal of an absent level.

## Fuzz

Five targets type-check on 1.99.1 and on nightly. **No target ran**: `cargo-fuzz` is not
installed here. There is no seed corpus. `src/book/sync.rs` and `src/book/recovery.rs` have no
fuzz target.

The `fuzz` job of `.github/workflows/ci.yml` installs nightly and `cargo-fuzz` on the runner and
runs each target for 30 seconds (`-max_total_time=30`). The job has not run yet: nothing is
pushed. It does not upload the crash input of a failed target; the log has the input.
