# Status

Date: 10 Nov 2026. Plan: `deltanuetral/.strategy/research/05-risex-adapter-plan.md`, steps 0 to 8.
Step 9 (mainnet) is not started.

## Summary

The crate builds and all offline tests pass. **No signed request went to RISEx, on testnet or on
mainnet.** The only venue contact was public, read-only data: the fixtures in `tests/fixtures/`
and one run of `data_tester`.

Signing is confirmed for two actions only: place order and cancel order. The proof is a set of
real mainnet transactions whose hashes and signers the tests reproduce. Each other signed body
follows the documentation alone.

## Verification

| Check | Result |
|---|---|
| `cargo fmt --check` (root and `fuzz/`) | Clean |
| `cargo clippy --all-targets -- -D warnings` | Clean |
| `cargo clippy --all-targets --features examples -- -D warnings` | Clean |
| `cargo test --features examples` | 192 passed: 174 unit, 1 data client, 17 execution client |
| `cargo bench --no-run` | Builds 4 benches |
| `cargo check --manifest-path fuzz/Cargo.toml` | Passes (type check only) |
| `cargo clippy --features python` (with the patch, see Python) | Clean |
| `cargo test --features python --test python` (with the patch) | 1 passed |

The tests use the well-known Anvil test key and a mock server on `127.0.0.1`. They open no
connection to the venue.

`data_tester` ran once against public mainnet data on 10 Nov 2026: 53 instruments, 908 book
updates, 238 quotes, 24 trades, 152 mark prices, 152 index prices, 32 funding rates, 2 bars, and
no checksum failure. The venue refused the weekly bar subscription.

## Done, by plan step

| Step | Content | State |
|---|---|---|
| 0 | Crate, toolchain 1.99.1, CI workflow, fixtures | Done |
| 1 | EIP-712 signing, nonce manager, permit | Done; place and cancel confirmed on mainnet payloads |
| 2 | HTTP client, models, rate limit, instruments | Done |
| 3 | WebSocket client, local book, CRC32, recovery | Done; one live run of public data |
| 4 | Data client | Done; instrument status is not implemented |
| 5 | Execution client: place, cancel, cancel-all, events | Done against the mock server only |
| 6 | Modify, batch, TP/SL, leverage, margin mode, reconciliation | Done against the mock server only |
| 7 | Binaries, examples, Python binding | Done; no binary has an offline test |
| 8 | Benches, fuzz targets, README, this file | Done; no fuzz target ran |
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
| `src/execution/mod.rs:88` | `scOrderId` of the place response = `wide_order_id` of the hex id |
| `src/execution/session.rs:142` | The API guard for a cancel that reuses a resting slot |
| `src/execution/session.rs:174` | Leverage signature |
| `src/execution/session.rs:200` | Margin-mode signature |
| `src/execution/session.rs:226` | TP/SL signature and enum codes |
| `src/execution/session.rs:261` | Accepted range of `deadline` |
| `src/execution/order.rs:203` | `uint8` codes of TP/SL, inferred from the OpenAPI enum order |
| `src/execution/revoke.rs:107` | Body and signature of the signer revocation |
| `src/http/client.rs:330` | POST responses: with or without the `data` envelope |
| `src/http/models.rs:608` | Shapes of all signed request and response bodies |
| `src/execution/mod.rs:203`, `src/websocket/dispatch.rs:273` | Reject reason of a post-only order that crosses |
| `src/execution/mod.rs:270` | Reuse of the client order id after a modify |
| `src/execution/mod.rs:473`, `:1120` | Unit of the REST time filters (the adapter filters locally) |
| `src/execution/mod.rs:601` | Link from a triggered TP/SL order to its fills |

Other open points, with no marker:

- Reconnect and the REST fallback on a live socket. Tests cover them on the mock server only.
- Bar `time`: open or close of the bar. The server cap on bars per response.
- `margin_maint = 1 / maintenance_margin_factor` is an inference.
- The account channels deliver without WS auth on the `makers` filter. That is observed
  behavior, not an API contract. The adapter has no WS auth. A refused subscription
  disconnects the client.
- Testnet payload shapes. All fixtures are from mainnet.
- HTTP 404 on an unknown order id. The on-chain flag bit `0x20`.

## Not done

Compared with `nautilus-lighter`:

- WS auth.
- Live position reports from the `positions` channel (REST only).
- Public leverage and margin-mode methods at run time (config at connect only).
- Native batch and native modify, dead man's switch, STP `reduce`, spot: not on the venue.
- Transaction quota per address: the venue has one; the adapter only limits REST requests.

Compared with the plan:

- Instrument status poll.
- Contingent orders (OTO, OCO, bracket): orders of a list go out in sequence with no link.
- Modify of a trigger order. `OrderTriggered` event. GTD for TP/SL.
- TP/SL pre-check of the position, and the `allowance-status` check.
- `PlaceStopOrder`. `builder_fee_bps`.
- 30m and 12h bars. Weekly bars fail at the venue.
- `tests/stress/book_stress.rs`.
- Offline tests of the three binaries.

## Shortcuts in the code

Each has a `// shortcut:` comment with the limit and the upgrade condition.

| Location | Limit |
|---|---|
| `src/data/mod.rs:234` | REST snapshot has 250 levels per side; a deeper book fails the checksum on that path |
| `src/execution/mod.rs:783` | Modify reads the open size before the cancel |
| `src/execution/mod.rs:929` | No WS auth |
| `src/execution/mod.rs:174` | REST balance shows as all free until the `account` channel reports |
| `src/signing/action.rs:83` | No `builder_fee_bps` |
| `src/signing/nonce.rs:128` | Nonce restart when the venue returns the same anchor |
| `src/common/rate_limit.rs:26` | The first window can pass up to 2 times the limit |
| `src/http/client.rs:662` | One bars request; the server cap is not known |
| `src/book/sync.rs:28` | 20 bits for the log index |
| `src/websocket/dispatch.rs:112` | Closed orders stay tracked until the map is full |
| `src/websocket/messages.rs:67` | Two decode passes per frame |
| `bin/flatten.rs:139` | One IOC order per position; a thin book leaves a rest |

## Python

The `python` feature compiles and its test passes, with one condition. The published
`nautilus-core` 0.67.0 needs `pyo3-stub-gen =0.20.0`, and that crate does not compile with
`pyo3` 0.30.1. NautilusTrader carries a local patch for it (`patches/pyo3-stub-gen`, root
`Cargo.toml`, commit `d53849b216e20f2c122ca01b4dd0da377638b186`, read on 10 Nov 2026). The
published crates do not carry the patch, so the build needs it on the command line. The command
is in `README.md`.

Not confirmed: a route to a wheel that a Python `TradingNode` can load. This crate has no
`cdylib` target and no top-level module. A separate extension module has its own copy of the
Nautilus PyO3 registry, so the official wheel cannot see its factories. The Rust `LiveNode` does
not have this problem.

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
