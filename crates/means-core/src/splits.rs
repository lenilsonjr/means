//! Amount validation shared by capture and draft categorization.
use crate::{Error, Result};
use rust_decimal::Decimal;

#[derive(Debug, Clone)]
pub struct Leg {
    pub account_id: i64,
    /// Amount in the bank account's currency (negative for discounts); only the final leg may omit it.
    pub quantity: Option<Decimal>,
    pub memo: String,
}

pub fn resolve(total: Decimal, legs: &[Leg]) -> Result<Vec<(i64, Decimal, String)>> {
    if total <= Decimal::ZERO || legs.is_empty() {
        return Err(Error::Invalid("a positive total and at least one split are required".into()));
    }
    let mut sum = Decimal::ZERO;
    let mut out = Vec::new();
    for (i, leg) in legs.iter().enumerate() {
        let q = match leg.quantity {
            Some(q) => q,
            None if i + 1 == legs.len() => total.checked_sub(sum).ok_or_else(|| Error::Invalid("split amount overflow".into()))?,
            None => return Err(Error::Invalid("only the last split can take the remainder".into())),
        };
        if q.is_zero() || (leg.quantity.is_none() && q.is_sign_negative()) {
            return Err(Error::Invalid("split amounts must be nonzero and an omitted remainder must be positive".into()));
        }
        sum = sum.checked_add(q).ok_or_else(|| Error::Invalid("split total overflow".into()))?;
        out.push((leg.account_id, q, leg.memo.clone()));
    }
    if sum != total {
        return Err(Error::Invalid(format!("splits sum to {sum} but the amount is {total}")));
    }
    Ok(out)
}
