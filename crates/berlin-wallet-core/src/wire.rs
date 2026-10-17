use std::collections::BTreeMap;

use fedimint_client::module::transaction::FeeQuote;
use fedimint_core::module::{Amounts, AmountUnit};

/// Serde-serializable wire form of [`FeeQuote`], which is `Debug + Clone +
/// PartialEq + Eq` but not `Serialize`/`Deserialize` in 0.12.0.
///
/// `Amounts` is a `BTreeMap<AmountUnit, Amount>` internally and implements
/// `IntoIterator<Item = (AmountUnit, Amount)>`; `AmountUnit: Ord` (so it is a
/// valid `BTreeMap` key) and both `AmountUnit` and `Amount` are serde.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FeeQuoteWire {
    pub input: BTreeMap<AmountUnit, fedimint_core::Amount>,
    pub output: BTreeMap<AmountUnit, fedimint_core::Amount>,
    pub dust: BTreeMap<AmountUnit, fedimint_core::Amount>,
}

impl From<&FeeQuote> for FeeQuoteWire {
    fn from(quote: &FeeQuote) -> Self {
        Self {
            input: quote.input.clone().into_iter().collect(),
            output: quote.output.clone().into_iter().collect(),
            dust: quote.dust.clone().into_iter().collect(),
        }
    }
}

impl From<FeeQuoteWire> for FeeQuote {
    fn from(wire: FeeQuoteWire) -> Self {
        Self {
            input: amounts(wire.input),
            output: amounts(wire.output),
            dust: amounts(wire.dust),
        }
    }
}

fn amounts(map: BTreeMap<AmountUnit, fedimint_core::Amount>) -> Amounts {
    let mut amounts = Amounts::ZERO;
    for (unit, amount) in map {
        amounts = amounts
            .checked_add_unit(amount, unit)
            .expect("a valid quote cannot overflow Amounts");
    }
    amounts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_quote_round_trip() {
        let quote = FeeQuote {
            input: Amounts::new_bitcoin_msats(1_234),
            output: Amounts::new_bitcoin_msats(56),
            dust: Amounts::new_bitcoin_msats(7),
        };
        let wire = FeeQuoteWire::from(&quote);
        let back = FeeQuote::from(wire);
        assert_eq!(back, quote);
    }
}
