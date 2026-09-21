//! Exact money arithmetic. Storage is an `i64` count of the commodity's minor unit,
//! domain amounts carry their commodity; Decimal is used at input/display boundaries (D14).

use rust_decimal::prelude::*;
use rust_decimal::{Decimal, RoundingStrategy};

use crate::{Error, Result};

/// The most decimals a commodity may have (D14). Eight is a satoshi.
pub const MAX_PRECISION: u32 = 8;

/// Database boundary conversion only. Arithmetic in domain models uses `Money`,
/// which also carries commodity identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MinorUnits {
    minor: i64,
    precision: u32,
}

impl MinorUnits {
    pub fn from_minor(minor: i64, precision: u32) -> Result<MinorUnits> {
        check_precision(precision)?;
        Ok(MinorUnits { minor, precision })
    }

    /// From a major-unit decimal, banker's rounding to the minor unit.
    pub fn from_major(major: Decimal, precision: u32) -> Result<MinorUnits> {
        check_precision(precision)?;
        let out_of_range = || Error::Invalid(format!("amount out of range: {major}"));
        let minor =
            major.checked_mul(Decimal::from(10i64.pow(precision))).ok_or_else(out_of_range)?.round_dp_with_strategy(0, RoundingStrategy::MidpointNearestEven).to_i64().ok_or_else(out_of_range)?;
        Ok(MinorUnits { minor, precision })
    }

    /// Divide into equal shares without losing minor units. Earlier shares receive
    /// the remainder; negating the total negates every share (including i64::MIN).
    pub fn split(&self, parts: usize) -> Result<Vec<MinorUnits>> {
        if parts == 0 {
            return Err(Error::Invalid("split needs at least one part".into()));
        }
        let mut result = Vec::new();
        result.try_reserve_exact(parts).map_err(|_| Error::Invalid("too many split parts".into()))?;
        let magnitude = self.minor.unsigned_abs() as u128;
        let base = magnitude / parts as u128;
        let remainder = magnitude % parts as u128;
        for i in 0..parts {
            let value = base + u128::from((i as u128) < remainder);
            result.push(self.share(value));
        }
        Ok(result)
    }

    /// Allocate by nonnegative integer ratios using largest remainders, with ties
    /// resolved in input order. Zero-weight shares receive zero. Each share is
    /// within one minor unit of its ideal quota and the total is conserved.
    pub fn allocate(&self, ratios: &[u64]) -> Result<Vec<MinorUnits>> {
        let sum = ratios.iter().try_fold(0u128, |sum, &r| sum.checked_add(r as u128)).filter(|&sum| sum > 0).ok_or_else(|| Error::Invalid("allocation needs a positive total ratio".into()))?;
        let magnitude = self.minor.unsigned_abs() as u128;
        let mut parts = Vec::with_capacity(ratios.len());
        let mut remainders = Vec::with_capacity(ratios.len());
        for (index, &ratio) in ratios.iter().enumerate() {
            let weighted = magnitude * ratio as u128;
            parts.push(weighted / sum);
            remainders.push((weighted % sum, index));
        }
        let remaining = magnitude - parts.iter().sum::<u128>();
        remainders.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        for &(_, index) in remainders.iter().take(remaining as usize) {
            parts[index] += 1;
        }
        Ok(parts.into_iter().map(|value| self.share(value)).collect())
    }

    fn share(&self, magnitude: u128) -> MinorUnits {
        // Every share is bounded by abs(self.minor), so signed conversion is exact.
        let signed = if self.minor < 0 { -(magnitude as i128) } else { magnitude as i128 };
        MinorUnits { minor: signed as i64, precision: self.precision }
    }

    pub fn minor(&self) -> i64 {
        self.minor
    }

    pub fn precision(&self) -> u32 {
        self.precision
    }

    pub fn major(&self) -> Decimal {
        // `precision` is checked at construction, so the scale is inside Decimal's range.
        Decimal::from_i128_with_scale(self.minor as i128, self.precision).normalize()
    }
}

