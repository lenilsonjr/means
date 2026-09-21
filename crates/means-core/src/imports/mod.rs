//! Imports: files in, statement lines kept verbatim, journal entries out.

pub mod account_tracker;
pub mod bank_api;
mod coverage;
pub mod csvkit;
pub mod email;
pub mod enable_banking;
pub mod inbox;
pub mod mercury;
pub mod ofx;
pub mod pluggy;
pub mod presets;
pub mod remessa;

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};

use crate::accounts;
use crate::model::*;
use crate::money::{self, Money};
use crate::{new_uid, now_ts, Error, Result};

/// A parsed record before it becomes a statement line.
#[derive(Debug, Clone, Default)]
pub struct ParsedLine {
    pub date: Option<NaiveDate>,
    /// Signed as the bank shows it: positive is money in.
    pub amount: Option<Decimal>,
    pub currency: String,
    pub description: String,
    pub reference: String,
    pub balance_after: Option<Decimal>,
    pub raw: serde_json::Value,
    pub skip: Option<String>,
    pub original: Option<(Decimal, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct ParseOutput {
    pub lines: Vec<ParsedLine>,
    pub headers: Vec<String>,
    pub sample_rows: Vec<String>,
    pub currency: String,
    pub opening_balance: Option<Decimal>,
    pub closing_balance: Option<Decimal>,
    pub closing_date: Option<NaiveDate>,
    pub period_from: Option<NaiveDate>,
    pub period_to: Option<NaiveDate>,
    pub detected_source: String,
    pub account_ref: String,
    pub warnings: Vec<String>,
    /// Records the parser refused outright, which therefore have no line of their own: a Pluggy
    /// transaction that is not settled yet. They are counted among the import's skipped ones.
    pub skipped_records: usize,
}

/// Column mapping for CSV files (mirrors the proto message).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CsvMapping {
    pub delimiter: String,
    pub header_row: i32,
    pub date_column: String,
    pub date_format: String,
    pub amount_column: String,
    pub debit_column: String,
    pub credit_column: String,
    pub description_column: String,
    pub reference_column: String,
    pub balance_column: String,
    pub currency_column: String,
    pub decimal_separator: String,
    pub invert_sign: bool,
    pub currency: String,
    pub extra_description_columns: Vec<String>,
}

impl CsvMapping {
    fn is_empty(&self) -> bool {
        self.date_column.is_empty() && self.amount_column.is_empty() && self.debit_column.is_empty()
    }
}

pub fn checksum(content: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(content);
    hex::encode(h.finalize())
}

/// Guess the source from the file name and content.
pub fn detect_source(filename: &str, content: &[u8]) -> String {
    let lower = filename.to_lowercase();
    if lower.ends_with(".atb") || content.starts_with(b"bplist00") {
        return "account_tracker".into();
    }
    if lower.ends_with(".ofx") || lower.ends_with(".qfx") {
        return "inter_ofx".into();
    }
    if lower.ends_with(".json") && pluggy::is_pluggy_file(content) {
        return pluggy::SOURCE.into();
    }
    if lower.ends_with(".json") && enable_banking::is_file(content) {
        return enable_banking::SOURCE.into();
    }
    if lower.ends_with(".json") && mercury::is_file(content) {
        return mercury::SOURCE.into();
    }
    if lower.ends_with(".json") {
        if let Some(source) = bank_api::source(content) {
            return source.into();
        }
    }
    let head: String = csvkit::decode(&content[..content.len().min(4000)]);
    if head.to_uppercase().contains("<OFX>") || head.to_uppercase().contains("OFXHEADER") {
        return "inter_ofx".into();
    }
    if let Ok(table) = csvkit::parse_table(&head, None, None) {
        if let Some(s) = presets::detect(&table.headers) {
            return s.into();
        }
    }
    "generic_csv".into()
}

/// Parse a file into lines. `mapping` is required for generic_csv and may override presets.
pub fn parse(source: &str, content: &[u8], mapping: Option<&CsvMapping>) -> Result<ParseOutput> {
    match source {
        "inter_ofx" | "ofx" => ofx::parse(content),
        "remessa_csv" => remessa::parse(content, mapping),
        "pluggy_json" => pluggy::parse(content),
        "enable_banking_json" => enable_banking::parse(content),
        "mercury_json" => mercury::parse(content),
        bank_api::INTER | bank_api::WISE => bank_api::parse(content),
        "account_tracker" => Err(Error::Invalid("Account Tracker backups use ImportAccountTracker".into())),
        _ => parse_csv(source, content, mapping),
    }
}

fn parse_csv(source: &str, content: &[u8], mapping: Option<&CsvMapping>) -> Result<ParseOutput> {
    let text = csvkit::decode(content);
    let (delim, header_row) = match mapping {
        Some(m) => (m.delimiter.chars().next().filter(|c| *c != ' '), if m.header_row > 0 { Some(m.header_row as usize) } else { None }),
        None => (None, None),
    };
    let delim = delim.or_else(|| if source == "inter_csv" { Some(';') } else { None });
    let table = csvkit::parse_table(&text, delim, header_row)?;
    let detected = presets::detect(&table.headers).map(|s| s.to_string()).unwrap_or_else(|| "generic_csv".into());
    let effective_source = if source == "generic_csv" || source.is_empty() { detected.clone() } else { source.to_string() };
    let mut m = match mapping {
        Some(m) if !m.is_empty() => m.clone(),
        _ => presets::mapping_for(&effective_source, &table.headers).unwrap_or_default(),
    };
    let mut out = ParseOutput { headers: table.headers.clone(), sample_rows: table.rows.iter().take(8).map(|r| r.join(" | ")).collect(), detected_source: detected.clone(), ..Default::default() };
    if m.is_empty() {
        out.warnings.push("columns could not be recognised; map them by hand".into());
        return Ok(out);
    }
    let headers = &table.headers;
    let date_i = csvkit::col_by_name(headers, &m.date_column).ok_or_else(|| Error::Parse(format!("date column {:?} not found", m.date_column)))?;
    let amount_i = csvkit::col_by_name(headers, &m.amount_column);
    let debit_i = csvkit::col_by_name(headers, &m.debit_column);
    let credit_i = csvkit::col_by_name(headers, &m.credit_column);
    if amount_i.is_none() && debit_i.is_none() && credit_i.is_none() {
        return Err(Error::Parse("an amount column (or debit/credit columns) is required".into()));
    }
    let desc_i = csvkit::col_by_name(headers, &m.description_column);
    let extra_i: Vec<usize> = m.extra_description_columns.iter().filter_map(|c| csvkit::col_by_name(headers, c)).collect();
    let ref_i = csvkit::col_by_name(headers, &m.reference_column);
    let bal_i = csvkit::col_by_name(headers, &m.balance_column);
    let cur_i = csvkit::col_by_name(headers, &m.currency_column);
    if m.date_format.is_empty() {
        let samples: Vec<&str> = table.rows.iter().filter_map(|r| r.get(date_i)).map(|s| s.as_str()).collect();
        m.date_format = csvkit::detect_date_format(&samples);
    }
    let sep = if m.decimal_separator.is_empty() {
        let col = amount_i.or(debit_i).unwrap_or(0);
        let samples: Vec<&str> = table.rows.iter().filter_map(|r| r.get(col)).map(|s| s.as_str()).collect();
        csvkit::detect_decimal_sep(&samples)
    } else {
        m.decimal_separator.chars().next().unwrap_or('.')
    };
    let default_currency = m.currency.trim().to_ascii_uppercase();
    for (ri, row) in table.rows.iter().enumerate() {
        let mut raw = serde_json::Map::new();
        for (i, h) in headers.iter().enumerate() {
            if let Some(v) = row.get(i) {
                if !v.is_empty() {
                    raw.insert(h.clone(), serde_json::Value::String(v.clone()));
                }
            }
        }
        let mut line = ParsedLine { raw: serde_json::Value::Object(raw), ..Default::default() };
        let date_s = row.get(date_i).cloned().unwrap_or_default();
        match csvkit::parse_date(&date_s, &m.date_format) {
            Ok(d) => line.date = Some(d),
            Err(e) => {
                line.skip = Some(format!("row {}: {e}", ri + 1));
            }
        }
        let amount = match (amount_i, debit_i, credit_i) {
            (Some(ai), _, _) => csvkit::parse_number(row.get(ai).map(|s| s.as_str()).unwrap_or(""), sep)?,
            (None, di, ci) => {
                let d = di.and_then(|i| csvkit::parse_number(row.get(i).map(|s| s.as_str()).unwrap_or(""), sep).ok().flatten());
                let c = ci.and_then(|i| csvkit::parse_number(row.get(i).map(|s| s.as_str()).unwrap_or(""), sep).ok().flatten());
                match (d, c) {
                    (Some(d), _) if !d.is_zero() => Some(-d.abs()),
                    (_, Some(c)) => Some(c.abs()),
                    _ => None,
                }
            }
        };
        line.amount = amount.map(|a| if m.invert_sign { -a } else { a });
        if line.amount.is_none() && line.skip.is_none() {
            line.skip = Some(format!("row {}: no amount", ri + 1));
        }
        let mut desc = desc_i.and_then(|i| row.get(i)).cloned().unwrap_or_default();
        for i in &extra_i {
            if let Some(v) = row.get(*i) {
                if !v.is_empty() && !desc.contains(v.as_str()) {
                    if !desc.is_empty() {
                        desc.push(' ');
                    }
                    desc.push_str(v);
                }
            }
        }
        line.description = desc.split_whitespace().collect::<Vec<_>>().join(" ");
        line.reference = ref_i.and_then(|i| row.get(i)).cloned().unwrap_or_default();
        line.balance_after = bal_i.and_then(|i| row.get(i)).and_then(|s| csvkit::parse_number(s, sep).ok().flatten());
        line.currency = cur_i.and_then(|i| row.get(i)).map(|s| s.trim().to_ascii_uppercase()).filter(|s| !s.is_empty()).unwrap_or_else(|| default_currency.clone());
        let mut extra = Vec::new();
        presets::post_process(&effective_source, &table, row, &mut line, &mut extra);
        out.lines.push(line);
        out.lines.extend(extra);
    }
    if out.currency.is_empty() {
        out.currency = out.lines.iter().find(|l| !l.currency.is_empty()).map(|l| l.currency.clone()).unwrap_or(default_currency);
    }
    // Closing balance from the last dated line with a running balance.
    let mut dated: Vec<&ParsedLine> = out.lines.iter().filter(|l| l.skip.is_none() && l.date.is_some()).collect();
    dated.sort_by_key(|l| l.date);
    if let Some(last) = dated.iter().rev().find(|l| l.balance_after.is_some()) {
        out.closing_balance = last.balance_after;
        out.closing_date = last.date;
    }
    if let (Some(first), Some(last)) = (dated.first(), dated.last()) {
        out.period_from = first.date;
        out.period_to = last.date;
    }
    Ok(out)
}

fn normalize_desc(s: &str) -> String {
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn fingerprint(account_id: i64, date: NaiveDate, amount: Decimal, description: &str, nth: usize) -> String {
    let mut h = Sha256::new();
    h.update(format!("{account_id}|{date}|{}|{}|{nth}", money::plain(amount), normalize_desc(description)).as_bytes());
    hex::encode(&h.finalize()[..16])
}

pub struct ImportOutcome {
    pub import: Import,
    pub lines: Vec<StatementLine>,
    pub parse: ParseOutput,
}

/// A captured entry and its bank line can be a few days apart (card bookings, weekends).
pub(crate) const MATCH_WINDOW_DAYS: i64 = 5;

/// One file to import, and how to read it. `source` may be "auto" or "" to detect it from the
/// file, and `preview` parses without writing anything.
pub struct ImportRequest<'a> {
    pub source: &'a str,
    pub account_id: Option<i64>,
    pub filename: &'a str,
    pub content: &'a [u8],
    pub mapping: Option<&'a CsvMapping>,
    pub preview: bool,
    pub options: serde_json::Value,
}

impl<'a> ImportRequest<'a> {
    pub fn new(source: &'a str, account_id: Option<i64>, filename: &'a str, content: &'a [u8]) -> ImportRequest<'a> {
        ImportRequest { source, account_id, filename, content, mapping: None, preview: false, options: serde_json::json!({}) }
    }

    pub fn mapping(mut self, mapping: Option<&'a CsvMapping>) -> ImportRequest<'a> {
        self.mapping = mapping;
        self
    }

    pub fn preview(mut self, preview: bool) -> ImportRequest<'a> {
        self.preview = preview;
        self
    }

    pub fn options(mut self, options: serde_json::Value) -> ImportRequest<'a> {
        self.options = options;
        self
    }
}

/// The pipeline: receive, parse, dedupe, match, draft, reconcile.
pub fn run_import(conn: &mut Connection, request: ImportRequest<'_>) -> Result<ImportOutcome> {
    let ImportRequest { source, account_id, filename, content, mapping, preview, options } = request;
    let source = if source.is_empty() || source == "auto" { detect_source(filename, content) } else { source.to_string() };
    if source == "account_tracker" {
        return Err(Error::Invalid("use ImportAccountTracker for Account Tracker backups".into()));
    }
    let sum = checksum(content);
    if !preview {
        let dup: Option<i64> =
            conn.query_row("SELECT id FROM imports WHERE source = ?1 AND COALESCE(account_id, 0) = COALESCE(?2, 0) AND checksum = ?3", params![source, account_id, sum], |r| r.get(0)).optional()?;
        if let Some(id) = dup {
            return Err(Error::Conflict(format!("this file was already imported (import #{id})")));
        }
    }
    let mut parse = parse(&source, content, mapping)?;
    if source == "remessa_csv" {
        return remessa::run(conn, parse, &sum, ImportRequest::new(&source, account_id, filename, content).preview(preview).options(options));
    }
    if preview && account_id.is_none() {
        // Untyped amounts remain in parse.lines until an account supplies a unit.
        // Detect and parse only: the screen shows headers and sample lines before an account is chosen.
        let lines: Vec<StatementLine> = parse
            .lines
            .iter()
            .enumerate()
            .map(|(i, pl)| StatementLine {
                id: 0,
                import_id: 0,
                account_id: None,
                position: i as i32 + 1,
                raw: pl.raw.clone(),
                date: pl.date,
                amount: None,
                currency: pl.currency.clone(),
                description: pl.description.clone(),
                reference: pl.reference.trim().to_string(),
                balance_after: None,
                fingerprint: String::new(),
                posting_id: None,
                journal_entry_id: None,
                duplicate_of_id: None,
                status: if pl.skip.is_some() { "skipped".into() } else { "unmatched".into() },
                note: pl.skip.clone().unwrap_or_default(),
            })
            .collect();
        let import = Import {
            id: 0,
            uid: String::new(),
            source: source.clone(),
            account_id: None,
            filename: filename.to_string(),
            checksum: sum,
            status: "preview".into(),
            period_from: parse.period_from,
            period_to: parse.period_to,
            opening_balance: parse.opening_balance,
            closing_balance: parse.closing_balance,
            lines_count: lines.len() as i32,
            created_count: 0,
            matched_count: 0,
            duplicate_count: 0,
            skipped_count: lines.iter().filter(|l| l.status == "skipped").count() as i32 + parse.skipped_records as i32,
            unmatched_count: 0,
            error_count: 0,
            error: String::new(),
            options: serde_json::json!({"currency": parse.currency}),
            created_at: now_ts(),
        };
        return Ok(ImportOutcome { import, lines, parse });
    }
    let account_id = account_id.ok_or_else(|| Error::Invalid("choose the account this statement belongs to".into()))?;
    let account = accounts::get_account(conn, account_id)?;
    if matches!(source.as_str(), enable_banking::SOURCE | mercury::SOURCE | bank_api::INTER | bank_api::WISE) && parse.currency != account.commodity {
        return Err(Error::Invalid(format!("Bank API file currency {} does not match account currency {}", parse.currency, account.commodity)));
    }
    if matches!(source.as_str(), bank_api::INTER | bank_api::WISE) {
        bank_api::validate_destination(content, &account)?;
    }
    if source == mercury::SOURCE {
        mercury::validate_destination(content, &account)?;
    }
    if account.placeholder {
        return Err(Error::Invalid("statements cannot be imported into a placeholder account".into()));
    }
    // A multi-currency file (Wise history, a Revolut statement over several balances) keeps the
    // lines in this account's currency and skips the rest; a file with none of them is the wrong file.
    let live = parse.lines.iter().filter(|l| l.skip.is_none()).count();
    let foreign = parse.lines.iter().filter(|l| l.skip.is_none() && !l.currency.is_empty() && l.currency != account.commodity).count();
    if live > 0 && foreign == live {
        let cur = parse.lines.iter().find(|l| l.skip.is_none()).map(|l| l.currency.clone()).unwrap_or_default();
        return Err(Error::Invalid(format!("the file is in {cur} but {} is a {} account", account.path, account.commodity)));
    }
    let mut lines: Vec<StatementLine> = Vec::with_capacity(parse.lines.len());
    let mut seen: std::collections::HashMap<(NaiveDate, String, String), usize> = std::collections::HashMap::new();
    for (i, pl) in parse.lines.iter().enumerate() {
        let mut sl = StatementLine {
            id: 0,
            import_id: 0,
            account_id: Some(account_id),
            position: i as i32 + 1,
            raw: pl.raw.clone(),
            date: pl.date,
            amount: pl.amount.map(|a| Money::from_major(a, &account.commodity, account.precision)).transpose()?,
            currency: if pl.currency.is_empty() { account.commodity.clone() } else { pl.currency.clone() },
            description: pl.description.clone(),
            reference: pl.reference.trim().to_string(),
            balance_after: pl.balance_after.map(|a| Money::from_major(a, &account.commodity, account.precision)).transpose()?,
            fingerprint: String::new(),
            posting_id: None,
            journal_entry_id: None,
            duplicate_of_id: None,
            status: if let Some(reason) = &pl.skip {
                let _ = reason;
                "skipped".into()
            } else {
                "unmatched".into()
            },
            note: pl.skip.clone().unwrap_or_default(),
        };
        if sl.status != "skipped" && !pl.currency.is_empty() && pl.currency != account.commodity {
            sl.status = "skipped".into();
            sl.note = format!("{} line in a {} account", pl.currency, account.commodity);
        }
        if let (Some(d), Some(a)) = (sl.date, sl.amount) {
            let key = (d, money::plain(a.major()), normalize_desc(&sl.description));
            let nth = seen.entry(key).or_insert(0);
            sl.fingerprint = fingerprint(account_id, d, a.major(), &sl.description, *nth);
            *nth += 1;
        }
        if let Some((oa, oc)) = &pl.original {
            sl.raw["_original"] = serde_json::json!({"quantity": money::plain(*oa), "commodity": oc});
        }
        lines.push(sl);
    }
    let prior_dates = coverage::dates(conn, account_id, 0)?;
    let period_from = parse.period_from.or_else(|| lines.iter().filter_map(|l| l.date).min());
    let period_to = parse.period_to.or_else(|| lines.iter().filter_map(|l| l.date).max());
    let mut import = Import {
        id: 0,
        uid: new_uid(),
        source: source.clone(),
        account_id: Some(account_id),
        filename: filename.to_string(),
        checksum: sum.clone(),
        status: if preview { "preview".into() } else { "done".into() },
        period_from,
        period_to,
        opening_balance: parse.opening_balance,
        closing_balance: parse.closing_balance,
        lines_count: lines.len() as i32,
        created_count: 0,
        matched_count: 0,
        duplicate_count: 0,
        skipped_count: lines.iter().filter(|l| l.status == "skipped").count() as i32 + parse.skipped_records as i32,
        unmatched_count: 0,
        error_count: 0,
        error: String::new(),
        options: serde_json::json!({"mapping": mapping, "closing_date": parse.closing_date.map(|d| d.to_string())}),
        created_at: now_ts(),
    };
    if preview {
        if let Some(warning) = coverage::warning(&prior_dates, &lines, true) {
            import.options["coverage_warning"] = serde_json::json!(warning);
            parse.warnings.push(warning);
        }
        return Ok(ImportOutcome { import, lines, parse });
    }
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO imports (uid, source, account_id, filename, checksum, status, period_from, period_to, opening_balance, closing_balance, lines_count, skipped_count, options, content, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'done', ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            import.uid,
            import.source,
            account_id,
            import.filename,
            import.checksum,
            import.period_from.map(|d| d.to_string()),
            import.period_to.map(|d| d.to_string()),
            import.opening_balance.map(money::plain),
            import.closing_balance.map(money::plain),
            import.lines_count,
            import.skipped_count,
            import.options.to_string(),
            content,
            import.created_at
        ],
    )?;
    import.id = tx.last_insert_rowid();
    for sl in lines.iter_mut() {
        sl.import_id = import.id;
        sl.id = insert_line(&tx, sl)?;
    }
    tx.commit()?;
    // Dedupe, match, draft: each line in its own transaction so one bad line does not lose the file.
    let rules = crate::rules::list_rules(conn, Some(account.entity_id))?;
    for sl in lines.iter_mut() {
        if sl.status == "skipped" {
            continue;
        }
        match process_line(conn, &account, sl, &rules, &import.source) {
            Ok(()) => {}
            Err(e) => {
                sl.status = "error".into();
                sl.note = e.to_string();
                conn.execute("UPDATE statement_lines SET status = 'error', note = ?2 WHERE id = ?1", params![sl.id, sl.note])?;
            }
        }
        match sl.status.as_str() {
            "created" => import.created_count += 1,
            "matched" => import.matched_count += 1,
            "duplicate" => import.duplicate_count += 1,
            "unmatched" => import.unmatched_count += 1,
            "error" => import.error_count += 1,
            _ => {}
        }
    }
    conn.execute(
        "UPDATE imports SET created_count = ?2, matched_count = ?3, duplicate_count = ?4, unmatched_count = ?5, error_count = ?6 WHERE id = ?1",
        params![import.id, import.created_count, import.matched_count, import.duplicate_count, import.unmatched_count, import.error_count],
    )?;
    if let Some(warning) = coverage::warning(&prior_dates, &lines, false) {
        import.options["coverage_warning"] = serde_json::json!(warning);
        conn.execute("UPDATE imports SET options=?2 WHERE id=?1", params![import.id, import.options.to_string()])?;
        parse.warnings.push(warning);
    }
    if source == enable_banking::SOURCE {
        enable_banking::learn(conn, &parse, account_id)?;
    } else if matches!(source.as_str(), bank_api::INTER | bank_api::WISE) {
        bank_api::learn(conn, &parse, account_id)?;
    } else if source == mercury::SOURCE {
        mercury::learn(conn, &parse, account_id)?;
    } else {
        inbox::learn_profile(conn, &source, filename, account_id)?;
    }
    crate::audit::log(conn, "imports", import.id, "create", None, Some(serde_json::json!({"source": source, "filename": filename, "lines": import.lines_count})))?;
    Ok(ImportOutcome { import: get_import(conn, import.id)?.0, lines, parse })
}

