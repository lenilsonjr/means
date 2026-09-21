//! Formatting helpers: decimal strings from the wire, column widths, truncation.

/// Format a decimal string ("-1234.5") with thousands separators and a fixed number of decimals.
/// Never goes through floats. Empty input stays empty.
pub fn money(s: &str, precision: usize) -> String {
    let t = s.trim();
    if t.is_empty() {
        return String::new();
    }
    let (neg, body) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (int_part, frac_part) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    let int_part = if int_part.is_empty() { "0" } else { int_part };
    let mut grouped = String::new();
    let digits: Vec<char> = int_part.chars().collect();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(*c);
    }
    let mut frac: String = frac_part.chars().take(precision).collect();
    while frac.len() < precision {
        frac.push('0');
    }
    let mut out = String::new();
    let is_zero = digits.iter().all(|c| *c == '0') && frac.chars().all(|c| c == '0');
    if neg && !is_zero {
        out.push('-');
    }
    out.push_str(&grouped);
    if precision > 0 {
        out.push('.');
        out.push_str(&frac);
    }
    out
}

/// Sign of a decimal string: -1, 0, 1.
pub fn sign(s: &str) -> i8 {
    let t = s.trim();
    if t.is_empty() {
        return 0;
    }
    let neg = t.starts_with('-');
    let zero = t.chars().all(|c| c == '0' || c == '.' || c == '-' || c == '+');
    if zero {
        0
    } else if neg {
        -1
    } else {
        1
    }
}

/// Truncate to `width` characters, ending with an ellipsis when cut.
pub fn truncate(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Pad on the right to `width` characters (after truncating).
pub fn pad_right(s: &str, width: usize) -> String {
    let t = truncate(s, width);
    let n = t.chars().count();
    let mut out = t;
    for _ in n..width {
        out.push(' ');
    }
    out
}

/// Pad on the left to `width` characters (after truncating).
pub fn pad_left(s: &str, width: usize) -> String {
    let t = truncate(s, width);
    let n = t.chars().count();
    let mut out = String::new();
    for _ in n..width {
        out.push(' ');
    }
    out.push_str(&t);
    out
}

/// Indent a name by its depth in the account tree.
pub fn indent(depth: i32, name: &str) -> String {
    let mut s = String::new();
    for _ in 0..depth.max(0) {
        s.push_str("  ");
    }
    s.push_str(name);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_money_without_floats() {
        assert_eq!(money("-1234.5", 2), "-1,234.50");
        assert_eq!(money("7950", 2), "7,950.00");
        assert_eq!(money("0.01234567", 8), "0.01234567");
        assert_eq!(money("1000000", 0), "1,000,000");
        assert_eq!(money("-0", 2), "0.00");
        assert_eq!(money("", 2), "");
        assert_eq!(money("3.2", 2), "3.20");
        assert_eq!(money("12345678.999", 2), "12,345,678.99");
    }

    #[test]
    fn signs_and_padding() {
        assert_eq!(sign("-3.2"), -1);
        assert_eq!(sign("0.00"), 0);
        assert_eq!(sign("12"), 1);
        assert_eq!(truncate("Assets:Bank:N26", 8), "Assets:…");
        assert_eq!(pad_left("1.00", 8), "    1.00");
        assert_eq!(pad_right("ab", 4), "ab  ");
        assert_eq!(indent(2, "N26"), "    N26");
    }
}
