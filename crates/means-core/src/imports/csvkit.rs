//! CSV helpers: delimiter and header detection, tolerant date and number parsing.

use chrono::NaiveDate;
use rust_decimal::Decimal;

use crate::money;
use crate::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub delimiter: char,
    pub header_row: usize,
    /// Lines before the header (bank preambles), kept for balance hints.
    pub preamble: Vec<String>,
}

pub fn decode(content: &[u8]) -> String {
    let text = match std::str::from_utf8(content) {
        Ok(s) => s.to_string(),
        Err(_) => content.iter().map(|&b| b as char).collect(), // Latin-1 fallback
    };
    text.trim_start_matches('\u{feff}').to_string()
}

pub fn detect_delimiter(sample: &str) -> char {
    let mut best = (',', 0usize);
    for d in [',', ';', '\t', '|'] {
        let count = sample.lines().take(5).map(|l| l.matches(d).count()).sum::<usize>();
        if count > best.1 {
            best = (d, count);
        }
    }
    best.0
}

/// Parse a CSV text into a table. `header_row` is 1-based; None means auto-detect.
pub fn parse_table(text: &str, delimiter: Option<char>, header_row: Option<usize>) -> Result<Table> {
    let delimiter = delimiter.unwrap_or_else(|| detect_delimiter(text));
    let mut reader = csv::ReaderBuilder::new().delimiter(delimiter as u8).has_headers(false).flexible(true).trim(csv::Trim::All).from_reader(text.as_bytes());
    let mut records: Vec<Vec<String>> = Vec::new();
    for rec in reader.records() {
        let rec = rec.map_err(|e| Error::Parse(format!("csv: {e}")))?;
        records.push(rec.iter().map(|s| s.to_string()).collect());
    }
    if records.is_empty() {
        return Err(Error::Parse("the file has no rows".into()));
    }
    let header_idx = match header_row {
        Some(h) if h >= 1 => h - 1,
        _ => auto_header(&records),
    };
    if header_idx >= records.len() {
        return Err(Error::Parse("header row is beyond the end of the file".into()));
    }
    let preamble: Vec<String> = records[..header_idx].iter().map(|r| r.join(" ")).collect();
    let headers: Vec<String> = records[header_idx].iter().map(|h| h.trim().to_string()).collect();
    let rows: Vec<Vec<String>> = records[header_idx + 1..]
        .iter()
        .filter(|r| r.iter().any(|c| !c.trim().is_empty()))
        .map(|r| {
            let mut r = r.clone();
            r.resize(headers.len().max(r.len()), String::new());
            r
        })
        .collect();
    Ok(Table { headers, rows, delimiter, header_row: header_idx + 1, preamble })
}

/// The header is the first row with at least three non-empty cells, at least one letter,
/// whose width matches most of the following rows.
fn auto_header(records: &[Vec<String>]) -> usize {
    let widths: Vec<usize> = records.iter().map(|r| r.iter().filter(|c| !c.trim().is_empty()).count()).collect();
    // The modal width of non-empty rows is the shape of the data.
    let mut counts: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for w in widths.iter().filter(|w| **w >= 2) {
        *counts.entry(*w).or_insert(0) += 1;
    }
    let modal = counts.iter().max_by_key(|(w, n)| (**n, **w)).map(|(w, _)| *w).unwrap_or(2);
    for (i, rec) in records.iter().enumerate().take(50) {
        let non_empty = widths[i];
        if non_empty + 1 < modal || non_empty < 2 {
            continue;
        }
        let has_alpha = rec.iter().any(|c| c.chars().any(|ch| ch.is_alphabetic()));
        let numeric_cells = rec.iter().filter(|c| !c.trim().is_empty() && c.trim().chars().all(|ch| ch.is_ascii_digit() || ch == '.' || ch == ',' || ch == '-' || ch == '/')).count();
        if !has_alpha || numeric_cells * 2 > non_empty {
            continue;
        }
        return i;
    }
    0
}

/// Find a column by candidate names: exact (case-insensitive) first, then contains.
pub fn find_col(headers: &[String], candidates: &[&str]) -> Option<usize> {
    let norm = |s: &str| s.trim().to_lowercase();
    for c in candidates {
        let c = norm(c);
        if let Some(i) = headers.iter().position(|h| norm(h) == c) {
            return Some(i);
        }
    }
    for c in candidates {
        let c = norm(c);
        if let Some(i) = headers.iter().position(|h| norm(h).contains(&c)) {
            return Some(i);
        }
    }
    None
}

/// Find a column by its exact name (case-insensitive, trimmed).
pub fn exact_col(headers: &[String], name: &str) -> Option<usize> {
    let n = name.trim().to_lowercase();
    headers.iter().position(|h| h.trim().to_lowercase() == n)
}

pub fn col_by_name(headers: &[String], name: &str) -> Option<usize> {
    if name.trim().is_empty() {
        return None;
    }
    if let Ok(n) = name.trim().parse::<usize>() {
        if n >= 1 && n <= headers.len() {
            return Some(n - 1);
        }
    }
    find_col(headers, &[name])
}