/// An exact amount with its commodity and precision. Arithmetic refuses different
/// commodities even when they happen to use the same number of decimal places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(into = "MoneyWire", try_from = "MoneyWire")]
pub struct Money {
    units: MinorUnits,
    // Commodity codes are validated ASCII of at most 12 bytes; padding is zero.
    commodity: [u8; 12],
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MoneyWire {
    minor: String,
    commodity: String,
    precision: u32,
}

impl From<Money> for MoneyWire {
    fn from(value: Money) -> Self {
        Self { minor: value.minor().to_string(), commodity: value.commodity().to_owned(), precision: value.precision() }
    }
}

impl TryFrom<MoneyWire> for Money {
    type Error = Error;
    fn try_from(value: MoneyWire) -> Result<Self> {
        let minor = value.minor.parse().map_err(|_| Error::Invalid("invalid minor-unit count".into()))?;
        Self::from_minor(minor, &value.commodity, value.precision)
    }
}

/// Decode a database amount only after joining its commodity identity and precision.
pub(crate) fn from_row(row: &rusqlite::Row<'_>, minor: usize, commodity: usize, precision: usize) -> rusqlite::Result<Money> {
    Money::from_minor(row.get(minor)?, &row.get::<_, String>(commodity)?, row.get(precision)?)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(minor, rusqlite::types::Type::Integer, Box::new(e)))
}

impl Money {
    pub fn from_minor(minor: i64, commodity: &str, precision: u32) -> Result<Self> {
        let code = crate::entities::normalize_code(commodity)?;
        let mut commodity = [0; 12];
        commodity[..code.len()].copy_from_slice(code.as_bytes());
        Ok(Self { units: MinorUnits::from_minor(minor, precision)?, commodity })
    }

    pub fn from_major(major: Decimal, commodity: &str, precision: u32) -> Result<Self> {
        Self::from_minor(MinorUnits::from_major(major, precision)?.minor(), commodity, precision)
    }

    pub fn commodity(&self) -> &str {
        let end = self.commodity.iter().position(|&b| b == 0).unwrap_or(self.commodity.len());
        std::str::from_utf8(&self.commodity[..end]).expect("validated ASCII commodity code")
    }
    pub fn precision(&self) -> u32 {
        self.units.precision()
    }
    pub fn minor(&self) -> i64 {
        self.units.minor()
    }
    pub fn major(&self) -> Decimal {
        self.units.major()
    }
    pub fn is_zero(&self) -> bool {
        self.minor() == 0
    }
    pub fn is_negative(&self) -> bool {
        self.minor() < 0
    }

    fn check_unit(&self, other: &Self) -> Result<()> {
        if self.commodity != other.commodity || self.precision() != other.precision() {
            return Err(Error::Invalid(format!("cannot combine {} (precision {}) and {} (precision {})", self.commodity(), self.precision(), other.commodity(), other.precision())));
        }
        Ok(())
    }

    pub fn checked_add(self, other: Self) -> Result<Self> {
        self.check_unit(&other)?;
        let minor = self.minor().checked_add(other.minor()).ok_or_else(|| Error::Invalid("money addition overflow".into()))?;
        Ok(Self { units: MinorUnits { minor, ..self.units }, ..self })
    }

    pub fn checked_sub(self, other: Self) -> Result<Self> {
        self.check_unit(&other)?;
        let minor = self.minor().checked_sub(other.minor()).ok_or_else(|| Error::Invalid("money subtraction overflow".into()))?;
        Ok(Self { units: MinorUnits { minor, ..self.units }, ..self })
    }

    /// Sum in a checked commodity, using a wide accumulator so cancellation does
    /// not overflow merely because an intermediate total exceeds i64.
    pub fn checked_sum(self, values: impl IntoIterator<Item = Self>) -> Result<Self> {
        let mut total = self.minor() as i128;
        for value in values {
            self.check_unit(&value)?;
            total = total.checked_add(value.minor() as i128).ok_or_else(|| Error::Invalid("money sum overflow".into()))?;
        }
        let minor = i64::try_from(total).map_err(|_| Error::Invalid("money sum overflow".into()))?;
        Ok(Self { units: MinorUnits { minor, ..self.units }, ..self })
    }

    pub fn checked_neg(self) -> Result<Self> {
        let minor = self.minor().checked_neg().ok_or_else(|| Error::Invalid("money negation overflow".into()))?;
        Ok(Self { units: MinorUnits { minor, ..self.units }, ..self })
    }

    pub fn split(self, parts: usize) -> Result<Vec<Self>> {
        Ok(self.units.split(parts)?.into_iter().map(|units| Self { units, ..self }).collect())
    }

