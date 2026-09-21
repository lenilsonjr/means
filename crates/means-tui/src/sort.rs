//! Shared ordering choices for tabular views; amounts compare as decimals, never strings.
use rust_decimal::Decimal;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Accounts,
    Ledger,
    Journal,
    Review,
    Expenses,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Newest,
    Oldest,
    NameAsc,
    NameDesc,
    AmountDesc,
    AmountAsc,
}
impl Order {
    pub const ALL: [Self; 6] = [Self::Newest, Self::Oldest, Self::NameAsc, Self::NameDesc, Self::AmountDesc, Self::AmountAsc];
    pub fn label(self) -> &'static str {
        match self {
            Self::Newest => "Date: newest first",
            Self::Oldest => "Date: oldest first",
            Self::NameAsc => "Name: A-Z",
            Self::NameDesc => "Name: Z-A",
            Self::AmountDesc => "Amount: largest magnitude",
            Self::AmountAsc => "Amount: smallest magnitude",
        }
    }
    pub fn items(dates: bool) -> Vec<(i64, String, String)> {
        Self::ALL.iter().enumerate().filter(|(i, _)| dates || *i >= 2).map(|(i, o)| (i as i64, o.label().into(), String::new())).collect()
    }
    pub fn apply<T>(self, rows: &mut [T], key: impl Fn(&T) -> (String, String, String, i64)) {
        rows.sort_by_cached_key(|row| {
            let (date, name, amount, id) = key(row);
            let number = amount.parse::<Decimal>().unwrap_or_default().abs();
            match self {
                Self::Newest | Self::Oldest => (date, Decimal::ZERO, id),
                Self::NameAsc | Self::NameDesc => (name.to_lowercase(), Decimal::ZERO, id),
                Self::AmountDesc | Self::AmountAsc => (String::new(), number, id),
            }
        });
        if matches!(self, Self::Newest | Self::NameDesc | Self::AmountDesc) {
            rows.reverse();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn orders_dates_names_and_decimal_magnitudes_without_changing_rows() {
        let original = vec![("2024".into(), "Zoo".into(), "-100".into(), 1), ("2026".into(), "apple".into(), "9".into(), 2), ("2025".into(), "Bear".into(), "20".into(), 3)];
        for (order, ids) in [
            (Order::Newest, vec![2, 3, 1]),
            (Order::Oldest, vec![1, 3, 2]),
            (Order::NameAsc, vec![2, 3, 1]),
            (Order::NameDesc, vec![1, 3, 2]),
            (Order::AmountDesc, vec![1, 3, 2]),
            (Order::AmountAsc, vec![2, 3, 1]),
        ] {
            let mut rows = original.clone();
            order.apply(&mut rows, Clone::clone);
            assert_eq!(rows.iter().map(|r| r.3).collect::<Vec<_>>(), ids);
            for row in rows {
                assert!(original.contains(&row));
            }
        }
    }
}
