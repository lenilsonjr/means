//! Remessa Online CSV: each row is an exchange (BRL out, foreign currency in, IOF).
//! Rows become draft exchange entries between two accounts; the bank statements then
//! match the two bank postings.

use rusqlite::{params, Connection};
use rust_decimal::Decimal;

use super::csvkit::{self, find_col};
use super::{CsvMapping, ImportOutcome, ParseOutput, ParsedLine};
use crate::accounts;
use crate::model::*;
use crate::money;
use crate::{new_uid, now_ts, Error, Result};

/// Strip a currency code or symbol from a foreign amount cell ("USD 1.000,00", "1,000.00 USD", "US$ 500,00").
fn split_currency(cell: &str) -> Option<String> {
    let t = cell.trim();
    for tok in t.split(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        if tok.len() == 3 && tok.chars().all(|c| c.is_ascii_alphabetic()) {
            return Some(tok.to_ascii_uppercase());
        }
    }
    let symbols: &[(&str, &str)] = &[("US$", "USD"), ("U$", "USD"), ("€", "EUR"), ("£", "GBP"), ("¥", "JPY"), ("C$", "CAD"), ("A$", "AUD"), ("CHF", "CHF"), ("$", "USD")];
    symbols.iter().find(|(sym, _)| t.contains(sym)).map(|(_, code)| code.to_string())
}

/// The BRL side of an exchange as it hit the bank account. Remessa's VET (valor efetivo total) is
/// the all-in rate, so VET × foreign amount is the total charged (outbound) or the net received
/// (inbound). When the BRL column is the principal instead, IOF and the fee are added or removed
/// to reach that total; without a VET the BRL column is taken as the total.
fn settle_brl(brl: Decimal, iof: Decimal, fee: Decimal, vet: Option<Decimal>, fx: Decimal, outbound: bool) -> Decimal {
    let Some(vet) = vet.filter(|v| !v.is_zero()) else { return brl };
    let expected = (vet * fx).round_dp(2);
    let adjusted = if outbound { brl + iof + fee } else { brl - iof - fee };
    let (d_brl, d_adj) = ((brl - expected).abs(), (adjusted - expected).abs());
    // A VET is quoted to four decimals, so the closer candidate within 1% is the total.
    let tol = (expected.abs() * Decimal::new(1, 2)).max(Decimal::ONE);
    if d_adj < d_brl && d_adj <= tol {
        adjusted
    } else {
        brl
    }
}

