//! OFX statements (1.x SGML and 2.x XML), as exported by Banco Inter and most banks.

use chrono::NaiveDate;
use rust_decimal::Decimal;

use super::{ParseOutput, ParsedLine};
use crate::money;
use crate::{Error, Result};

fn tag_value(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = block.find(&open)? + open.len();
    let rest = &block[start..];
    let end = rest.find('<').unwrap_or(rest.len());
    let v = rest[..end].trim();
    if v.is_empty() {
        None
    } else {
        Some(unescape(v))
    }
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'")
}

fn parse_ofx_date(s: &str) -> Option<NaiveDate> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    NaiveDate::parse_from_str(&digits[..8], "%Y%m%d").ok()
}

fn parse_amount(s: &str) -> Result<Decimal> {
    let sep = money::guess_decimal_sep(s);
    money::parse_localized(s, sep)
}

pub fn parse(content: &[u8]) -> Result<ParseOutput> {
    let text = super::csvkit::decode(content);
    let upper = text.to_uppercase();
    if !upper.contains("<STMTTRN>") && !upper.contains("<OFX>") {
        return Err(Error::Parse("not an OFX file (no <OFX> or <STMTTRN> tags)".into()));
    }
    let mut out = ParseOutput { detected_source: "inter_ofx".into(), ..Default::default() };
    out.currency = tag_value(&text, "CURDEF").unwrap_or_default().to_ascii_uppercase();
    if let Some(bal_block) = text.find("<LEDGERBAL>").map(|i| &text[i..]) {
        if let Some(b) = tag_value(bal_block, "BALAMT") {
            out.closing_balance = parse_amount(&b).ok();
        }
        if let Some(d) = tag_value(bal_block, "DTASOF") {
            out.closing_date = parse_ofx_date(&d);
        }
    }
    if let Some(v) = tag_value(&text, "DTSTART") {
        out.period_from = parse_ofx_date(&v);
    }
    if let Some(v) = tag_value(&text, "DTEND") {
        out.period_to = parse_ofx_date(&v);
    }
    out.account_ref = tag_value(&text, "ACCTID").unwrap_or_default();
    let mut rest = text.as_str();
    while let Some(start) = rest.find("<STMTTRN>") {
        let after = &rest[start + 9..];
        let end = after.find("</STMTTRN>").or_else(|| after.find("<STMTTRN>")).unwrap_or(after.len());
        let block = &after[..end];
        rest = &after[end..];
        let date = tag_value(block, "DTPOSTED").and_then(|d| parse_ofx_date(&d));
        let amount = match tag_value(block, "TRNAMT") {
            Some(a) => parse_amount(&a).ok(),
            None => None,
        };
        let name = tag_value(block, "NAME").unwrap_or_default();
        let memo = tag_value(block, "MEMO").unwrap_or_default();
        // Inter repeats the NAME at the start of the MEMO ("Pix enviado" / "Pix enviado: ..."): keep the memo alone then.
        let description = if name.is_empty() {
            memo.clone()
        } else if memo.is_empty() || memo.to_lowercase().starts_with(&name.to_lowercase()) {
            if memo.is_empty() {
                name.clone()
            } else {
                memo.clone()
            }
        } else {
            format!("{name} {memo}")
        };
        let trntype = tag_value(block, "TRNTYPE").unwrap_or_default();
        let refnum = tag_value(block, "REFNUM").unwrap_or_default();
        let fitid = tag_value(block, "FITID").unwrap_or_else(|| refnum.clone());
        let mut line = ParsedLine {
            date,
            amount,
            currency: out.currency.clone(),
            description: description.trim().to_string(),
            reference: fitid.clone(),
            raw: serde_json::json!({"TRNTYPE": trntype, "DTPOSTED": tag_value(block, "DTPOSTED"), "TRNAMT": tag_value(block, "TRNAMT"), "FITID": fitid, "NAME": name, "MEMO": memo, "CHECKNUM": tag_value(block, "CHECKNUM"), "REFNUM": refnum}),
            ..Default::default()
        };
        if line.date.is_none() || line.amount.is_none() {
            line.skip = Some("missing date or amount".into());
        }
        out.lines.push(line);
    }
    if out.lines.is_empty() {
        out.warnings.push("no transactions found in the OFX file".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sgml_ofx() {
        let text = "OFXHEADER:100\nDATA:OFXSGML\n<OFX><BANKMSGSRSV1><STMTTRNRS><STMTRS><CURDEF>BRL<BANKACCTFROM><ACCTID>12345-6</BANKACCTFROM>\n<BANKTRANLIST><DTSTART>20260101<DTEND>20260131\n<STMTTRN><TRNTYPE>DEBIT<DTPOSTED>20260102120000[-3:BRT]<TRNAMT>-34.04<FITID>2026010200001<MEMO>Pix enviado Padaria</STMTTRN>\n<STMTTRN><TRNTYPE>CREDIT<DTPOSTED>20260103<TRNAMT>1000.00<FITID>2026010300002<NAME>Cliente<MEMO>Pix recebido</STMTTRN>\n</BANKTRANLIST><LEDGERBAL><BALAMT>2234.56<DTASOF>20260131</LEDGERBAL></STMTRS></STMTTRNRS></BANKMSGSRSV1></OFX>";
        let out = parse(text.as_bytes()).unwrap();
        assert_eq!(out.currency, "BRL");
        assert_eq!(out.lines.len(), 2);
        assert_eq!(out.lines[0].amount.unwrap().to_string(), "-34.04");
        assert_eq!(out.lines[0].date.unwrap(), NaiveDate::from_ymd_opt(2026, 1, 2).unwrap());
        assert_eq!(out.lines[0].reference, "2026010200001");
        assert_eq!(out.lines[1].description, "Cliente Pix recebido");
        assert_eq!(out.closing_balance.unwrap().to_string(), "2234.56");
        assert_eq!(out.closing_date.unwrap(), NaiveDate::from_ymd_opt(2026, 1, 31).unwrap());
        assert_eq!(out.account_ref, "12345-6");
    }
}
