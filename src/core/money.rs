use rmcp::schemars::{self, JsonSchema};
use serde::{Deserialize, Serialize};

/// An amount in the currency's minor unit (cents for USD), as in design.md
/// §5.3. Never a float: budgets are compared and reserved exactly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Money {
    /// ISO 4217 code, e.g. "USD".
    pub currency: String,
    pub amount_minor: i64,
}

impl Money {
    pub fn new(currency: impl Into<String>, amount_minor: i64) -> Self {
        Self {
            currency: currency.into(),
            amount_minor,
        }
    }

    /// `None` when the currencies differ: amounts in different currencies are
    /// never silently combined or compared.
    pub fn checked_add(&self, other: &Money) -> Option<Money> {
        (self.currency == other.currency)
            .then(|| self.amount_minor.checked_add(other.amount_minor))
            .flatten()
            .map(|amount_minor| Money::new(self.currency.clone(), amount_minor))
    }

    /// `None` when the currencies differ.
    pub fn le(&self, other: &Money) -> Option<bool> {
        (self.currency == other.currency).then_some(self.amount_minor <= other.amount_minor)
    }
}

/// An exact, non-negative decimal number of minor units, for unit prices
/// that have fractions of a cent (an API price of $1.066 is 106.6 cents).
/// Amounts that are charged, limited or recorded are [`Money`]; a `Rate`
/// only ever feeds a calculation whose result is rounded up to `Money`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    /// The value is `mantissa / 10^scale` minor units. Kept without
    /// trailing zeros, so equal values are equal.
    mantissa: i64,
    scale: u8,
}

impl Rate {
    /// The most decimal places of a minor unit a rate may have.
    pub const MAX_SCALE: u8 = 12;
    pub const ZERO: Rate = Rate {
        mantissa: 0,
        scale: 0,
    };

    /// `None` if negative or finer than [`Rate::MAX_SCALE`].
    pub fn new(mantissa: i64, scale: u8) -> Option<Self> {
        if mantissa < 0 || scale > Self::MAX_SCALE {
            return None;
        }
        let (mut mantissa, mut scale) = (mantissa, scale);
        while scale > 0 && mantissa % 10 == 0 {
            mantissa /= 10;
            scale -= 1;
        }
        Some(Self { mantissa, scale })
    }

    /// A whole number of minor units. Negative amounts become zero.
    pub fn from_minor(minor: i64) -> Self {
        Self {
            mantissa: minor.max(0),
            scale: 0,
        }
    }

    pub fn mantissa(&self) -> i64 {
        self.mantissa
    }

    pub fn scale(&self) -> u8 {
        self.scale
    }

    /// Parse a plain decimal ("106.6", "3", "0.05") whose unit is
    /// `10^unit_digits` minor units: 0 for minor units, 2 for dollars.
    /// `None` if it is not a non-negative decimal or has too many digits.
    fn parse(text: &str, unit_digits: u8) -> Option<Self> {
        let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if whole.is_empty() || !digits(whole) || !digits(fraction) {
            return None;
        }
        // Pad the fraction so the unit shift never needs a negative scale.
        let fraction = format!("{fraction:0<width$}", width = usize::from(unit_digits));
        let scale = u8::try_from(fraction.len())
            .ok()?
            .checked_sub(unit_digits)?;
        let mantissa = format!("{whole}{fraction}").parse::<i64>().ok()?;
        Self::new(mantissa, scale)
    }

    /// From minor units, with any number of decimals: "106.6".
    pub fn parse_minor(text: &str) -> Option<Self> {
        Self::parse(text.trim(), 0)
    }

    /// From major units of a two-decimal currency, with any number of
    /// decimals: "1.066" or "$1.066" is 106.6 cents.
    pub fn parse_major(text: &str) -> Option<Self> {
        let text = text.trim();
        Self::parse(text.strip_prefix('$').unwrap_or(text).trim_start(), 2)
    }

    /// The digits of the value with the decimal point `shift` places
    /// further left, showing at least `min_decimals` decimals.
    fn format(&self, shift: u8, min_decimals: usize) -> String {
        let decimals = usize::from(self.scale) + usize::from(shift);
        let digits = format!("{:0>width$}", self.mantissa, width = decimals + 1);
        let (whole, fraction) = digits.split_at(digits.len() - decimals);
        let fraction = fraction.trim_end_matches('0');
        let fraction = format!("{fraction:0<min_decimals$}");
        if fraction.is_empty() {
            whole.to_owned()
        } else {
            format!("{whole}.{fraction}")
        }
    }