/// Columns of the Remessa Online extrato CSV (help centre, PJ export): Data de operação, Direção,
/// Tipo de operação, Contraparte, Valor da moeda estrangeira, Valor na moeda em real, Spread,
/// IOF, VET. Older and personal exports are accepted through the alternative names below; a
/// currency column, or a code inside the foreign amount, or `mapping.currency` names the currency.
pub fn parse(content: &[u8], mapping: Option<&CsvMapping>) -> Result<ParseOutput> {
    let text = csvkit::decode(content);
    let (delim, header_row) = match mapping {
        Some(m) => (m.delimiter.chars().next().filter(|c| *c != ' '), if m.header_row > 0 { Some(m.header_row as usize) } else { None }),
        None => (None, None),
    };
    let table = csvkit::parse_table(&text, delim, header_row)?;
    let h = &table.headers;
    let col = |names: &[&str]| find_col(h, names);
    let date_i =
        col(&["Data de operação", "Data da operação", "Data de operacao", "Data da operacao", "Data", "Date", "Criado em"]).ok_or_else(|| Error::Parse("Remessa CSV: no date column".into()))?;
    let vet_i = col(&["VET", "Valor Efetivo Total"]);
    let fx_i = col(&["Valor da moeda estrangeira", "Valor em moeda estrangeira", "Valor moeda estrangeira", "Valor enviado", "Valor recebido", "Valor estrangeiro", "Valor na moeda", "Amount"]);
    let brl_i = col(&["Valor na moeda em real", "Valor em reais", "Valor em real", "Valor em BRL", "Valor (BRL)", "Total em reais", "Valor total", "Total (BRL)", "Valor BRL", "Total"])
        .filter(|i| Some(*i) != vet_i && Some(*i) != fx_i);
    let cur_i = col(&["Moeda", "Moeda estrangeira", "Currency"]).filter(|i| Some(*i) != fx_i && Some(*i) != brl_i);
    let iof_i = col(&["IOF"]);
    let spread_i = col(&["Spread"]);
    let rate_i = col(&["Taxa de câmbio", "Taxa de cambio", "Câmbio", "Cambio", "Cotação", "Cotacao", "Exchange rate", "Rate"]);
    let fee_i = col(&["Tarifa", "Taxa de serviço", "Taxa de servico", "Fee", "Custo"]);
    let dir_i = col(&["Direção", "Direcao", "Direction", "Sentido"]);
    let kind_i = col(&["Tipo de operação", "Tipo de operacao", "Natureza", "Motivo", "Finalidade", "Tipo"]);
    let party_i = col(&["Contraparte", "Beneficiário", "Beneficiario", "Pagador", "Nome"]);
    let ref_i = col(&["Contrato", "Número do contrato", "Numero do contrato", "Nº do contrato", "Identificador", "ID", "Código", "Codigo"]);
    let (Some(brl_i), Some(fx_i)) = (brl_i, fx_i) else {
        return Err(Error::Parse(format!("Remessa CSV: could not find the BRL and foreign amount columns in {:?}", h)));
    };
    let dates: Vec<&str> = table.rows.iter().filter_map(|r| r.get(date_i)).map(|s| s.as_str()).collect();
    let fmt = csvkit::detect_date_format(&dates);
    let sep_of = |i: usize| {
        let v: Vec<&str> = table.rows.iter().filter_map(|r| r.get(i)).map(|s| s.as_str()).collect();
        csvkit::detect_decimal_sep(&v)
    };
    let brl_sep = sep_of(brl_i);
    let fx_sep = sep_of(fx_i);
    let default_currency = mapping.map(|m| m.currency.trim().to_ascii_uppercase()).unwrap_or_default();
    let mut out = ParseOutput {
        headers: h.clone(),
        detected_source: "remessa_csv".into(),
        currency: "BRL".into(),
        sample_rows: table.rows.iter().take(8).map(|r| r.join(" | ")).collect(),
        ..Default::default()
    };
    for row in table.rows.iter() {
        let cell = |i: Option<usize>| i.and_then(|i| row.get(i)).map(|s| s.trim().to_string()).unwrap_or_default();
        let mut raw = serde_json::Map::new();
        for (i, name) in h.iter().enumerate() {
            if let Some(v) = row.get(i) {
                if !v.is_empty() {
                    raw.insert(name.clone(), serde_json::Value::String(v.clone()));
                }
            }
        }
        let mut line = ParsedLine { raw: serde_json::Value::Object(raw), ..Default::default() };
        line.date = csvkit::parse_date(&cell(Some(date_i)), &fmt).ok();
        let brl = csvkit::parse_number(&cell(Some(brl_i)), brl_sep).ok().flatten();
        let fx_cell = cell(Some(fx_i));
        let fx = csvkit::parse_number(&fx_cell, fx_sep).ok().flatten().map(|v| v.abs());
        let iof = csvkit::parse_number(&cell(iof_i), brl_sep).ok().flatten().unwrap_or(Decimal::ZERO).abs();
        let fee = csvkit::parse_number(&cell(fee_i), brl_sep).ok().flatten().unwrap_or(Decimal::ZERO).abs();
        let vet = csvkit::parse_number(&cell(vet_i), brl_sep).ok().flatten();
        let mut currency = cell(cur_i).to_ascii_uppercase();
        if currency.is_empty() {
            currency = split_currency(&fx_cell).unwrap_or_else(|| default_currency.clone());
        }
        let kind = cell(kind_i);
        let party = cell(party_i);
        let direction = cell(dir_i).to_lowercase();
        let inbound_words = |s: &str| s.contains("receb") || s.contains("inbound") || s.contains("entrada") || s == "in";
        // Direção says which way the money went; without it a negative BRL amount or a
        // "recebimento" in the operation type marks an inbound exchange.
        let outbound = if !direction.is_empty() {
            !inbound_words(&direction)
        } else if brl.is_some_and(|b| b.is_sign_negative()) {
            true
        } else {
            !inbound_words(&kind.to_lowercase())
        };
        let total = match (brl, fx) {
            (Some(b), Some(f)) => Some(settle_brl(b.abs(), iof, fee, vet, f, outbound)),
            (Some(b), None) => Some(b.abs()),
            _ => None,
        };
        line.amount = total.map(|t| if outbound { -t } else { t });
        line.currency = "BRL".into();
        line.reference = cell(ref_i);
        let mut description = format!("Remessa Online {} {} {}", if outbound { "→" } else { "←" }, fx.map(money::plain).unwrap_or_default(), currency);
        if !party.is_empty() {
            description.push(' ');
            description.push_str(&party);
        }
        if !kind.is_empty() {
            description.push_str(&format!(" ({kind})"));
        }
        line.description = description.split_whitespace().collect::<Vec<_>>().join(" ");
        let rate = match vet {
            Some(v) => money::plain(v),
            None => cell(rate_i),
        };
        line.raw["_exchange"] = serde_json::json!({
            "outbound": outbound,
            "foreign_quantity": fx.map(money::plain),
            "foreign_currency": currency,
            "iof": money::plain(iof),
            "fee": money::plain(fee),
            "rate": rate,
            "spread": cell(spread_i),
            "counterparty": party,
            "kind": kind,
            "brl_total": total.map(money::plain),
        });
        let mut missing = Vec::new();
        if line.date.is_none() {
            missing.push("date");
        }
        if brl.is_none() {
            missing.push("BRL amount");
        }
        if fx.is_none() {
            missing.push("foreign amount");
        }
        if currency.is_empty() {
            missing.push("currency (add a Moeda column or set the currency in the mapping)");
        }
        if !missing.is_empty() {
            line.skip = Some(format!("missing {}", missing.join(", ")));
        }
        out.lines.push(line);
    }
    Ok(out)
}