/// Detect a date format from sample values. Returns a strftime pattern.
pub fn detect_date_format(values: &[&str]) -> String {
    let mut first_gt_12 = false;
    let mut second_gt_12 = false;
    let mut sep = '/';
    let mut year_first = false;
    let mut has_time = false;
    let mut two_digit_year = false;
    for v in values.iter().filter(|v| !v.trim().is_empty()).take(200) {
        let v = v.trim();
        let core = v.split([' ', 'T']).next().unwrap_or(v);
        if v.len() > core.len() {
            has_time = true;
        }
        let parts: Vec<&str> = core.split(['/', '-', '.']).collect();
        if parts.len() != 3 {
            continue;
        }
        if core.contains('-') {
            sep = '-';
        } else if core.contains('.') {
            sep = '.';
        }
        if parts[0].len() == 4 {
            year_first = true;
            continue;
        }
        if parts[2].len() == 2 {
            two_digit_year = true;
        }
        if parts[0].parse::<u32>().map(|n| n > 12).unwrap_or(false) {
            first_gt_12 = true;
        }
        if parts[1].parse::<u32>().map(|n| n > 12).unwrap_or(false) {
            second_gt_12 = true;
        }
    }
    let year = if two_digit_year { "%y" } else { "%Y" };
    let base = if year_first {
        format!("%Y{sep}%m{sep}%d")
    } else if second_gt_12 && !first_gt_12 {
        format!("%m{sep}%d{sep}{year}")
    } else {
        format!("%d{sep}%m{sep}{year}")
    };
    if has_time {
        format!("{base} %H:%M")
    } else {
        base
    }
}

/// Parse a date with a format, tolerating trailing time and a few common alternatives.
pub fn parse_date(value: &str, format: &str) -> Result<NaiveDate> {
    let v = value.trim();
    if v.is_empty() {
        return Err(Error::Parse("empty date".into()));
    }
    let with_time = format.contains("%H");
    let core = if with_time { v } else { v.split('T').next().unwrap_or(v).trim() };
    let core = if !with_time && core.len() > 10 && core.as_bytes().get(10) == Some(&b' ') { &core[..10] } else { core };
    let date_part = core.split(' ').next().unwrap_or(core);
    // The given format first, then the same field order with another separator: a bank that
    // writes 02-04-2022 one year and 02/04/2022 the next must not flip its months and days.
    let mut formats: Vec<String> = vec![format.to_string()];
    for sep in ['-', '/', '.'] {
        let alt: String = format.chars().map(|c| if matches!(c, '-' | '/' | '.') { sep } else { c }).collect();
        if !formats.contains(&alt) {
            formats.push(alt);
        }
    }
    for f in &formats {
        if let Ok(d) = NaiveDate::parse_from_str(core, f) {
            return Ok(d);
        }
        if f.contains("%H") {
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(core, f) {
                return Ok(dt.date());
            }
            let date_only = f.split(' ').next().unwrap_or(f);
            if let Ok(d) = NaiveDate::parse_from_str(date_part, date_only) {
                return Ok(d);
            }
        }
    }
    for alt in ["%Y-%m-%d", "%d/%m/%Y", "%d.%m.%Y", "%d-%m-%Y", "%Y/%m/%d", "%m/%d/%Y", "%d/%m/%y", "%Y%m%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(date_part, alt) {
            return Ok(d);
        }
    }
    Err(Error::Parse(format!("unrecognised date {value:?} (format {format})")))
}

/// Detect the decimal separator used in a column of numbers.
pub fn detect_decimal_sep(values: &[&str]) -> char {
    let mut comma = 0;
    let mut dot = 0;
    for v in values.iter().filter(|v| !v.trim().is_empty()).take(200) {
        match money::guess_decimal_sep(v) {
            ',' => comma += 1,
            _ => dot += 1,
        }
    }
    if comma > dot {
        ','
    } else {
        '.'
    }
}

pub fn parse_number(value: &str, decimal_sep: char) -> Result<Option<Decimal>> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    money::parse_localized(value, decimal_sep).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_header_after_preamble() {
        let text = "Extrato Conta Corrente\nConta;12345\nPeríodo;01/01/2026 a 31/01/2026\n\nData Lançamento;Histórico;Descrição;Valor;Saldo\n02/01/2026;Pix enviado;Padaria;-34,04;1.234,56\n03/01/2026;Pix recebido;Cliente;1.000,00;2.234,56\n";
        let t = parse_table(text, None, None).unwrap();
        assert_eq!(t.delimiter, ';');
        assert_eq!(t.header_row, 4);
        assert_eq!(t.headers[0], "Data Lançamento");
        assert_eq!(t.rows.len(), 2);
        assert_eq!(detect_date_format(&["02/01/2026", "31/01/2026"]), "%d/%m/%Y");
        assert_eq!(detect_decimal_sep(&["-34,04", "1.234,56"]), ',');
        assert_eq!(parse_number("1.234,56", ',').unwrap().unwrap().to_string(), "1234.56");
    }

    #[test]
    fn date_formats() {
        assert_eq!(detect_date_format(&["2026-01-02", "2026-01-03"]), "%Y-%m-%d");
        assert_eq!(detect_date_format(&["01/15/2026", "02/03/2026"]), "%m/%d/%Y");
        assert_eq!(detect_date_format(&["2026-01-02 10:11:12"]), "%Y-%m-%d %H:%M");
        assert_eq!(parse_date("2026-01-02 10:11:12", "%Y-%m-%d %H:%M").unwrap(), NaiveDate::from_ymd_opt(2026, 1, 2).unwrap());
        assert_eq!(parse_date("2026-01-02T10:11:12Z", "%Y-%m-%d").unwrap(), NaiveDate::from_ymd_opt(2026, 1, 2).unwrap());
        assert_eq!(parse_date("15/01/2026", "%d/%m/%Y").unwrap(), NaiveDate::from_ymd_opt(2026, 1, 15).unwrap());
    }
}
