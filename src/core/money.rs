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
}
