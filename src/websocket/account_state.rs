//! `account` and `positions` channel rows to Nautilus balances and position reports.

use nautilus_core::UnixNanos;
use nautilus_model::{
    enums::PositionSide,
    identifiers::AccountId,
    instruments::{Instrument, InstrumentAny},
    reports::PositionStatusReport,
    types::{AccountBalance, Currency},
};
use rust_decimal::Decimal;

use super::messages::{RisexWsAccount, RisexWsPosition};
use crate::common::{
    enums::RisexOrderSide,
    parse::{parse_decimal, quantity_from_decimal},
};

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

/// The position report of a `positions` channel row.
///
/// # Errors
///
/// Returns an error if a number does not parse or the size does not fit the instrument.
pub fn parse_position_status_report(
    position: &RisexWsPosition,
    account_id: AccountId,
    instrument: &InstrumentAny,
    ts_init: UnixNanos,
) -> anyhow::Result<PositionStatusReport> {
    let size = parse_decimal(&position.size)?;
    let quantity = quantity_from_decimal(size.abs(), instrument.size_precision())?;
    // unverified: short and closed rows follow the docs (a negative size, and "0" with the
    // last side kept); every recorded row is long. A short is read from either sign or side.
    let position_side = if quantity.is_zero() {
        PositionSide::Flat
    } else if size.is_sign_negative() || position.side == Some(RisexOrderSide::Sell) {
        PositionSide::Short
    } else {
        PositionSide::Long
    };
    let avg_px_open = parse_decimal(&position.avg_entry_price)?;

    Ok(PositionStatusReport::new(
        account_id,
        instrument.id(),
        position_side,
        quantity,
        ts_init,
        ts_init,
        None,
        None,
        (avg_px_open > Decimal::ZERO).then_some(avg_px_open),
    ))
}

#[cfg(test)]
mod tests {
    use nautilus_model::types::Quantity;
    use rstest::rstest;

    use super::*;
    use crate::http::parse::tests::instrument;

    /// The first recorded `positions` update row (BTC, long) with `size` and `side` replaced.
    fn position(size: &str, side: &str) -> RisexWsPosition {
        let frames = include_str!("../../tests/fixtures/ws/account_channels_unauth.jsonl");
        let frame = frames
            .lines()
            .find(|line| line.starts_with(r#"{"channel":"positions","type":"update""#))
            .unwrap();
        let mut value: serde_json::Value = serde_json::from_str(frame).unwrap();
        let row = &mut value["data"][0];
        if !size.is_empty() {
            row["size"] = size.into();
            row["side"] = side.into();
        }
        serde_json::from_value(row.clone()).unwrap()
    }

    #[rstest]
    // Recorded.
    #[case("", "", PositionSide::Long, "4.207594")]
    // Hand-made from the docs: no short or closed row was recorded.
    #[case("-1.5", "SELL", PositionSide::Short, "1.500000")]
    #[case("1.5", "SELL", PositionSide::Short, "1.500000")]
    #[case("0", "SELL", PositionSide::Flat, "0.000000")]
    fn position_report_from_a_channel_row(
        #[case] size: &str,
        #[case] side: &str,
        #[case] position_side: PositionSide,
        #[case] quantity: &str,
    ) {
        let report = parse_position_status_report(
            &position(size, side),
            AccountId::new("RISEX-001"),
            &instrument(1),
            UnixNanos::default(),
        )
        .unwrap();

        assert_eq!(report.instrument_id.to_string(), "BTC-USDC-PERP.RISEX");
        assert_eq!(report.position_side, position_side);
        assert_eq!(report.quantity, Quantity::from(quantity));
        assert!(report.avg_px_open.is_some());
    }

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