    pub fn allocate(self, ratios: &[u64]) -> Result<Vec<Self>> {
        Ok(self.units.allocate(ratios)?.into_iter().map(|units| Self { units, ..self }).collect())
    }
}

pub fn check_precision(precision: u32) -> Result<()> {
    if precision > MAX_PRECISION {
        return Err(Error::Invalid(format!("commodity precision must be 0 to {MAX_PRECISION}, got {precision}")));
    }
    Ok(())
}

/// Parse a decimal string from the API ("1234.56", "-0.5", "1e3" not allowed).
pub fn parse(s: &str) -> Result<Decimal> {
    let t = s.trim().replace(['_', ' '], "");
    if t.is_empty() {
        return Err(Error::Invalid("empty amount".into()));
    }
    Decimal::from_str(&t).map_err(|_| Error::Invalid(format!("not a decimal: {s:?}")))
}

/// Parse an optional decimal: "" -> None.
pub fn parse_opt(s: &str) -> Result<Option<Decimal>> {
    if s.trim().is_empty() {
        Ok(None)
    } else {
        parse(s).map(Some)
    }
}

/// Parse a number written with a locale: thousands separators and a decimal separator.
/// `decimal_sep` is '.' or ','. Handles "1.234,56", "1,234.56", "R$ 1.234,56", "(12.30)".
pub fn parse_localized(s: &str, decimal_sep: char) -> Result<Decimal> {
    let mut t: String = s.trim().chars().filter(|c| c.is_ascii_digit() || *c == '-' || *c == '.' || *c == ',' || *c == '(' || *c == ')' || *c == '+').collect();
    let negative_parens = t.starts_with('(') && t.ends_with(')');
    t.retain(|c| c != '(' && c != ')' && c != '+');
    let thousands = if decimal_sep == ',' { '.' } else { ',' };
    let cleaned: String = t.chars().filter(|c| *c != thousands).map(|c| if c == decimal_sep { '.' } else { c }).collect();
    if cleaned.is_empty() || cleaned == "-" {
        return Err(Error::Parse(format!("not a number: {s:?}")));
    }
    let mut d = Decimal::from_str(&cleaned).map_err(|_| Error::Parse(format!("not a number: {s:?}")))?;
    if negative_parens {
        d = -d;
    }
    Ok(d)
}

/// Guess the decimal separator of a number string. Returns '.' when unsure.
pub fn guess_decimal_sep(s: &str) -> char {
    let last_dot = s.rfind('.');
    let last_comma = s.rfind(',');
    match (last_dot, last_comma) {
        (Some(d), Some(c)) => {
            if c > d {
                ','
            } else {
                '.'
            }
        }
        (None, Some(c)) => {
            // "1,234" is ambiguous; treat a comma followed by exactly 3 digits at the end as thousands.
            let after = &s[c + 1..];
            if after.chars().filter(|ch| ch.is_ascii_digit()).count() == 3 && !after.contains(',') && s.len() > 4 {
                '.'
            } else {
                ','
            }
        }
        _ => '.',
    }
}

/// Round to a commodity's precision, banker's rounding.
pub fn round_to(d: Decimal, precision: u32) -> Decimal {
    d.round_dp_with_strategy(precision, RoundingStrategy::MidpointNearestEven)
}

/// Format with exactly `precision` decimals.
pub fn fmt(d: Decimal, precision: u32) -> String {
    let r = round_to(d, precision);
    format!("{:.*}", precision as usize, r)
}

/// Format an optional decimal, "" for None.
pub fn fmt_opt(d: Option<Decimal>, precision: u32) -> String {
    d.map(|v| fmt(v, precision)).unwrap_or_default()
}

/// Display a decimal without trailing zeros beyond its natural scale.
pub fn plain(d: Decimal) -> String {
    d.normalize().to_string()
}