/// Retry only unfinished bank-import lines from their persisted evidence.
/// Completed lines are never replayed, even when an earlier pass stopped mid-file.
pub fn retry_import(conn: &mut Connection, id: i64) -> Result<Import> {
    let (mut import, mut lines) = get_import(conn, id)?;
    if import.source == "account_tracker" {
        return Err(Error::Invalid("Account Tracker is a migration; retry it through its migration importer".into()));
    }
    let account = accounts::get_account(conn, import.account_id.ok_or_else(|| Error::Invalid("place the import on an account before retrying".into()))?)?;
    let rules = crate::rules::list_rules(conn, Some(account.entity_id))?;
    let prior_dates = coverage::dates(conn, account.id, id)?;
    let skipped_before = lines.iter().filter(|l| l.status == "skipped").count() as i32;
    let mut attempted = 0;
    for sl in lines.iter_mut().filter(|l| matches!(l.status.as_str(), "unmatched" | "error") && l.journal_entry_id.is_none() && l.posting_id.is_none()) {
        attempted += 1;
        let result = if import.source == "remessa_csv" { remessa::retry_line(conn, &account, sl, &import.options) } else { process_line(conn, &account, sl, &rules, &import.source) };
        if let Err(e) = result {
            conn.execute("UPDATE statement_lines SET status = 'error', note = ?2 WHERE id = ?1", params![sl.id, e.to_string()])?;
        }
    }
    // Counts may have been stale after a crash, so derive them from persisted lines.
    let (_, lines) = get_import(conn, id)?;
    let count = |status: &str| lines.iter().filter(|l| l.status == status).count() as i32;
    conn.execute(
        "UPDATE imports SET created_count = ?2, matched_count = ?3, duplicate_count = ?4, unmatched_count = ?5, error_count = ?6, skipped_count = skipped_count + ?7 WHERE id = ?1",
        params![id, count("created"), count("matched"), count("duplicate"), count("unmatched"), count("error"), count("skipped") - skipped_before],
    )?;
    if let Some(warning) = coverage::warning(&prior_dates, &lines, false) {
        import.options["coverage_warning"] = serde_json::json!(warning);
        conn.execute("UPDATE imports SET options=?2 WHERE id=?1", params![id, import.options.to_string()])?;
    }
    crate::audit::log(conn, "imports", id, "retry", None, Some(serde_json::json!({"attempted": attempted})))?;
    Ok(get_import(conn, id)?.0)
}

