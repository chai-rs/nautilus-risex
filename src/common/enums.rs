//! RISEx venue enums mirrored from REST and WebSocket payloads.

use std::{fmt::Display, str::FromStr};

use nautilus_model::{
    data::BarType,
    enums::{
        AggregationSource, AggressorSide, BarAggregation, LiquiditySide, OrderSide, OrderType,
        PositionSide, PriceType, TimeInForce, TriggerType,
    },
};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use strum::{AsRefStr, Display, EnumIter, EnumString};

/// RISEx API environment.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Display,
    PartialEq,
    Eq,
    Hash,
    AsRefStr,
    EnumIter,
    EnumString,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "lowercase")]
#[strum(ascii_case_insensitive, serialize_all = "lowercase")]
#[cfg_attr(
    feature = "python",
    pyo3::pyclass(
        eq,
        eq_int,
        module = "nautilus_risex",
        from_py_object,
        rename_all = "SCREAMING_SNAKE_CASE"
    )
)]
pub enum RisexEnvironment {
    /// Mainnet trading environment.
    #[default]
    Mainnet,
    /// Testnet environment.
    Testnet,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum NameOrCode {
    Code(u8),
    Name(String),
}

/// Declares a venue enum with its wire names.
///
/// The `Variant = code => "NAME"` form is for enums the venue sends as a name on some
/// endpoints and as a numeric code on others (`/v1/orders` vs `/v1/orders/open`); it accepts
/// both when deserializing. Serialization always writes the name.
macro_rules! wire_enum {
    (@common $(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident $(= $code:literal)? => $wire:literal,)+ }) => {
        $(#[$meta])*
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant $(= $code)?,)+
        }

