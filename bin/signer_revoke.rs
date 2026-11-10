//! Revokes the session signer of a RISEx account: use it when leaving the
//! adapter or when the signer key may have leaked. The execution client cannot
//! trade with that signer afterwards.
//!
//! The request is signed by the ACCOUNT key (the wallet), not by the session
//! signer. Nothing is sent before Enter is pressed.
//!
//! Environment selection: pass `testnet` as the first argv to target testnet;
//! defaults to mainnet.
//!
//! Environment variables (read at run time only, never printed):
//! - `RISEX_ACCOUNT_ADDRESS`, `RISEX_SIGNER_PRIVATE_KEY`: the account and the
//!   signer to revoke.
//! - `RISEX_ACCOUNT_PRIVATE_KEY`: the wallet key of the account.
//! - Testnet: the `RISEX_TESTNET_*` variants of all three.
//!
//! Run with:
//!
//! ```bash
//! cargo run --bin risex-signer-revoke           # mainnet
//! cargo run --bin risex-signer-revoke testnet   # testnet
//! ```

use std::{
    env,
    io::{BufRead, Write},
};

use nautilus_risex::{common::enums::RisexEnvironment, execution::revoke::SignerRevocation};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    nautilus_common::logging::ensure_logging_initialized();

    let environment = if env::args().nth(1).is_some_and(|arg| arg == "testnet") {
        RisexEnvironment::Testnet
    } else {
        RisexEnvironment::Mainnet
    };
    let revocation = SignerRevocation::from_env(environment)?;
    let signer = revocation.signer();

    println!();
    println!("RISEx signer revocation");
    println!("  environment : {environment:?}");
    println!("  account     : {}", revocation.account());
    println!("  signer      : {signer}");
    println!("  status      : {}", revocation.status().await?);
    println!();
    println!("Orders signed by this signer are refused once it is revoked.");
    print!("\nProceed? [Enter to continue, Ctrl+C to abort] ");
    std::io::stdout().flush()?;
    std::io::stdin().lock().read_line(&mut String::new())?;

    revocation.submit().await?;
    println!("Signer {signer} revoked.");
    Ok(())
}