/// Values already carry the account commodity and are written as exact minor units.
fn insert_line(conn: &Connection, sl: &StatementLine) -> Result<i64> {
    conn.execute(
        "INSERT INTO statement_lines (import_id, account_id, position, raw, date, amount, currency, description, reference, balance_after, fingerprint, status, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            sl.import_id,
            sl.account_id,
            sl.position,
            sl.raw.to_string(),
            sl.date.map(|d| d.to_string()),
            sl.amount.map(|a| a.minor()),
            sl.currency,
            sl.description,
            sl.reference,
            sl.balance_after.map(|a| a.minor()),
            sl.fingerprint,
            sl.status,
            sl.note
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Dedupe against earlier lines and postings, match a posting, or draft an entry.
fn process_line(conn: &mut Connection, account: &Account, sl: &mut StatementLine, rules: &[Rule], source: &str) -> Result<()> {
    let (Some(date), Some(amount)) = (sl.date, sl.amount) else {
        conn.execute("UPDATE statement_lines SET status = 'skipped', note = 'line has no date or amount' WHERE id = ?1", [sl.id])?;
        sl.status = "skipped".into();
        return Ok(());
    };
    // 1. Duplicate by reference.
    if !sl.reference.is_empty() {
        let earlier: Option<i64> = conn
            .query_row(
                "SELECT id FROM statement_lines WHERE account_id = ?1 AND reference = ?2 AND id <> ?3 AND status IN ('matched','created') ORDER BY id LIMIT 1",
                params![account.id, sl.reference, sl.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(e) = earlier {
            return mark_duplicate(conn, sl, e);
        }
        let posted: Option<i64> = conn
            .query_row(
                "SELECT p.id FROM postings p JOIN journal_entries e ON e.id = p.journal_entry_id
             WHERE p.account_id = ?1 AND p.external_id = ?2 AND e.status IN ('draft','posted') AND e.reverses_id IS NULL",
                params![account.id, sl.reference],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(pid) = posted {
            // The posting exists from another source (e.g. the Account Tracker migration): this line evidences it.
            return crate::matcher::attach(conn, sl, pid);
        }
    }
    // Enable Banking references identify movements across pulls. A new reference
    // must not collide with a fingerprint from a different movement or partial pull.
    // Other formats can change IDs between exports, so retain their fallback.
    if source != enable_banking::SOURCE || sl.reference.is_empty() {
        // 2. Duplicate by fingerprint.
        let earlier: Option<i64> = conn
            .query_row(
                "SELECT id FROM statement_lines WHERE account_id = ?1 AND fingerprint = ?2 AND id <> ?3 AND status IN ('matched','created') ORDER BY id LIMIT 1",
                params![account.id, sl.fingerprint, sl.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(e) = earlier {
            return mark_duplicate(conn, sl, e);
        }
    }
    // 3. Match an existing posting (a captured entry, a scheduled draft, a migrated row).
    if let Some(pid) = crate::matcher::best_match_for(conn, account.id, amount, date, MATCH_WINDOW_DAYS, &sl.description)? {
        return crate::matcher::attach(conn, sl, pid);
    }
    // A possible full refund waits for explicit confirmation instead of being
    // automatically categorized as income by a broad credit rule.
    if !crate::refunds::candidates(conn, sl.id, crate::refunds::DEFAULT_WINDOW_DAYS)?.is_empty() {
        return crate::rules::draft_from_line(conn, account, sl, &[]);
    }
    // 4. Draft through rules (or Suspense).
    crate::rules::draft_from_line(conn, account, sl, rules)
}

fn mark_duplicate(conn: &Connection, sl: &mut StatementLine, of: i64) -> Result<()> {
    sl.status = "duplicate".into();
    sl.duplicate_of_id = Some(of);
    conn.execute("UPDATE statement_lines SET status = 'duplicate', duplicate_of_id = ?2, note = '' WHERE id = ?1", params![sl.id, of])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Reading and rollback
// ---------------------------------------------------------------------------

const IMPORT_SELECT: &str = "SELECT id, uid, source, account_id, filename, checksum, status, period_from, period_to, opening_balance, closing_balance, lines_count, created_count, matched_count, duplicate_count, skipped_count, unmatched_count, error_count, error, options, created_at FROM imports";

fn row_to_import(r: &rusqlite::Row<'_>) -> rusqlite::Result<Import> {
    let pd = |s: Option<String>| s.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok());
    let pm = |s: Option<String>| s.and_then(|s| money::parse(&s).ok());
    let options: String = r.get(19)?;
    Ok(Import {
        id: r.get(0)?,
        uid: r.get(1)?,
        source: r.get(2)?,
        account_id: r.get(3)?,
        filename: r.get(4)?,
        checksum: r.get(5)?,
        status: r.get(6)?,
        period_from: pd(r.get(7)?),
        period_to: pd(r.get(8)?),
        opening_balance: pm(r.get(9)?),
        closing_balance: pm(r.get(10)?),
        lines_count: r.get(11)?,
        created_count: r.get(12)?,
        matched_count: r.get(13)?,
        duplicate_count: r.get(14)?,
        skipped_count: r.get(15)?,
        unmatched_count: r.get(16)?,
        error_count: r.get(17)?,
        error: r.get(18)?,
        options: serde_json::from_str(&options).unwrap_or(serde_json::json!({})),
        created_at: r.get(20)?,
    })
}

pub fn list_imports(conn: &Connection, account_id: Option<i64>, limit: i64) -> Result<Vec<Import>> {
    let sql = format!("{IMPORT_SELECT} WHERE (?1 IS NULL OR account_id = ?1) ORDER BY id DESC LIMIT ?2");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![account_id, if limit <= 0 { 100 } else { limit }], row_to_import)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn get_import(conn: &Connection, id: i64) -> Result<(Import, Vec<StatementLine>)> {
    let sql = format!("{IMPORT_SELECT} WHERE id = ?1");
    let import = conn.query_row(&sql, [id], row_to_import).optional()?.ok_or_else(|| Error::NotFound(format!("import {id}")))?;
    let lines = list_lines(conn, None, "", Some(id), 100_000)?;
    Ok((import, lines))
}

/// The commodity of a statement line is its account's; a line with no account names it in
/// `currency` (D14). Column 17 is that commodity's precision, NULL when neither resolves.
const LINE_SELECT: &str = "SELECT s.id, s.import_id, s.account_id, s.position, s.raw, s.date, s.amount, s.currency, s.description, s.reference,
        s.balance_after, s.fingerprint, s.posting_id, s.journal_entry_id, s.duplicate_of_id, s.status, s.note, COALESCE(ac.precision, cc.precision), COALESCE(ac.code, cc.code)
     FROM statement_lines s
     LEFT JOIN accounts a ON a.id = s.account_id
     LEFT JOIN commodities ac ON ac.id = a.commodity_id
     LEFT JOIN commodities cc ON cc.code = UPPER(TRIM(s.currency))";

/// What the two money columns of a statement line hold, before they have a commodity.
struct LineMinor {
    amount: Option<i64>,
    balance_after: Option<i64>,
    precision: Option<u32>,
    commodity: Option<String>,
}

fn row_to_line(r: &rusqlite::Row<'_>) -> rusqlite::Result<(StatementLine, LineMinor)> {
    let raw: String = r.get(4)?;
    let date: Option<String> = r.get(5)?;
    let line = StatementLine {
        id: r.get(0)?,
        import_id: r.get(1)?,
        account_id: r.get(2)?,
        position: r.get(3)?,
        raw: serde_json::from_str(&raw).unwrap_or(serde_json::json!({})),
        date: date.and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()),
        amount: None,
        currency: r.get(7)?,
        description: r.get(8)?,
        reference: r.get(9)?,
        balance_after: None,
        fingerprint: r.get(11)?,
        posting_id: r.get(12)?,
        journal_entry_id: r.get(13)?,
        duplicate_of_id: r.get(14)?,
        status: r.get(15)?,
        note: r.get(16)?,
    };
    let minor = LineMinor { amount: r.get(6)?, balance_after: r.get(10)?, precision: r.get::<_, Option<i64>>(17)?.map(|p| p as u32), commodity: r.get(18)? };
    Ok((line, minor))
}

fn line_with_money(row: (StatementLine, LineMinor)) -> Result<StatementLine> {
    let (mut line, minor) = row;
    if minor.amount.is_none() && minor.balance_after.is_none() {
        return Ok(line);
    }
    let precision = minor.precision.ok_or_else(|| Error::Invalid(format!("statement line {} has a value but no commodity: neither an account nor a known currency", line.id)))?;
    let commodity = minor.commodity.ok_or_else(|| Error::Invalid(format!("statement line {} has no commodity", line.id)))?;
    line.amount = minor.amount.map(|v| Money::from_minor(v, &commodity, precision)).transpose()?;
    line.balance_after = minor.balance_after.map(|v| Money::from_minor(v, &commodity, precision)).transpose()?;
    Ok(line)
}

pub fn get_line(conn: &Connection, id: i64) -> Result<StatementLine> {
    let sql = format!("{LINE_SELECT} WHERE s.id = ?1");
    let row = conn.query_row(&sql, [id], row_to_line).optional()?.ok_or_else(|| Error::NotFound(format!("statement line {id}")))?;
    line_with_money(row)
}

pub fn list_lines(conn: &Connection, account_id: Option<i64>, status: &str, import_id: Option<i64>, limit: i64) -> Result<Vec<StatementLine>> {
    let sql = format!(
        "{LINE_SELECT} WHERE (?1 IS NULL OR s.account_id = ?1) AND (?2 = '' OR s.status = ?2) AND (?3 IS NULL OR s.import_id = ?3)
         ORDER BY CASE WHEN ?3 IS NULL THEN s.date END DESC, s.import_id DESC, s.position LIMIT ?4"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![account_id, status, import_id, if limit <= 0 { 500 } else { limit }], row_to_line)?;
    rows.map(|r| line_with_money(r?)).collect()
}

pub fn count_lines(conn: &Connection, status: &str) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM statement_lines WHERE status = ?1", [status], |r| r.get(0))?)
}

/// Roll an import back: remove the entries it created and the lines it added, while nothing was edited or
/// reconciled by hand. With `force`, entries you edited after the import are removed too.
/// Neither mode can remove entries or reconciliation from a locked period.
pub fn delete_import(conn: &mut Connection, id: i64, force: bool) -> Result<()> {
    let (import, lines) = get_import(conn, id)?;
    let tx = conn.transaction()?;
    for l in lines.iter() {
        if let Some(eid) = l.journal_entry_id {
            if l.status == "created" {
                let entry = crate::journal::get_entry(&tx, eid)?;
                crate::journal::check_mutation_lock(&tx, &entry)?;
                let other_evidence: i64 = tx.query_row("SELECT COUNT(*) FROM statement_lines WHERE journal_entry_id = ?1 AND import_id <> ?2", params![eid, id], |r| r.get(0))?;
                if other_evidence > 0 {
                    return Err(Error::Locked(format!("entry #{eid} is also evidenced by another import")));
                }
                let edited: i64 = tx.query_row("SELECT COUNT(*) FROM audit_log WHERE table_name = 'journal_entries' AND row_id = ?1 AND action = 'update'", [eid], |r| r.get(0))?;
                if edited > 0 && entry.status == EntryStatus::Posted && !force {
                    return Err(Error::Locked(format!("entry #{eid} was edited after the import; roll back with force to discard those edits, or void it by hand")));
                }
                if let Some(seq) = entry.seq {
                    tx.execute("DELETE FROM journal_entries WHERE id = ?1", [eid])?;
                    crate::hashchain::rechain(&tx, entry.entity_id, seq)?;
                } else {
                    tx.execute("DELETE FROM journal_entries WHERE id = ?1", [eid])?;
                }
            }
        }
        if l.status == "matched" {
            if let Some(pid) = l.posting_id {
                let eid: i64 = tx.query_row("SELECT journal_entry_id FROM postings WHERE id = ?1", [pid], |r| r.get(0))?;
                crate::journal::check_mutation_lock(&tx, &crate::journal::get_entry(&tx, eid)?)?;
                tx.execute("UPDATE postings SET reconciled_at = NULL WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM statement_lines s JOIN imports i ON i.id=s.import_id WHERE s.posting_id=?1 AND s.import_id<>?2 AND i.source<>'account_tracker' AND s.status IN ('matched','created'))", params![pid, id])?;
            }
        }
    }
    tx.execute("DELETE FROM imports WHERE id = ?1", [id])?;
    crate::audit::log(&tx, "imports", id, "delete", Some(serde_json::to_value(&import)?), None)?;
    tx.commit()?;
    Ok(())
}

pub fn skip_line(conn: &Connection, line_id: i64, unskip: bool) -> Result<StatementLine> {
    let l = get_line(conn, line_id)?;
    if unskip {
        if l.status == "skipped" {
            conn.execute("UPDATE statement_lines SET status = 'unmatched', note = '' WHERE id = ?1", [line_id])?;
        }
    } else {
        if l.status == "matched" || l.status == "created" {
            return Err(Error::Invalid("this line already evidences an entry; void the entry first".into()));
        }
        conn.execute("UPDATE statement_lines SET status = 'skipped', note = 'skipped by hand' WHERE id = ?1", [line_id])?;
    }
    get_line(conn, line_id)
}

/// What a re-match pass did.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RematchReport {
    pub lines: usize,
    pub rematched: usize,
    pub category_kept: usize,
    pub untouched: usize,
    pub notes: Vec<String>,
}

/// Re-run matching for the lines an import created, against captured or migrated entries that were
/// not eligible at import time. A duplicate pair is merged: the earlier (captured) entry is kept, the
/// category you gave the import's entry is carried over, the import's entry is removed, and the bank
/// line becomes the kept entry's evidence.
pub fn rematch_import(conn: &mut Connection, import_id: i64) -> Result<RematchReport> {
    let tx = conn.transaction()?;
    let (import, lines) = get_import(&tx, import_id)?;
    if import.source == "account_tracker" {
        return Err(Error::Invalid("the Account Tracker migration is the capture itself; re-match a bank import".into()));
    }
    let mut report = RematchReport { lines: lines.len(), ..Default::default() };
    let index = accounts::path_index(&tx)?;
    for mut sl in lines.into_iter().filter(|l| l.status == "created" && l.journal_entry_id.is_some()) {
        let (Some(acc_id), Some(date), Some(amount)) = (sl.account_id, sl.date, sl.amount) else { continue };
        let own_entry_id = sl.journal_entry_id.unwrap();
        let cands: Vec<crate::matcher::Candidate> =
            crate::matcher::candidates_for(&tx, acc_id, amount, date, MATCH_WINDOW_DAYS, &sl.description)?.into_iter().filter(|c| c.journal_entry_id != own_entry_id).collect();
        let Some(target) = cands.first() else {
            report.untouched += 1;
            continue;
        };
        let own = crate::journal::get_entry(&tx, own_entry_id)?;
        let kept = crate::journal::get_entry(&tx, target.journal_entry_id)?;
        // Check both sides before category edits, deletion, or reconciliation. The kept
        // entry will be posted by attach, so its date must be open even if it is a draft.
        let lock_check = crate::journal::check_mutation_lock(&tx, &own).and_then(|()| crate::journal::check_lock(&crate::entities::get_entity(&tx, kept.entity_id)?, kept.date));
        if let Err(e) = lock_check {
            if !matches!(e, Error::Locked(_)) {
                return Err(e);
            }
            report.untouched += 1;
            report.notes.push(format!("line #{}: {e}", sl.id));
            continue;
        }
        // Carry the category over when both sides are a simple "bank + one category" entry.
        let own_contra = own.postings.iter().find(|p| p.account_id != acc_id);
        let kept_categories: Vec<&Posting> = kept.postings.iter().filter(|p| matches!(p.account_type, AccountType::Income | AccountType::Expense)).collect();
        let own_is_categorised =
            own.postings.iter().any(|p| matches!(p.account_type, AccountType::Income | AccountType::Expense) && index.get(&p.account_id).is_some_and(|a| a.system_role != "suspense"));
        tx.execute_batch("SAVEPOINT rematch_line")?;
        let result = (|| -> Result<bool> {
            let other_evidence: i64 = tx.query_row("SELECT COUNT(*) FROM statement_lines WHERE journal_entry_id=?1 AND id<>?2", params![own.id, sl.id], |r| r.get(0))?;
            if other_evidence != 0 {
                return Err(Error::Conflict(format!("entry #{} has other statement evidence; rematch cannot remove it", own.id)));
            }
            if own_is_categorised && (own.postings.len() != 2 || kept_categories.len() != 1 || kept.postings.len() != 2) {
                return Err(Error::Conflict("categorized split entries need manual review before rematch".into()));
            }
            let carry_category = own_is_categorised;
            let mut posting_id = target.posting_id;
            if carry_category {
                let contra = own_contra.unwrap();
                let mut input = EntryInput::new(kept.entity_id, kept.date);
                input.payee = if kept.payee.trim().is_empty() { own.payee.clone() } else { kept.payee.clone() };
                input.description = kept.description.clone();
                input.notes = kept.notes.clone();
                input.status = EntryStatus::Posted;
                input.origin = kept.origin.clone();
                for p in &kept.postings {
                    if matches!(p.account_type, AccountType::Income | AccountType::Expense) {
                        input.postings.push(PostingInput::balancing(contra.account_id).memo(&p.memo));
                    } else {
                        input.postings.push(PostingInput {
                            account_id: p.account_id,
                            quantity: p.quantity.major(),
                            amount: Some(p.amount.major()),
                            memo: p.memo.clone(),
                            metadata: p.metadata.clone(),
                            external_id: p.external_id.clone(),
                            fingerprint: p.fingerprint.clone(),
                            ..Default::default()
                        });
                    }
                }

                let updated = crate::journal::update_entry_in_transaction(&tx, kept.id, input)?;
                // Posting IDs can change during a category edit. Resolve the bank
                // leg from the updated entry before removing the source.
                posting_id = updated
                    .postings
                    .iter()
                    .find(|p| p.account_id == acc_id && p.quantity == amount)
                    .map(|p| p.id)
                    .ok_or_else(|| Error::Conflict("rematch category edit lost the bank posting".into()))?;
            }
            tx.execute("UPDATE statement_lines SET status = 'unmatched', posting_id = NULL, journal_entry_id = NULL WHERE id = ?1", [sl.id])?;
            tx.execute("DELETE FROM journal_entries WHERE id = ?1", [own.id])?;
            if let Some(seq) = own.seq {
                crate::hashchain::rechain(&tx, own.entity_id, seq)?;
            }
            crate::audit::log(&tx, "journal_entries", own.id, "delete", Some(serde_json::to_value(&own)?), Some(serde_json::json!({"duplicate_of": kept.id, "line": sl.id})))?;
            sl.status = "unmatched".into();
            sl.journal_entry_id = None;
            sl.posting_id = None;
            crate::matcher::attach_in_transaction(&tx, &mut sl, posting_id)?;
            tx.execute("UPDATE imports SET created_count = created_count - 1, matched_count = matched_count + 1 WHERE id = ?1", [import_id])?;
            Ok(carry_category)
        })();
        match result {
            Ok(carried) => {
                tx.execute_batch("RELEASE rematch_line")?;
                report.rematched += 1;
                report.category_kept += usize::from(carried);
            }
            Err(e) => {
                tx.execute_batch("ROLLBACK TO rematch_line; RELEASE rematch_line")?;
                report.untouched += 1;
                report.notes.push(format!("line {}: {e}", sl.id));
            }
        }
    }
    crate::audit::log(&tx, "imports", import_id, "rematch", None, Some(serde_json::to_value(&report)?))?;
    tx.commit()?;
    Ok(report)
}