/// Default display precision for a currency code.
pub fn default_precision(code: &str) -> u32 {
    match code {
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "HUF" | "TWD" => 0,
        "BHD" | "KWD" | "OMR" | "JOD" | "TND" => 3,
        "BTC" | "ETH" | "SOL" | "LTC" => 8,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trips_exact_units_and_validates_the_unit() {
        for minor in [i64::MIN, -9007199254740991, 0, i64::MAX] {
            let value = Money::from_minor(minor, "BTC", 8).unwrap();
            let json = serde_json::to_value(value).unwrap();
            assert_eq!(json, serde_json::json!({"minor": minor.to_string(), "commodity": "BTC", "precision": 8}));
            assert_eq!(serde_json::from_value::<Money>(json).unwrap(), value);
        }
        for json in [
            serde_json::json!({"minor": "9223372036854775808", "commodity": "EUR", "precision": 2}),
            serde_json::json!({"minor": "1.1", "commodity": "EUR", "precision": 2}),
            serde_json::json!({"minor": "1", "commodity": "", "precision": 2}),
            serde_json::json!({"minor": "1", "commodity": "EUR", "precision": 9}),
            serde_json::json!({"minor": 1, "commodity": "EUR", "precision": 2}),
        ] {
            assert!(serde_json::from_value::<Money>(json).is_err());
        }
    }

    #[test]
    fn commodity_arithmetic_checks_units_and_range() {
        let eur = Money::from_minor(125, " eur ", 2).unwrap();
        assert_eq!(eur.commodity(), "EUR");
        assert_eq!(eur.major(), Decimal::from_str("1.25").unwrap());
        assert_eq!(eur.checked_add(eur).unwrap().minor(), 250);
        assert_eq!(eur.checked_sub(eur).unwrap().minor(), 0);
        assert_eq!(eur.checked_neg().unwrap().minor(), -125);
        for other in [Money::from_minor(125, "USD", 2).unwrap(), Money::from_minor(125, "EUR", 3).unwrap()] {
            assert!(eur.checked_add(other).is_err());
            assert!(eur.checked_sub(other).is_err());
            assert!(eur.checked_sum([other]).is_err());
        }
        let max = Money::from_minor(i64::MAX, "EUR", 2).unwrap();
        let min = Money::from_minor(i64::MIN, "EUR", 2).unwrap();
        let one = Money::from_minor(1, "EUR", 2).unwrap();
        assert!(max.checked_add(one).is_err());
        assert!(min.checked_sub(one).is_err());
        assert!(min.checked_neg().is_err());
        assert!(max.checked_sum([one]).is_err());
        assert_eq!(max.checked_sum([one, min]).unwrap().minor(), 0);
        for code in ["", "💰", "ABCDEFGHIJKLM", "EUR USD"] {
            assert!(Money::from_minor(1, code, 2).is_err());
        }
        assert!(Money::from_minor(1, "EUR", 9).is_err());
    }

    #[test]
    fn allocations_preserve_commodity_and_exact_total() {
        for code in ["EUR", "BTC", "ABCDEFGHIJKL"] {
            for minor in [i64::MIN, -101, 0, 101, i64::MAX] {
                let total = Money::from_minor(minor, code, 8).unwrap();
                for parts in [total.split(3).unwrap(), total.allocate(&[1, 2, 3]).unwrap()] {
                    assert!(parts.iter().all(|p| p.commodity() == code && p.precision() == 8));
                    assert_eq!(Money::from_minor(0, code, 8).unwrap().checked_sum(parts).unwrap(), total);
                }
            }
        }
    }

    fn minor(s: &str, precision: u32) -> i64 {
        MinorUnits::from_major(Decimal::from_str(s).unwrap(), precision).unwrap().minor()
    }

    #[test]
    fn minor_units_per_precision() {
        assert_eq!(minor("3.20", 2), 320);
        assert_eq!(minor("-41.00", 2), -4100);
        assert_eq!(minor("500", 0), 500);
        assert_eq!(minor("0.01", 8), 1_000_000);
        assert_eq!(minor("1234.56789012", 8), 123_456_789_012);
        for (s, p) in [("3.20", 2), ("500", 0), ("0.01", 8), ("-41", 2)] {
            let m = MinorUnits::from_major(Decimal::from_str(s).unwrap(), p).unwrap();
            assert_eq!(m.major(), Decimal::from_str(s).unwrap());
            assert_eq!(MinorUnits::from_minor(m.minor(), p).unwrap(), m);
            assert_eq!(m.precision(), p);
        }
    }

    #[test]
    fn halves_round_to_even() {
        assert_eq!(minor("0.005", 2), 0);
        assert_eq!(minor("0.015", 2), 2);
        assert_eq!(minor("-0.005", 2), 0);
        assert_eq!(minor("-0.015", 2), -2);
        assert_eq!(minor("0.5", 0), 0);
        assert_eq!(minor("1.5", 0), 2);
        assert_eq!(minor("0.000000005", 8), 0);
        assert_eq!(minor("0.000000015", 8), 2);
    }

    #[test]
    fn refuses_out_of_range_and_bad_precision() {
        let huge = Decimal::from_str("100000000000").unwrap();
        assert!(matches!(MinorUnits::from_major(huge, 8), Err(Error::Invalid(_))));
        assert!(matches!(MinorUnits::from_major(Decimal::ONE, MAX_PRECISION + 1), Err(Error::Invalid(_))));
        assert!(matches!(MinorUnits::from_minor(1, 9), Err(Error::Invalid(_))));
        assert!(MinorUnits::from_minor(i64::MAX, 8).is_ok());
    }

    #[test]
    fn localized() {
        assert_eq!(parse_localized("R$ 1.234,56", ',').unwrap(), Decimal::from_str("1234.56").unwrap());
        assert_eq!(parse_localized("1,234.56", '.').unwrap(), Decimal::from_str("1234.56").unwrap());
        assert_eq!(parse_localized("-34,04", ',').unwrap(), Decimal::from_str("-34.04").unwrap());
        assert_eq!(parse_localized("(12.30)", '.').unwrap(), Decimal::from_str("-12.30").unwrap());
        assert_eq!(guess_decimal_sep("1.234,56"), ',');
        assert_eq!(guess_decimal_sep("1,234.56"), '.');
        assert_eq!(guess_decimal_sep("34,04"), ',');
        assert_eq!(guess_decimal_sep("-3.20"), '.');
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt(Decimal::from_str("3.2").unwrap(), 2), "3.20");
        assert_eq!(fmt(Decimal::from_str("-0.005").unwrap(), 2), "0.00");
        assert_eq!(fmt(Decimal::from_str("0.015").unwrap(), 2), "0.02");
    }
}