/// Create draft exchange entries. `options` must carry `to_account_id` (the foreign-currency account)
/// and may carry `iof_account_id` and `fee_account_id`.
pub fn run(conn: &mut Connection, parse: ParseOutput, checksum: &str, request: super::ImportRequest<'_>) -> Result<ImportOutcome> {
    let super::ImportRequest { account_id: from_account_id, filename, content, preview, options, .. } = request;
    let from_id = from_account_id.ok_or_else(|| Error::Invalid("choose the BRL account the remittances left from".into()))?;
    let from = accounts::get_account(conn, from_id)?;
    let to_id = options.get("to_account_id").and_then(|v| v.as_i64()).filter(|v| *v > 0);
    let to = to_id.map(|id| accounts::get_account(conn, id)).transpose()?;
    let mut lines: Vec<StatementLine> = parse
        .lines
        .iter()
        .enumerate()
        .map(|(i, pl)| {
            // Rows in another currency than the chosen foreign account wait for their own import.
            let foreign = pl.raw["_exchange"]["foreign_currency"].as_str().unwrap_or_default().to_string();
            let (status, note) = match (&pl.skip, &to) {
                (Some(reason), _) => ("skipped", reason.clone()),
                (None, Some(to)) if !foreign.is_empty() && foreign != to.commodity => ("skipped", format!("{foreign} exchange; choose the {foreign} account")),
                _ => ("unmatched", String::new()),
            };
            Ok(StatementLine {
                id: 0,
                import_id: 0,
                account_id: Some(from_id),
                position: i as i32 + 1,
                raw: pl.raw.clone(),
                date: pl.date,
                amount: pl.amount.map(|a| crate::money::Money::from_major(a, &from.commodity, from.precision)).transpose()?,
                currency: "BRL".into(),
                description: pl.description.clone(),
                reference: pl.reference.clone(),
                balance_after: None,
                fingerprint: pl.date.zip(pl.amount).map(|(d, a)| super::fingerprint(from_id, d, a, &pl.description, i)).unwrap_or_default(),
                posting_id: None,
                journal_entry_id: None,
                duplicate_of_id: None,
                status: status.into(),
                note,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut import = Import {
        id: 0,
        uid: new_uid(),
        source: "remessa_csv".into(),
        account_id: Some(from_id),
        filename: filename.to_string(),
        checksum: checksum.to_string(),
        status: if preview { "preview".into() } else { "done".into() },
        period_from: lines.iter().filter_map(|l| l.date).min(),
        period_to: lines.iter().filter_map(|l| l.date).max(),
        opening_balance: None,
        closing_balance: None,
        lines_count: lines.len() as i32,
        created_count: 0,
        matched_count: 0,
        duplicate_count: 0,
        skipped_count: lines.iter().filter(|l| l.status == "skipped").count() as i32,
        unmatched_count: 0,
        error_count: 0,
        error: String::new(),
        options: options.clone(),
        created_at: now_ts(),
    };
    if preview {
        return Ok(ImportOutcome { import, lines, parse });
    }
    let to = to.ok_or_else(|| Error::Invalid("choose the account that received the foreign currency (to_account_id)".into()))?;
    if to.entity_id != from.entity_id {
        return Err(Error::Invalid("Remessa imports need both accounts in the same entity; use a transfer between entities otherwise".into()));
    }
    let iof_account = match options.get("iof_account_id").and_then(|v| v.as_i64()).filter(|v| *v > 0) {
        Some(id) => accounts::get_account(conn, id)?,
        None => accounts::ensure_account(conn, from.entity_id, AccountType::Expense, &["Taxes", "IOF"], "expense", "")?,
    };
    let fee_account = match options.get("fee_account_id").and_then(|v| v.as_i64()).filter(|v| *v > 0) {
        Some(id) => accounts::get_account(conn, id)?,
        None => accounts::ensure_account(conn, from.entity_id, AccountType::Expense, &["Bank fees"], "expense", "")?,
    };
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO imports (uid, source, account_id, filename, checksum, status, period_from, period_to, lines_count, skipped_count, options, content, created_at)
         VALUES (?1, 'remessa_csv', ?2, ?3, ?4, 'done', ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            import.uid,
            from_id,
            filename,
            checksum,
            import.period_from.map(|d| d.to_string()),
            import.period_to.map(|d| d.to_string()),
            import.lines_count,
            import.skipped_count,
            options.to_string(),
            content,
            import.created_at
        ],
    )?;
    import.id = tx.last_insert_rowid();
    for sl in lines.iter_mut() {
        sl.import_id = import.id;
        sl.id = super::insert_line(&tx, sl)?;
    }
    tx.commit()?;
    for sl in lines.iter_mut() {
        if sl.status == "skipped" {
            continue;
        }
        if let Err(err) = process_line(conn, &from, &to, &iof_account, &fee_account, sl) {
            sl.status = "error".into();
            sl.note = err.to_string();
            conn.execute("UPDATE statement_lines SET status = 'error', note = ?2 WHERE id = ?1", params![sl.id, sl.note])?;
        }
        match sl.status.as_str() {
            "created" => import.created_count += 1,
            "duplicate" => import.duplicate_count += 1,
            "error" => import.error_count += 1,
            _ => {}
        }
    }
    conn.execute("UPDATE imports SET created_count = ?2, duplicate_count = ?3, error_count = ?4 WHERE id = ?1", params![import.id, import.created_count, import.duplicate_count, import.error_count])?;
    Ok(ImportOutcome { import: super::get_import(conn, import.id)?.0, lines, parse })
}

use rusqlite::OptionalExtension;

fn process_line(conn: &mut Connection, from: &Account, to: &Account, iof_account: &Account, fee_account: &Account, sl: &mut StatementLine) -> Result<()> {
    let dup: Option<i64> = conn
        .query_row("SELECT id FROM statement_lines WHERE account_id = ?1 AND fingerprint = ?2 AND id <> ?3 AND status = 'created'", params![from.id, sl.fingerprint, sl.id], |r| r.get(0))
        .optional()?;
    if let Some(d) = dup {
        sl.status = "duplicate".into();
        sl.duplicate_of_id = Some(d);
        conn.execute("UPDATE statement_lines SET status = 'duplicate', duplicate_of_id = ?2, note = '' WHERE id = ?1", params![sl.id, d])?;
        return Ok(());
    }
    let ex = &sl.raw["_exchange"];
    let outbound = ex["outbound"].as_bool().unwrap_or(true);
    let fx_q = ex["foreign_quantity"].as_str().and_then(|s| money::parse(s).ok()).unwrap_or(Decimal::ZERO);
    let iof = ex["iof"].as_str().and_then(|s| money::parse(s).ok()).unwrap_or(Decimal::ZERO);
    let fee = ex["fee"].as_str().and_then(|s| money::parse(s).ok()).unwrap_or(Decimal::ZERO);
    let brl = sl.amount.map(|a| a.major()).unwrap_or(Decimal::ZERO).abs();
    let date = sl.date.ok_or_else(|| Error::Invalid("line has no date".into()))?;
    let mut input = EntryInput::new(from.entity_id, date);
    input.payee = "Remessa Online".into();
    input.description = sl.description.clone();
    input.status = EntryStatus::Draft;
    input.origin = "import".into();
    input.absorb_fx = true;
    if outbound {
        // Money leaves the BRL account: the total charged is the BRL amount; IOF and fee are part of it.
        let principal = brl - iof - fee;
        input.postings.push(PostingInput::new(from.id, -brl).external(Some(sl.reference.clone()).filter(|s| !s.is_empty()), Some(sl.fingerprint.clone())));
        if !iof.is_zero() {
            input.postings.push(PostingInput::valued(iof_account.id, iof, "BRL").memo("IOF"));
        }
        if !fee.is_zero() {
            input.postings.push(PostingInput::valued(fee_account.id, fee, "BRL").memo("fee"));
        }
        input.postings.push(PostingInput::new(to.id, fx_q.abs()).meta("principal_brl", serde_json::json!(money::plain(principal))));
    } else {
        input.postings.push(PostingInput::new(from.id, brl).external(Some(sl.reference.clone()).filter(|s| !s.is_empty()), Some(sl.fingerprint.clone())));
        if !iof.is_zero() {
            input.postings.push(PostingInput::valued(iof_account.id, iof, "BRL").memo("IOF"));
        }
        if !fee.is_zero() {
            input.postings.push(PostingInput::valued(fee_account.id, fee, "BRL").memo("fee"));
        }
        input.postings.push(PostingInput::new(to.id, -fx_q.abs()));
    }
    let tx = conn.transaction()?;
    let entry = crate::journal::create_entry_in_transaction(&tx, input)?;
    tx.execute("UPDATE statement_lines SET status = 'created', journal_entry_id = ?2, note = '' WHERE id = ?1", params![sl.id, entry.id])?;
    tx.commit()?;
    sl.status = "created".into();
    sl.journal_entry_id = Some(entry.id);
    Ok(())
}

pub(super) fn retry_line(conn: &mut Connection, from: &Account, sl: &mut StatementLine, options: &serde_json::Value) -> Result<()> {
    let to_id =
        options.get("to_account_id").and_then(|v| v.as_i64()).filter(|v| *v > 0).ok_or_else(|| Error::Invalid("choose the account that received the foreign currency (to_account_id)".into()))?;
    let to = accounts::get_account(conn, to_id)?;
    if to.entity_id != from.entity_id {
        return Err(Error::Invalid("Remessa imports need both accounts in the same entity".into()));
    }
    let iof = match options.get("iof_account_id").and_then(|v| v.as_i64()).filter(|v| *v > 0) {
        Some(id) => accounts::get_account(conn, id)?,
        None => accounts::ensure_account(conn, from.entity_id, AccountType::Expense, &["Taxes", "IOF"], "expense", "")?,
    };
    let fee = match options.get("fee_account_id").and_then(|v| v.as_i64()).filter(|v| *v > 0) {
        Some(id) => accounts::get_account(conn, id)?,
        None => accounts::ensure_account(conn, from.entity_id, AccountType::Expense, &["Bank fees"], "expense", "")?,
    };
    process_line(conn, from, &to, &iof, &fee, sl)
}
