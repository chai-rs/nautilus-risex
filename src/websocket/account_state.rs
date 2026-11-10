//! `account` channel rows to Nautilus account balances.

use nautilus_model::types::{AccountBalance, Currency};

use super::messages::RisexWsAccount;
use crate::common::parse::parse_decimal;

/// The USDC balance of an `account` channel row: total is the account equity,
/// free is the free cross margin, the rest is locked.
///
/// # Errors
///
/// Returns an error if a number does not parse or does not fit a `Money`.
pub fn parse_account_balance(account: &RisexWsAccount) -> anyhow::Result<AccountBalance> {
    let total = parse_decimal(&account.account_equity)?;
    let free = parse_decimal(&account.free_cross_margin_balance)?;
    AccountBalance::from_total_and_locked(total, total - free, Currency::USDC())
        .map_err(|e| anyhow::anyhow!("invalid account balance: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_from_the_fixture_snapshot() {
        let frames = include_str!("../../tests/fixtures/ws/account_channels_unauth.jsonl");
        let frame = frames
            .lines()
            .find(|line| line.starts_with(r#"{"channel":"account","type":"snapshot""#))
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(frame).unwrap();
        let account: RisexWsAccount = serde_json::from_value(value["data"].clone()).unwrap();

        let balance = parse_account_balance(&account).unwrap();

        assert_eq!(balance.currency, Currency::USDC());
        assert_eq!(
            balance.total.as_decimal(),
            balance.locked.as_decimal() + balance.free.as_decimal()
        );
        assert!(balance.total.as_decimal() > rust_decimal::Decimal::from(1_178_058));
        assert!(balance.free.as_decimal() <= balance.total.as_decimal());
    }
}