#[cfg(test)]
mod allocation_tests {
    use super::*;

    #[test]
    fn splitting_conserves_every_minor_unit_including_signed_limits() {
        for value in [-101, -1, 0, 1, 101, i64::MIN, i64::MAX] {
            for count in 1..=17 {
                let parts = MinorUnits::from_minor(value, 8).unwrap().split(count).unwrap();
                assert_eq!(parts.iter().map(|p| p.minor as i128).sum::<i128>(), value as i128);
                let min = parts.iter().map(|p| p.minor).min().unwrap();
                let max = parts.iter().map(|p| p.minor).max().unwrap();
                assert!(max as i128 - min as i128 <= 1);
                assert!(parts.iter().all(|p| p.precision == 8));
            }
        }
        assert!(MinorUnits::from_minor(1, 2).unwrap().split(0).is_err());
    }

    #[test]
    fn weighted_allocations_conserve_total_and_obey_quotas() {
        for value in [-101, -1, 0, 1, 101, i64::MIN, i64::MAX] {
            for weights in [vec![1, 1, 1], vec![0, 1, 2], vec![u64::MAX, u64::MAX, 1], vec![0, 0, 7]] {
                let parts = MinorUnits::from_minor(value, 2).unwrap().allocate(&weights).unwrap();
                assert_eq!(parts.iter().map(|p| p.minor as i128).sum::<i128>(), value as i128);
                let sum = weights.iter().map(|&w| w as u128).sum::<u128>();
                for (p, &weight) in parts.iter().zip(&weights) {
                    let floor = value.unsigned_abs() as u128 * weight as u128 / sum;
                    let magnitude = p.minor.unsigned_abs() as u128;
                    assert!(magnitude == floor || magnitude == floor + 1);
                    if weight == 0 {
                        assert_eq!(p.minor, 0);
                    }
                }
            }
        }
        let positive = MinorUnits::from_minor(101, 2).unwrap().allocate(&[1, 1, 1]).unwrap();
        assert_eq!(positive.iter().map(|p| p.minor).collect::<Vec<_>>(), [34, 34, 33]);
        let negative = MinorUnits::from_minor(-101, 2).unwrap().allocate(&[1, 1, 1]).unwrap();
        assert!(positive.iter().zip(negative).all(|(p, n)| p.minor == -n.minor));
        for weights in [vec![], vec![0], vec![0, 0]] {
            assert!(MinorUnits::from_minor(0, 2).unwrap().allocate(&weights).is_err());
        }
    }
}