        impl $name {
            /// Returns the name the venue uses on the wire.
            #[must_use]
            pub const fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }
        }

        impl FromStr for $name {
            type Err = String;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($wire => Ok(Self::$variant),)+
                    _ => Err(format!("unknown {}: {s:?}", stringify!($name))),
                }
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }
    };
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $wire:literal,)+ }) => {
        wire_enum!(@common $(#[$meta])* $name { $($(#[$vmeta])* $variant => $wire,)+ });

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                String::deserialize(deserializer)?
                    .parse()
                    .map_err(de::Error::custom)
            }
        }
    };
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident = $code:literal => $wire:literal,)+ }) => {
        wire_enum!(@common $(#[$meta])* #[repr(u8)] $name { $($(#[$vmeta])* $variant = $code => $wire,)+ });

        impl $name {
            /// Returns the numeric code the venue uses on the wire.
            #[must_use]
            pub const fn code(&self) -> u8 {
                *self as u8
            }

            /// Returns the variant for a numeric wire code.
            #[must_use]
            pub const fn from_code(code: u8) -> Option<Self> {
                match code {
                    $($code => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                match NameOrCode::deserialize(deserializer)? {
                    NameOrCode::Name(name) => name.parse().map_err(de::Error::custom),
                    NameOrCode::Code(code) => Self::from_code(code).ok_or_else(|| {
                        de::Error::custom(format!("unknown {} code: {code}", stringify!($name)))
                    }),
                }
            }
        }
    };
}

wire_enum! {
    /// Order side. Also used for position sides (`BUY` = long) and the maker side of a trade.
    RisexOrderSide {
        Buy = 0 => "BUY",
        Sell = 1 => "SELL",
    }
}

impl RisexOrderSide {
    /// Returns the position side when this value describes a position (`BUY` = long).
    #[must_use]
    pub const fn as_position_side(&self) -> PositionSide {
        match self {
            Self::Buy => PositionSide::Long,
            Self::Sell => PositionSide::Short,
        }
    }

    /// Returns the aggressor side when this value is the maker side of a trade.
    #[must_use]
    pub const fn as_aggressor_of_maker(&self) -> AggressorSide {
        match self {
            Self::Buy => AggressorSide::Sell,
            Self::Sell => AggressorSide::Buy,
        }
    }
}

impl From<RisexOrderSide> for OrderSide {
    fn from(value: RisexOrderSide) -> Self {
        match value {
            RisexOrderSide::Buy => Self::Buy,
            RisexOrderSide::Sell => Self::Sell,
        }
    }
}

impl From<OrderSide> for RisexOrderSide {
    fn from(value: OrderSide) -> Self {
        match value {
            OrderSide::Buy => Self::Buy,
            OrderSide::Sell => Self::Sell,
        }
    }
}

wire_enum! {
    /// Order type.
    RisexOrderType {
        Market = 0 => "MARKET",
        Limit = 1 => "LIMIT",
    }
}

impl From<RisexOrderType> for OrderType {
    fn from(value: RisexOrderType) -> Self {
        match value {
            RisexOrderType::Market => Self::Market,
            RisexOrderType::Limit => Self::Limit,
        }
    }
}

impl TryFrom<OrderType> for RisexOrderType {
    type Error = anyhow::Error;

    fn try_from(value: OrderType) -> Result<Self, Self::Error> {
        match value {
            OrderType::Market => Ok(Self::Market),
            OrderType::Limit => Ok(Self::Limit),
            _ => anyhow::bail!("unsupported order type: {value}"),
        }
    }
}

wire_enum! {
    /// Time in force.
    RisexTimeInForce {
        /// Good till cancelled.
        Gtc = 0 => "GTC",
        /// Good till time (the order carries an expiry).
        Gtt = 1 => "GTT",
        /// Fill or kill.
        Fok = 2 => "FOK",
        /// Immediate or cancel.
        Ioc = 3 => "IOC",
    }
}

impl From<RisexTimeInForce> for TimeInForce {
    fn from(value: RisexTimeInForce) -> Self {
        match value {
            RisexTimeInForce::Gtc => Self::Gtc,
            RisexTimeInForce::Gtt => Self::Gtd,
            RisexTimeInForce::Fok => Self::Fok,
            RisexTimeInForce::Ioc => Self::Ioc,
        }
    }
}

impl TryFrom<TimeInForce> for RisexTimeInForce {
    type Error = anyhow::Error;

    fn try_from(value: TimeInForce) -> Result<Self, Self::Error> {
        match value {
            TimeInForce::Gtc => Ok(Self::Gtc),
            TimeInForce::Gtd => Ok(Self::Gtt),
            TimeInForce::Fok => Ok(Self::Fok),
            TimeInForce::Ioc => Ok(Self::Ioc),
            _ => anyhow::bail!("unsupported time in force: {value}"),
        }
    }
}

wire_enum! {
    /// Self-trade prevention mode.
    RisexStpMode {
        ExpireMaker = 0 => "EXPIRE_MAKER",
        ExpireTaker = 1 => "EXPIRE_TAKER",
        ExpireBoth = 2 => "EXPIRE_BOTH",
    }
}

wire_enum! {
    /// Margin mode. Only the numeric form was seen on the wire; the names are assumed.
    RisexMarginMode {
        Cross = 0 => "CROSS",
        Isolated = 1 => "ISOLATED",
    }
}

wire_enum! {
    /// Session key (signer) status from `/v1/auth/session-key-status`.
    RisexSessionKeyStatus {
        NotExist = 0 => "NotExist",
        Active = 1 => "Active",
        Revoked = 2 => "Revoked",
    }
}

wire_enum! {
    /// Order status. A partial fill stays `ORDER_STATUS_OPEN` with a non-zero filled size.
    RisexOrderStatus {
        /// The order does not exist or is invalid.
        None => "ORDER_STATUS_NONE",
        Open => "ORDER_STATUS_OPEN",
        Filled => "ORDER_STATUS_FILLED",
        Cancelled => "ORDER_STATUS_CANCELLED",
    }
}

wire_enum! {
    /// Status of a take-profit / stop-loss order.
    RisexTpslOrderStatus {
        /// Accepted, waiting for the trigger.
        Accepted => "TPSL_ORDER_STATUS_ACCEPTED",
        /// Trigger condition met, executing.
        Triggered => "TPSL_ORDER_STATUS_TRIGGERED",
        Success => "TPSL_ORDER_STATUS_SUCCESS",
        Cancelled => "TPSL_ORDER_STATUS_CANCELLED",
    }
}

wire_enum! {
    /// Stop type. Plain orders report `TAKE_PROFIT` (the schema default) with an empty stop
    /// price, so only trust this field when a stop price is present.
    RisexStopType {
        TakeProfit => "TAKE_PROFIT",
        StopLoss => "STOP_LOSS",
        None => "STOP_TYPE_NONE",
    }
}

wire_enum! {
    /// Price a stop order triggers on.
    RisexStopPriceOption {
        LastTradedPrice => "LAST_TRADED_PRICE",
        MarkPrice => "MARK_PRICE",
        None => "PRICE_OPTION_NONE",
    }
}

impl RisexStopPriceOption {
    /// Returns the Nautilus trigger type, or `None` when the order has no trigger.
    #[must_use]
    pub const fn as_trigger_type(&self) -> Option<TriggerType> {
        match self {
            Self::LastTradedPrice => Some(TriggerType::LastPrice),
            Self::MarkPrice => Some(TriggerType::MarkPrice),
            Self::None => None,
        }
    }
}

wire_enum! {
    /// Whether a fill added or removed liquidity.
    RisexLiquidityIndicator {
        Taker => "TAKER",
        Maker => "MAKER",
    }
}

impl From<RisexLiquidityIndicator> for LiquiditySide {
    fn from(value: RisexLiquidityIndicator) -> Self {
        match value {
            RisexLiquidityIndicator::Taker => Self::Taker,
            RisexLiquidityIndicator::Maker => Self::Maker,
        }
    }
}

wire_enum! {
    /// Bar intervals the venue serves. Any other interval silently returns 1-minute bars
    /// (or HTTP 400), so requests must be validated against this list first.
    RisexBarInterval {
        Minute1 => "1m",
        Minute5 => "5m",
        Minute15 => "15m",
        Hour1 => "1h",
        Hour4 => "4h",
        Day1 => "1d",
        Week1 => "1w",
    }
}

impl RisexBarInterval {
    /// Returns the interval length in nanoseconds, as the `interval` query parameter expects.
    #[must_use]
    pub const fn as_nanos(&self) -> u64 {
        const MINUTE: u64 = 60_000_000_000;
        match self {
            Self::Minute1 => MINUTE,
            Self::Minute5 => 5 * MINUTE,
            Self::Minute15 => 15 * MINUTE,
            Self::Hour1 => 60 * MINUTE,
            Self::Hour4 => 240 * MINUTE,
            Self::Day1 => 1440 * MINUTE,
            Self::Week1 => 10_080 * MINUTE,
        }
    }
}

impl TryFrom<&BarType> for RisexBarInterval {
    type Error = anyhow::Error;

    fn try_from(bar_type: &BarType) -> Result<Self, Self::Error> {
        let spec = bar_type.spec();
        anyhow::ensure!(
            spec.price_type == PriceType::Last
                && bar_type.aggregation_source() == AggregationSource::External,
            "RISEx bars are LAST-EXTERNAL only, was {bar_type}"
        );

        match (spec.step.get(), spec.aggregation) {
            (1, BarAggregation::Minute) => Ok(Self::Minute1),
            (5, BarAggregation::Minute) => Ok(Self::Minute5),
            (15, BarAggregation::Minute) => Ok(Self::Minute15),
            (1, BarAggregation::Hour) => Ok(Self::Hour1),
            (4, BarAggregation::Hour) => Ok(Self::Hour4),
            (1, BarAggregation::Day) => Ok(Self::Day1),
            (1, BarAggregation::Week) => Ok(Self::Week1),
            _ => anyhow::bail!(
                "RISEx serves only 1m, 5m, 15m, 1h, 4h, 1d and 1w bars, was {bar_type}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("\"BUY\"", RisexOrderSide::Buy)]
    #[case("\"SELL\"", RisexOrderSide::Sell)]
    #[case("0", RisexOrderSide::Buy)]
    #[case("1", RisexOrderSide::Sell)]
    fn test_order_side_accepts_name_and_code(#[case] json: &str, #[case] expected: RisexOrderSide) {
        assert_eq!(
            serde_json::from_str::<RisexOrderSide>(json).unwrap(),
            expected
        );
    }

    #[rstest]
    fn test_coded_enum_round_trip() {
        assert_eq!(RisexTimeInForce::from_code(3), Some(RisexTimeInForce::Ioc));
        assert_eq!(RisexTimeInForce::Gtt.code(), 1);
        assert_eq!(RisexStpMode::ExpireBoth.code(), 2);
        assert_eq!(RisexOrderType::from_code(1), Some(RisexOrderType::Limit));
        assert_eq!(
            serde_json::to_string(&RisexTimeInForce::Fok).unwrap(),
            "\"FOK\""
        );
        assert!(serde_json::from_str::<RisexOrderType>("2").is_err());
        assert!(serde_json::from_str::<RisexOrderType>("\"STOP\"").is_err());
    }

    #[rstest]
    fn test_name_only_enum_rejects_codes() {
        assert_eq!(
            serde_json::from_str::<RisexOrderStatus>("\"ORDER_STATUS_OPEN\"").unwrap(),
            RisexOrderStatus::Open
        );
        assert!(serde_json::from_str::<RisexOrderStatus>("1").is_err());
        assert_eq!(
            RisexTpslOrderStatus::Cancelled.to_string(),
            "TPSL_ORDER_STATUS_CANCELLED"
        );
    }

    #[rstest]
    fn test_nautilus_conversions() {
        assert_eq!(OrderSide::from(RisexOrderSide::Sell), OrderSide::Sell);
        assert_eq!(
            RisexOrderSide::Buy.as_aggressor_of_maker(),
            AggressorSide::Sell
        );
        assert_eq!(RisexOrderSide::Sell.as_position_side(), PositionSide::Short);
        assert_eq!(TimeInForce::from(RisexTimeInForce::Gtt), TimeInForce::Gtd);
        assert_eq!(
            RisexTimeInForce::try_from(TimeInForce::Gtd).unwrap(),
            RisexTimeInForce::Gtt
        );
        assert!(RisexTimeInForce::try_from(TimeInForce::Day).is_err());
        assert!(RisexOrderType::try_from(OrderType::StopMarket).is_err());
        assert_eq!(RisexOrderSide::from(OrderSide::Buy), RisexOrderSide::Buy);
        assert_eq!(
            RisexStopPriceOption::MarkPrice.as_trigger_type(),
            Some(TriggerType::MarkPrice)
        );
        assert_eq!(RisexStopPriceOption::None.as_trigger_type(), None);
        assert_eq!(
            LiquiditySide::from(RisexLiquidityIndicator::Maker),
            LiquiditySide::Maker
        );
    }

    #[rstest]
    #[case(
        "AERO-USDC-PERP.RISEX-1-MINUTE-LAST-EXTERNAL",
        Some(RisexBarInterval::Minute1)
    )]
    #[case(
        "AERO-USDC-PERP.RISEX-15-MINUTE-LAST-EXTERNAL",
        Some(RisexBarInterval::Minute15)
    )]
    #[case(
        "AERO-USDC-PERP.RISEX-4-HOUR-LAST-EXTERNAL",
        Some(RisexBarInterval::Hour4)
    )]
    #[case(
        "AERO-USDC-PERP.RISEX-1-WEEK-LAST-EXTERNAL",
        Some(RisexBarInterval::Week1)
    )]
    #[case("AERO-USDC-PERP.RISEX-1-SECOND-LAST-EXTERNAL", None)]
    #[case("AERO-USDC-PERP.RISEX-3-MINUTE-LAST-EXTERNAL", None)]
    #[case("AERO-USDC-PERP.RISEX-1-MONTH-LAST-EXTERNAL", None)]
    #[case("AERO-USDC-PERP.RISEX-1-MINUTE-BID-EXTERNAL", None)]
    #[case("AERO-USDC-PERP.RISEX-1-MINUTE-LAST-INTERNAL", None)]
    fn test_bar_interval_validation(
        #[case] bar_type: &str,
        #[case] expected: Option<RisexBarInterval>,
    ) {
        let bar_type = BarType::from(bar_type);
        assert_eq!(RisexBarInterval::try_from(&bar_type).ok(), expected);
    }

    #[rstest]
    fn test_bar_interval_nanos() {
        assert_eq!(RisexBarInterval::Minute1.as_nanos(), 60_000_000_000);
        assert_eq!(RisexBarInterval::Week1.as_nanos(), 604_800_000_000_000);
        assert_eq!(
            "4h".parse::<RisexBarInterval>(),
            Ok(RisexBarInterval::Hour4)
        );
    }
}