    /// In minor units: "106.6".
    pub fn format_minor(&self) -> String {
        self.format(0, 0)
    }

    /// In major units of a two-decimal currency: "1.066", "3.00".
    pub fn format_major(&self) -> String {
        self.format(2, 2)
    }

    /// Whole minor units, dropping any fraction.
    pub fn floor_minor(&self) -> i64 {
        self.mantissa / 10i64.pow(u32::from(self.scale))
    }

    /// `self + other * count`, rounded up to whole minor units. Rounding up
    /// keeps an estimate on the safe side of a spending limit. `None` on
    /// overflow.
    pub fn plus_times_ceil(&self, other: &Rate, count: u32) -> Option<i64> {
        let scale = self.scale.max(other.scale);
        let at_scale = |rate: &Rate| {
            i128::from(rate.mantissa).checked_mul(10i128.pow(u32::from(scale - rate.scale)))
        };
        let total =
            at_scale(self)?.checked_add(at_scale(other)?.checked_mul(i128::from(count))?)?;
        let unit = 10i128.pow(u32::from(scale));
        let minor = total / unit + i128::from(total % unit != 0);
        i64::try_from(minor).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_currency_adds_and_compares() {
        let a = Money::new("USD", 250);
        let b = Money::new("USD", 50);
        assert_eq!(a.checked_add(&b), Some(Money::new("USD", 300)));
        assert_eq!(a.le(&Money::new("USD", 300)), Some(true));
        assert_eq!(a.le(&b), Some(false));
    }

    #[test]
    fn different_currencies_never_combine() {
        let usd = Money::new("USD", 100);
        let eur = Money::new("EUR", 100);
        assert_eq!(usd.checked_add(&eur), None);
        assert_eq!(usd.le(&eur), None);
    }

    #[test]
    fn overflow_is_none() {
        let max = Money::new("USD", i64::MAX);
        assert_eq!(max.checked_add(&Money::new("USD", 1)), None);
    }

    #[test]
    fn rates_keep_every_digit() {
        let rate = Rate::parse_major("$1.066").unwrap();
        assert_eq!((rate.mantissa(), rate.scale()), (1066, 1));
        assert_eq!(rate.format_major(), "1.066");
        assert_eq!(rate.format_minor(), "106.6");
        assert_eq!(Rate::parse_minor("106.6"), Some(rate));
        assert_eq!(Rate::parse_major("1.0660"), Some(rate));

        for (major, minor, shown) in [
            ("3", "300", "3.00"),
            ("1.2", "120", "1.20"),
            ("0.05", "5", "0.05"),
            ("0", "0", "0.00"),
            ("0.000123456", "0.0123456", "0.000123456"),
        ] {
            let rate = Rate::parse_major(major).unwrap();
            assert_eq!(rate.format_minor(), minor, "{major}");
            assert_eq!(rate.format_major(), shown, "{major}");
            assert_eq!(Rate::parse_minor(minor), Some(rate), "{major}");
        }
        assert_eq!(Rate::from_minor(120), Rate::parse_major("1.20").unwrap());

        for bad in [
            "",
            ".5",
            "-1",
            "1,20",
            "abc",
            "1.2.3",
            "1e3",
            "0.000000000000001",
        ] {
            assert_eq!(Rate::parse_major(bad), None, "{bad}");
        }
        assert_eq!(Rate::new(-1, 0), None);
        assert_eq!(Rate::new(1, Rate::MAX_SCALE + 1), None);
    }

    #[test]
    fn rate_totals_round_up_once() {
        let base = Rate::parse_major("1.066").unwrap();
        let extra = Rate::parse_major("0.123").unwrap();
        // 106.6 cents alone rounds up to 107.
        assert_eq!(base.plus_times_ceil(&extra, 0), Some(107));
        // 106.6 + 3 x 12.3 = 143.5, not 107 + 3 x 13 = 146.
        assert_eq!(base.plus_times_ceil(&extra, 3), Some(144));
        // Whole amounts are not rounded.
        let whole = Rate::from_minor(120);
        assert_eq!(whole.plus_times_ceil(&Rate::from_minor(20), 2), Some(160));
        assert_eq!(
            Rate::from_minor(i64::MAX).plus_times_ceil(&Rate::from_minor(1), 1),
            None
        );
    }
}
