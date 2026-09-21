//! The gRPC service: the proto contract mapped onto means-core.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::NaiveDate;
use means_core::model::*;
use means_core::{accounts, entities, hashchain, imports, journal, matcher, money, rates, reports, rules, templates, Db};
use means_proto::v1 as pb;
use means_proto::v1::means_server::Means;
use rust_decimal::Decimal;
use tonic::{Request, Response, Status};

#[derive(Clone)]
pub struct MeansService {
    db: Arc<Db>,
    connections: Option<Arc<crate::connections::Manager>>,
    replica: Option<means_sharing::store::Replica>,
    viewers: Arc<tokio::sync::Mutex<HashMap<String, crate::sharing::Viewer>>>,
}

impl MeansService {
    pub fn new(db: Arc<Db>, inbox: std::path::PathBuf) -> anyhow::Result<Self> {
        let connections = crate::connections::Manager::new(db.clone(), inbox)?;
        Ok(MeansService { db, connections: Some(connections), replica: None, viewers: Default::default() })
    }

    pub fn replica(db: Arc<Db>, replica: means_sharing::store::Replica) -> Self {
        Self { db, connections: None, replica: Some(replica), viewers: Default::default() }
    }
    fn writable(&self) -> Result<(), Status> {
        if self.db.is_replica() {
            Err(Status::permission_denied("received vault is read-only"))
        } else {
            Ok(())
        }
    }

    async fn run<T, F>(&self, f: F) -> Result<T, Status>
    where
        T: Send + 'static,
        F: FnOnce(&mut means_core::rusqlite::Connection) -> means_core::Result<T> + Send + 'static,
    {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = db.conn();
            f(&mut guard)
        })
        .await
        .map_err(|e| Status::internal(format!("task failed: {e}")))?
        .map_err(status)
    }
}

fn status(e: means_core::Error) -> Status {
    use means_core::Error as E;
    match e {
        E::NotFound(m) => Status::not_found(m),
        E::Invalid(m) | E::Parse(m) => Status::invalid_argument(m),
        E::Locked(m) | E::Unbalanced(m) => Status::failed_precondition(m),
        E::Conflict(m) => Status::already_exists(m),
        E::Db(e) => Status::internal(format!("database: {e}")),
        E::Json(e) => Status::internal(format!("json: {e}")),
        E::Other(e) => Status::internal(e.to_string()),
    }
}

fn dec(s: &str) -> Result<Decimal, Status> {
    money::parse(s).map_err(status)
}
fn dec_opt(s: &str) -> Result<Option<Decimal>, Status> {
    money::parse_opt(s).map_err(status)
}
fn date(s: &str) -> Result<NaiveDate, Status> {
    means_core::parse_date(s).map_err(status)
}
fn date_opt(s: &str) -> Result<Option<NaiveDate>, Status> {
    means_core::parse_opt_date(s).map_err(status)
}
fn nz(i: i64) -> Option<i64> {
    if i > 0 {
        Some(i)
    } else {
        None
    }
}
fn ostr(o: &Option<String>) -> String {
    o.clone().unwrap_or_default()
}
fn odate(o: Option<NaiveDate>) -> String {
    o.map(|d| d.to_string()).unwrap_or_default()
}
fn plain(d: Decimal) -> String {
    money::plain(d)
}
fn json_opt(s: &str) -> Result<serde_json::Value, Status> {
    if s.trim().is_empty() {
        Ok(serde_json::json!({}))
    } else {
        serde_json::from_str(s).map_err(|e| Status::invalid_argument(format!("metadata is not JSON: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

fn entity_pb(e: &Entity) -> pb::Entity {
    pb::Entity { id: e.id, name: e.name.clone(), kind: e.kind.clone(), country: e.country.clone(), currency: e.currency.clone(), lock_date: odate(e.lock_date), archived: e.archived_at.is_some() }
}

fn commodity_pb(c: &Commodity) -> pb::Commodity {
    pb::Commodity { id: c.id, code: c.code.clone(), kind: c.kind.clone(), name: c.name.clone(), precision: c.precision as i32, isin: c.isin.clone() }
}

fn price_pb(p: &Price) -> pb::Price {
    pb::Price { id: p.id, commodity: p.commodity.clone(), currency: p.currency.clone(), on: p.on.to_string(), price: plain(p.price), source: p.source.clone() }
}

fn account_pb(a: &Account) -> pb::Account {
    pb::Account {
        id: a.id,
        entity_id: a.entity_id,
        parent_id: a.parent_id.unwrap_or(0),
        code: a.code.clone(),
        name: a.name.clone(),
        path: a.path.clone(),
        r#type: a.r#type.as_str().to_string(),
        subtype: a.subtype.clone(),
        commodity: a.commodity.clone(),
        system_role: a.system_role.clone(),
        placeholder: a.placeholder,
        in_net_worth: a.in_net_worth,
        closed_at: ostr(&a.closed_at),
        position: a.position,
        depth: a.depth,
        credit_limit: a.credit_limit.map(plain).unwrap_or_default(),
        statement_day: a.statement_day.unwrap_or(0),
        due_day: a.due_day.unwrap_or(0),
        external_ids: a.external_ids.to_string(),
        balance: money::fmt(a.balance, a.precision),
        balance_functional: plain(a.balance_functional),
        postings_count: a.postings_count as i32,
        last_reconciled_at: ostr(&a.last_reconciled_at),
        notes: a.notes.clone(),
        class: a.class.clone(),
    }
}

fn simple_from_pb(e: &pb::SimpleEntryInput) -> Result<journal::SimpleEntry, Status> {
    let legs = e.splits.iter().map(|s| Ok(means_core::splits::Leg { account_id: s.account_id, quantity: dec_opt(&s.quantity)?, memo: s.memo.clone() })).collect::<Result<Vec<_>, Status>>()?;
    let splits = if legs.is_empty() { vec![] } else { means_core::splits::resolve(dec(&e.quantity)?, &legs).map_err(status)? };
    Ok(journal::SimpleEntry {
        entity_id: e.entity_id,
        date: date(&e.date)?,
        kind: e.kind.clone(),
        account_id: e.account_id,
        contra_account_id: nz(e.contra_account_id),
        quantity: dec(&e.quantity)?,
        contra_quantity: dec_opt(&e.contra_quantity)?,
        payee: e.payee.clone(),
        notes: e.notes.clone(),
        splits,
        status: EntryStatus::parse(&e.status).map_err(status)?,
        fee: dec_opt(&e.fee)?,
        fee_account_id: nz(e.fee_account_id),
        origin: "capture".into(),
    })
}

fn posting_pb(p: &Posting) -> pb::Posting {
    pb::Posting {
        id: p.id,
        journal_entry_id: p.journal_entry_id,
        account_id: p.account_id,
        account_path: p.account_path.clone(),
        commodity: p.quantity.commodity().to_owned(),
        quantity: plain(p.quantity.major()),
        amount: plain(p.amount.major()),
        rate: p.rate.map(plain).unwrap_or_default(),
        memo: p.memo.clone(),
        external_id: ostr(&p.external_id),
        fingerprint: ostr(&p.fingerprint),
        reconciled_at: ostr(&p.reconciled_at),
        metadata: if p.metadata.as_object().map(|m| m.is_empty()).unwrap_or(true) { String::new() } else { p.metadata.to_string() },
        position: p.position,
    }
}

fn entry_pb(e: &JournalEntry) -> pb::JournalEntry {
    pb::JournalEntry {
        id: e.id,
        entity_id: e.entity_id,
        date: e.date.to_string(),
        payee: e.payee.clone(),
        payee_id: e.payee_id.unwrap_or(0),
        display_payee: e.display_payee.clone(),
        description: e.description.clone(),
        notes: e.notes.clone(),
        status: e.status.as_str().to_string(),
        reverses_id: e.reverses_id.unwrap_or(0),
        reversed_by_id: e.reversed_by_id.unwrap_or(0),
        refund_of_id: e.refund_of_id.unwrap_or(0),
        counterpart_id: e.counterpart_id.unwrap_or(0),
        template_id: e.template_id.unwrap_or(0),
        origin: e.origin.clone(),
        posted_at: ostr(&e.posted_at),
        created_at: e.created_at.clone(),
        postings: e.postings.iter().map(posting_pb).collect(),
        kind: e.kind.clone(),
        amount_functional: plain(e.amount_functional),
        statement_line_id: e.statement_line_id.unwrap_or(0),
        tags: e.tags.clone(),
        reviewed_at: ostr(&e.reviewed_at),
    }
}

fn template_pb(t: &EntryTemplate) -> pb::EntryTemplate {
    pb::EntryTemplate {
        id: t.id,
        entity_id: t.entity_id,
        name: t.name.clone(),
        payee: t.payee.clone(),
        description: t.description.clone(),
        lines: t
            .lines
            .iter()
            .map(|l| pb::TemplateLine {
                account_id: l.account_id,
                account_path: String::new(),
                method: l.method.clone(),
                value: l.value.map(plain).unwrap_or_default(),
                of_line: l.of_line.unwrap_or(0) as i32,
                memo: l.memo.clone(),
                label: l.label.clone(),
            })
            .collect(),
        rrule: t.rrule.clone(),
        starts_on: odate(t.starts_on),
        next_on: odate(t.next_on),
        ends_on: odate(t.ends_on),
        auto_post: t.auto_post,
        lead_days: t.lead_days,
        version: t.version as i32,
        active: t.active,
        created_at: t.created_at.clone(),
    }
}

fn template_from_pb(t: &pb::EntryTemplate) -> Result<EntryTemplate, Status> {
    let mut lines = Vec::new();
    for l in &t.lines {
        lines.push(TemplateLine {
            account_id: l.account_id,
            method: l.method.clone(),
            value: dec_opt(&l.value)?,
            of_line: if l.of_line > 0 { Some(l.of_line as usize) } else { None },
            memo: l.memo.clone(),
            label: l.label.clone(),
        });
    }
    Ok(EntryTemplate {
        id: t.id,
        uid: String::new(),
        entity_id: t.entity_id,
        name: t.name.clone(),
        payee: t.payee.clone(),
        description: t.description.clone(),
        lines,
        rrule: t.rrule.clone(),
        starts_on: date_opt(&t.starts_on)?,
        next_on: date_opt(&t.next_on)?,
        ends_on: date_opt(&t.ends_on)?,
        auto_post: t.auto_post,
        lead_days: t.lead_days,
        version: t.version as i64,
        active: t.active || t.id == 0,
        created_at: String::new(),
    })
}

fn import_pb(i: &Import) -> pb::Import {
    pb::Import {
        id: i.id,
        source: i.source.clone(),
        account_id: i.account_id.unwrap_or(0),
        filename: i.filename.clone(),
        checksum: i.checksum.clone(),
        status: i.status.clone(),
        period_from: odate(i.period_from),
        period_to: odate(i.period_to),
        opening_balance: i.opening_balance.map(plain).unwrap_or_default(),
        closing_balance: i.closing_balance.map(plain).unwrap_or_default(),
        lines_count: i.lines_count,
        created_count: i.created_count,
        matched_count: i.matched_count,
        duplicate_count: i.duplicate_count,
        skipped_count: i.skipped_count,
        unmatched_count: i.unmatched_count,
        error_count: i.error_count,
        error: i.error.clone(),
        created_at: i.created_at.clone(),
        options: i.options.to_string(),
    }
}

fn line_pb(l: &StatementLine) -> pb::StatementLine {
    pb::StatementLine {
        id: l.id,
        import_id: l.import_id,
        position: l.position,
        date: odate(l.date),
        amount: l.amount.map(|a| plain(a.major())).unwrap_or_default(),
        currency: l.currency.clone(),
        description: l.description.clone(),
        reference: l.reference.clone(),
        balance_after: l.balance_after.map(|a| plain(a.major())).unwrap_or_default(),
        fingerprint: l.fingerprint.clone(),
        posting_id: l.posting_id.unwrap_or(0),
        journal_entry_id: l.journal_entry_id.unwrap_or(0),
        duplicate_of_id: l.duplicate_of_id.unwrap_or(0),
        status: l.status.clone(),
        note: l.note.clone(),
        raw: l.raw.to_string(),
        account_id: l.account_id.unwrap_or(0),
    }
}

fn rule_pb(r: &Rule) -> pb::Rule {
    pb::Rule {
        id: r.id,
        entity_id: r.entity_id,
        name: r.name.clone(),
        position: r.position,
        enabled: r.enabled,
        conditions: r.conditions.iter().map(|c| pb::RuleCondition { field: c.field.clone(), op: c.op.clone(), value: c.value.clone() }).collect(),
        account_id: r.account_id.unwrap_or(0),
        template_id: r.template_id.unwrap_or(0),
        payee: r.payee.clone(),
        hits_count: r.hits_count,
        created_at: r.created_at.clone(),
        tags: r.tags.clone(),
    }
}

fn rule_from_pb(r: &pb::Rule) -> Rule {
    Rule {
        id: r.id,
        entity_id: r.entity_id,
        name: r.name.clone(),
        position: r.position,
        enabled: r.enabled || r.id == 0,
        conditions: r.conditions.iter().map(|c| RuleCondition { field: c.field.clone(), op: c.op.clone(), value: c.value.clone() }).collect(),
        account_id: nz(r.account_id),
        template_id: nz(r.template_id),
        payee: r.payee.clone(),
        hits_count: r.hits_count,
        created_at: String::new(),
        tags: r.tags.clone(),
    }
}

fn report_row_pb(r: &ReportRow) -> pb::ReportRow {
    pb::ReportRow {
        account_id: r.account_id,
        path: r.path.clone(),
        name: r.name.clone(),
        r#type: r.r#type.as_str().to_string(),
        depth: r.depth,
        placeholder: r.placeholder,
        commodity: r.quantity.commodity().to_owned(),
        quantity: money::fmt(r.quantity.major(), r.quantity.precision()),
        amount: plain(r.amount.major()),
        debit: plain(r.debit.major()),
        credit: plain(r.credit.major()),
        market_value: r.market_value.map(|m| plain(m.major())).unwrap_or_default(),
    }
}

fn report_pb(r: reports::Report) -> pb::ReportResponse {
    pb::ReportResponse {
        rows: r.rows.iter().map(report_row_pb).collect(),
        total_debit: plain(r.total_debit.major()),
        total_credit: plain(r.total_credit.major()),
        net: plain(r.net.major()),
        currency: r.currency,
        summary: r.summary.iter().map(report_row_pb).collect(),
    }
}

fn ledger_row_pb(r: &LedgerRow) -> pb::LedgerRow {
    pb::LedgerRow {
        posting_id: r.posting_id,
        journal_entry_id: r.journal_entry_id,
        date: r.date.to_string(),
        payee: r.payee.clone(),
        display_payee: r.display_payee.clone(),
        description: r.description.clone(),
        status: r.status.as_str().to_string(),
        contra_path: r.contra_path.clone(),
        debit: plain(r.debit.major()),
        credit: plain(r.credit.major()),
        running_balance: plain(r.running_balance.major()),
        amount: plain(r.amount.major()),
        reconciled: r.reconciled,
        statement_line_id: r.statement_line_id.unwrap_or(0),
    }
}

fn posting_input_from_pb(p: &pb::PostingInput) -> Result<PostingInput, Status> {
    Ok(PostingInput {
        account_id: p.account_id,
        quantity: if p.quantity.trim().is_empty() { Decimal::ZERO } else { dec(&p.quantity)? },
        amount: dec_opt(&p.amount)?,
        memo: p.memo.clone(),
        metadata: json_opt(&p.metadata)?,
        external_id: (!p.external_id.is_empty()).then(|| p.external_id.clone()),
        fingerprint: (!p.fingerprint.is_empty()).then(|| p.fingerprint.clone()),
        balance: p.quantity.trim().is_empty() || p.quantity.trim() == "balance",
        value_in: None,
    })
}

fn entry_input_from_pb(e: &pb::JournalEntryInput) -> Result<EntryInput, Status> {
    let mut input = EntryInput::new(e.entity_id, date(&e.date)?);
    input.payee = e.payee.clone();
    input.description = e.description.clone();
    input.notes = e.notes.clone();
    input.status = EntryStatus::parse(&e.status).map_err(status)?;
    input.template_id = nz(e.template_id);
    input.origin = if e.origin.is_empty() { "capture".into() } else { e.origin.clone() };
    for p in &e.postings {
        input.postings.push(posting_input_from_pb(p)?);
    }
    Ok(input)
}

#[derive(serde::Serialize)]
pub struct StatusInfo {
    pub version: String,
    pub db_path: String,
    pub entities: i64,
    pub accounts: i64,
    pub journal_entries: i64,
    pub drafts: i64,
    pub unmatched_lines: i64,
    pub pending_imports: i64,
    pub unreviewed: i64,
    pub prices: i64,
    pub postings_without_rate: i64,
    pub onboarded: bool,
    pub latest_price_date: String,
    pub today: String,
}

pub fn status_of(db: &Db) -> means_core::Result<StatusInfo> {
    let conn = db.conn();
    let entities: i64 = conn.query_row("SELECT COUNT(*) FROM entities WHERE archived_at IS NULL", [], |r| r.get(0))?;
    let accounts: i64 = conn.query_row("SELECT COUNT(*) FROM accounts WHERE placeholder = 0 AND system_role = '' AND closed_at IS NULL", [], |r| r.get(0))?;
    let journal_entries: i64 = conn.query_row("SELECT COUNT(*) FROM journal_entries WHERE status <> 'void'", [], |r| r.get(0))?;
    let drafts = journal::count_by_status(&conn, "draft")?;
    let unmatched_lines = imports::count_lines(&conn, "unmatched")?;
    let pending_imports: i64 = conn.query_row("SELECT COUNT(*) FROM imports WHERE status = 'pending'", [], |r| r.get(0))?;
    let unreviewed: i64 = conn.query_row("SELECT COUNT(*) FROM journal_entries WHERE reviewed_at IS NULL AND status = 'posted'", [], |r| r.get(0))?;
    let (prices, latest) = rates::price_count(&conn)?;
    let postings_without_rate: i64 = conn.query_row("SELECT COUNT(*) FROM postings WHERE rate_source = 'missing'", [], |r| r.get(0))?;
    Ok(StatusInfo {
        version: crate::VERSION.into(),
        db_path: db.path().display().to_string(),
        entities,
        accounts,
        journal_entries,
        drafts,
        unmatched_lines,
        pending_imports,
        unreviewed,
        prices,
        postings_without_rate,
        onboarded: entities > 0 && accounts > 0,
        latest_price_date: latest.unwrap_or_default(),
        today: means_core::today().to_string(),
    })
}

fn account_with_balance(conn: &means_core::rusqlite::Connection, id: i64) -> means_core::Result<Account> {
    let a = accounts::get_account(conn, id)?;
    let all = accounts::list_accounts_with_balances(conn, Some(a.entity_id), true, None)?;
    Ok(all.into_iter().find(|x| x.id == id).unwrap_or(a))
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

#[tonic::async_trait]
impl Means for MeansService {
    async fn sharing(&self, req: Request<pb::SharingRequest>) -> Result<Response<pb::SharingResponse>, Status> {
        self.writable()?;
        let request = req.into_inner();
        let db_path = self.db.path().to_path_buf();
        let root = crate::sharing::root(&db_path);
        if request.operation == "close-view" {
            if request.arguments.len() != 1 {
                return Err(Status::invalid_argument("close-view requires viewer URL"));
            }
            self.viewers.lock().await.remove(&request.arguments[0]);
            return Ok(Response::new(pb::SharingResponse::default()));
        }
        if request.operation == "view" {
            if request.arguments.len() != 1 {
                return Err(Status::invalid_argument("view requires a grant ID"));
            }
            let grant = request.arguments[0].clone();
            let (db, replica) = tokio::task::spawn_blocking(move || means_sharing::store::Store::open(&root)?.open_replica(&grant))
                .await
                .map_err(|e| Status::internal(e.to_string()))?
                .map_err(|e| Status::failed_precondition(e.to_string()))?;
            let mut viewers = self.viewers.lock().await;
            viewers.retain(|_, v| !v.finished());
            if viewers.len() >= 8 {
                return Err(Status::resource_exhausted("close an existing received-vault viewer before opening another"));
            }
            let (url, task) = crate::sharing::viewer(db, replica).await.map_err(|e| Status::internal(e.to_string()))?;
            viewers.insert(url.clone(), crate::sharing::Viewer::new(task));
            return Ok(Response::new(pb::SharingResponse { replica_server: url, ..Default::default() }));
        }
        let response = tokio::task::spawn_blocking(move || crate::sharing::execute(&db_path, &root, request))
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .map_err(|e| Status::failed_precondition(e.to_string()))?;
        Ok(Response::new(response))
    }

    async fn get_status(&self, _: Request<pb::GetStatusRequest>) -> Result<Response<pb::GetStatusResponse>, Status> {
        let s = status_of(&self.db).map_err(status)?;
        Ok(Response::new(pb::GetStatusResponse {
            replica_status: self.replica.as_ref().map(|r| r.status().map(str::to_string)).transpose().map_err(|e| Status::internal(e.to_string()))?.unwrap_or_default(),
            version: s.version,
            db_path: s.db_path,
            entities: s.entities as i32,
            accounts: s.accounts as i32,
            journal_entries: s.journal_entries as i32,
            drafts: s.drafts as i32,
            unmatched_lines: s.unmatched_lines as i32,
            pending_imports: s.pending_imports as i32,
            unreviewed: s.unreviewed as i32,
            prices: s.prices as i32,
            postings_without_rate: s.postings_without_rate as i32,
            onboarded: s.onboarded,
            latest_price_date: s.latest_price_date,
            today: s.today,
        }))
    }

    async fn list_entities(&self, req: Request<pb::ListEntitiesRequest>) -> Result<Response<pb::ListEntitiesResponse>, Status> {
        let include = req.into_inner().include_archived;
        let list = self.run(move |c| entities::list_entities(c, include)).await?;
        Ok(Response::new(pb::ListEntitiesResponse { entities: list.iter().map(entity_pb).collect() }))
    }

    async fn create_entity(&self, req: Request<pb::CreateEntityRequest>) -> Result<Response<pb::EntityResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let e = self.run(move |c| entities::create_entity(c, &r.name, &r.kind, &r.country, &r.currency)).await?;
        Ok(Response::new(pb::EntityResponse { entity: Some(entity_pb(&e)) }))
    }

    async fn update_entity(&self, req: Request<pb::UpdateEntityRequest>) -> Result<Response<pb::EntityResponse>, Status> {
        self.writable()?;
        let e = req.into_inner().entity.ok_or_else(|| Status::invalid_argument("entity is required"))?;
        let lock = date_opt(&e.lock_date)?;
        let out = self.run(move |c| entities::update_entity(c, e.id, &e.name, &e.kind, &e.country, lock, e.archived)).await?;
        Ok(Response::new(pb::EntityResponse { entity: Some(entity_pb(&out)) }))
    }

    async fn list_commodities(&self, _: Request<pb::ListCommoditiesRequest>) -> Result<Response<pb::ListCommoditiesResponse>, Status> {
        let list = self.run(|c| entities::list_commodities(c)).await?;
        Ok(Response::new(pb::ListCommoditiesResponse { commodities: list.iter().map(commodity_pb).collect() }))
    }

    async fn create_commodity(&self, req: Request<pb::CreateCommodityRequest>) -> Result<Response<pb::CommodityResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let c = self.run(move |c| entities::create_commodity(c, &r.code, &r.kind, &r.name, if r.precision > 0 { Some(r.precision as u32) } else { None }, &r.isin)).await?;
        Ok(Response::new(pb::CommodityResponse { commodity: Some(commodity_pb(&c)) }))
    }

    async fn list_prices(&self, req: Request<pb::ListPricesRequest>) -> Result<Response<pb::ListPricesResponse>, Status> {
        let r = req.into_inner();
        let from = date_opt(&r.from)?;
        let to = date_opt(&r.to)?;
        let list = self.run(move |c| rates::list_prices(c, &r.commodity, &r.currency, from, to, r.limit as i64)).await?;
        Ok(Response::new(pb::ListPricesResponse { prices: list.iter().map(price_pb).collect() }))
    }

    async fn set_price(&self, req: Request<pb::SetPriceRequest>) -> Result<Response<pb::Empty>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let on = date(&r.on)?;
        let price = dec(&r.price)?;
        self.run(move |c| {
            rates::set_price(c, &r.commodity, &r.currency, on, price, if r.source.is_empty() { "manual" } else { r.source.as_str() })?;
            let _ = rates::revalue_missing(c)?;
            Ok(())
        })
        .await?;
        Ok(Response::new(pb::Empty {}))
    }

    async fn fetch_rates(&self, req: Request<pb::FetchRatesRequest>) -> Result<Response<pb::FetchRatesResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let from = if r.from.is_empty() { Some(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()) } else { date_opt(&r.from)? };
        let (inserted, revalued, latest) = crate::fetch_rates(&self.db, from).await.map_err(|e| Status::unavailable(format!("ECB rates: {e}")))?;
        Ok(Response::new(pb::FetchRatesResponse { inserted: inserted as i32, revalued: revalued as i32, latest }))
    }

    async fn list_accounts(&self, req: Request<pb::ListAccountsRequest>) -> Result<Response<pb::ListAccountsResponse>, Status> {
        let r = req.into_inner();
        let as_of = date_opt(&r.as_of)?;
        let list = self.run(move |c| accounts::list_accounts_with_balances(c, nz(r.entity_id), r.include_closed, as_of)).await?;
        let filter = r.r#type.trim().to_ascii_lowercase();
        let out: Vec<pb::Account> = list.iter().filter(|a| filter.is_empty() || a.r#type.as_str() == filter).map(account_pb).collect();
        Ok(Response::new(pb::ListAccountsResponse { accounts: out }))
    }

    async fn get_account(&self, req: Request<pb::GetAccountRequest>) -> Result<Response<pb::AccountResponse>, Status> {
        let id = req.into_inner().id;
        let a = self.run(move |c| account_with_balance(c, id)).await?;
        Ok(Response::new(pb::AccountResponse { account: Some(account_pb(&a)) }))
    }

    async fn create_account(&self, req: Request<pb::CreateAccountRequest>) -> Result<Response<pb::AccountResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let t = if r.r#type.trim().is_empty() { None } else { Some(AccountType::parse(&r.r#type).map_err(status)?) };
        let opening = dec_opt(&r.opening_balance)?;
        let opening_date = date_opt(&r.opening_date)?;
        let credit_limit = dec_opt(&r.credit_limit)?;
        let a = self
            .run(move |c| {
                let acc = accounts::create_account(
                    c,
                    NewAccount {
                        entity_id: r.entity_id,
                        parent_id: nz(r.parent_id),
                        code: r.code.clone(),
                        name: r.name.clone(),
                        r#type: t,
                        subtype: r.subtype.clone(),
                        commodity: r.commodity.clone(),
                        placeholder: r.placeholder,
                        in_net_worth: r.in_net_worth || !r.placeholder,
                        credit_limit,
                        statement_day: if r.statement_day > 0 { Some(r.statement_day) } else { None },
                        due_day: if r.due_day > 0 { Some(r.due_day) } else { None },
                        external_ids: None,
                        notes: r.notes.clone(),
                        ..Default::default()
                    },
                )?;
                if let Some(ob) = opening.filter(|d| !d.is_zero()) {
                    let equity = accounts::find_by_role(c, acc.entity_id, "opening_balance")?;
                    let mut input = EntryInput::new(acc.entity_id, opening_date.unwrap_or_else(means_core::today));
                    input.payee = "Opening balance".into();
                    input.origin = "system".into();
                    // Assets open with a debit; liabilities with a credit (what you owe).
                    let q = if acc.r#type == AccountType::Liability { -ob } else { ob };
                    input.postings.push(PostingInput::new(acc.id, q));
                    input.postings.push(PostingInput::balancing(equity.id));
                    journal::create_entry(c, input)?;
                }
                account_with_balance(c, acc.id)
            })
            .await?;
        Ok(Response::new(pb::AccountResponse { account: Some(account_pb(&a)) }))
    }

    async fn update_account(&self, req: Request<pb::UpdateAccountRequest>) -> Result<Response<pb::AccountResponse>, Status> {
        self.writable()?;
        let a = req.into_inner().account.ok_or_else(|| Status::invalid_argument("account is required"))?;
        let credit_limit = dec_opt(&a.credit_limit)?;
        let external_ids = if a.external_ids.trim().is_empty() { None } else { Some(json_opt(&a.external_ids)?) };
        let out = self
            .run(move |c| {
                accounts::update_account(
                    c,
                    a.id,
                    accounts::AccountUpdate {
                        parent_id: Some(nz(a.parent_id)),
                        code: Some(a.code.clone()),
                        name: Some(a.name.clone()),
                        subtype: if a.subtype.is_empty() { None } else { Some(a.subtype.clone()) },
                        in_net_worth: Some(a.in_net_worth),
                        credit_limit: Some(credit_limit),
                        statement_day: Some(if a.statement_day > 0 { Some(a.statement_day) } else { None }),
                        due_day: Some(if a.due_day > 0 { Some(a.due_day) } else { None }),
                        notes: Some(a.notes.clone()),
                        position: Some(a.position),
                        external_ids,
                        placeholder: Some(a.placeholder),
                        commodity: if a.commodity.is_empty() { None } else { Some(a.commodity.clone()) },
                        class: Some(a.class.clone()),
                    },
                )?;
                account_with_balance(c, a.id)
            })
            .await?;
        Ok(Response::new(pb::AccountResponse { account: Some(account_pb(&out)) }))
    }

    async fn close_account(&self, req: Request<pb::CloseAccountRequest>) -> Result<Response<pb::AccountResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let a = self
            .run(move |c| {
                accounts::close_account(c, r.id, r.reopen)?;
                account_with_balance(c, r.id)
            })
            .await?;
        Ok(Response::new(pb::AccountResponse { account: Some(account_pb(&a)) }))
    }

    async fn merge_accounts(&self, req: Request<pb::MergeAccountsRequest>) -> Result<Response<pb::MergeAccountsResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let m = self.run(move |c| accounts::merge_accounts(c, r.source_id, r.target_id)).await?;
        Ok(Response::new(pb::MergeAccountsResponse {
            moved_postings: m.moved_postings as i32,
            moved_rules: m.moved_rules as i32,
            moved_template_lines: m.moved_template_lines as i32,
            source_deleted: m.source_deleted,
        }))
    }

    async fn apply_chart(&self, req: Request<pb::ApplyChartRequest>) -> Result<Response<pb::ApplyChartResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let nodes: Vec<accounts::ChartNode> = r
            .accounts
            .iter()
            .map(|a| accounts::ChartNode { code: a.code.clone(), path: a.path.clone(), r#type: a.r#type.clone(), placeholder: a.placeholder, description: a.description.clone() })
            .collect();
        let (created, existing) = self.run(move |c| accounts::apply_chart(c, r.entity_id, &nodes)).await?;
        Ok(Response::new(pb::ApplyChartResponse { created: created as i32, existing: existing as i32 }))
    }

    async fn remap_accounts(&self, req: Request<pb::RemapAccountsRequest>) -> Result<Response<pb::RemapAccountsResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let nodes: Vec<accounts::ChartNode> = r
            .accounts
            .iter()
            .map(|a| accounts::ChartNode { code: a.code.clone(), path: a.path.clone(), r#type: a.r#type.clone(), placeholder: a.placeholder, description: a.description.clone() })
            .collect();
        let moves: Vec<(i64, String)> = r.moves.iter().map(|m| (m.source_id, m.target_path.clone())).collect();
        let rep = self.run(move |c| accounts::remap_accounts(c, r.entity_id, &nodes, &moves)).await?;
        Ok(Response::new(pb::RemapAccountsResponse { created: rep.created as i32, existing: rep.existing as i32, merged: rep.merged as i32, moved_postings: rep.moved_postings, skipped: rep.skipped }))
    }

    async fn list_journal_entries(&self, req: Request<pb::ListJournalEntriesRequest>) -> Result<Response<pb::ListJournalEntriesResponse>, Status> {
        let r = req.into_inner();
        let f = journal::EntryFilter {
            entity_id: nz(r.entity_id),
            account_id: nz(r.account_id),
            status: if r.status.trim().is_empty() { None } else { Some(EntryStatus::parse(&r.status).map_err(status)?) },
            from: date_opt(&r.from)?,
            to: date_opt(&r.to)?,
            query: r.query.clone(),
            origin: r.origin.clone(),
            only_unreviewed: r.only_unreviewed,
            limit: r.limit as i64,
            offset: r.offset as i64,
        };
        let (entries, total) = self.run(move |c| journal::list_entries(c, &f)).await?;
        Ok(Response::new(pb::ListJournalEntriesResponse { entries: entries.iter().map(entry_pb).collect(), total: total as i32 }))
    }

    async fn confirm_entries(&self, req: Request<pb::ConfirmEntriesRequest>) -> Result<Response<pb::ConfirmEntriesResponse>, Status> {
        self.writable()?;
        let ids = req.into_inner().ids;
        let n = self.run(move |c| journal::mark_reviewed(c, &ids)).await?;
        Ok(Response::new(pb::ConfirmEntriesResponse { confirmed: n as i32 }))
    }

    async fn set_tags(&self, req: Request<pb::SetTagsRequest>) -> Result<Response<pb::JournalEntryResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let entry = self
            .run(move |c| {
                let parsed = means_core::tags::parse(&r.tags.join(" "))?;
                means_core::tags::set_tags(c, r.entry_id, &parsed)?;
                journal::get_entry(c, r.entry_id)
            })
            .await?;
        Ok(Response::new(pb::JournalEntryResponse { entry: Some(entry_pb(&entry)), counterpart: None }))
    }

    async fn get_journal_entry(&self, req: Request<pb::GetJournalEntryRequest>) -> Result<Response<pb::JournalEntryResponse>, Status> {
        let id = req.into_inner().id;
        let (e, cp) = self
            .run(move |c| {
                let e = journal::get_entry(c, id)?;
                let cp = match e.counterpart_id {
                    Some(id) => journal::get_entry(c, id).ok(),
                    None => None,
                };
                Ok((e, cp))
            })
            .await?;
        Ok(Response::new(pb::JournalEntryResponse { entry: Some(entry_pb(&e)), counterpart: cp.as_ref().map(entry_pb) }))
    }

    async fn create_journal_entry(&self, req: Request<pb::CreateJournalEntryRequest>) -> Result<Response<pb::JournalEntryResponse>, Status> {
        self.writable()?;
        let e = req.into_inner().entry.ok_or_else(|| Status::invalid_argument("entry is required"))?;
        let input = entry_input_from_pb(&e)?;
        let out = self.run(move |c| journal::create_entry(c, input)).await?;
        Ok(Response::new(pb::JournalEntryResponse { entry: Some(entry_pb(&out)), counterpart: None }))
    }

    async fn create_simple_entry(&self, req: Request<pb::CreateSimpleEntryRequest>) -> Result<Response<pb::JournalEntryResponse>, Status> {
        self.writable()?;
        let e = req.into_inner().entry.ok_or_else(|| Status::invalid_argument("entry is required"))?;
        let s = simple_from_pb(&e)?;
        let out = self.run(move |c| journal::create_simple(c, s)).await?;
        Ok(Response::new(pb::JournalEntryResponse { entry: Some(entry_pb(&out)), counterpart: None }))
    }

    async fn create_transfer(&self, req: Request<pb::CreateTransferRequest>) -> Result<Response<pb::CreateTransferResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let t = journal::Transfer {
            date: date(&r.date)?,
            from_account_id: r.from_account_id,
            to_account_id: r.to_account_id,
            from_quantity: dec(&r.from_quantity)?,
            to_quantity: dec_opt(&r.to_quantity)?,
            fee: dec_opt(&r.fee)?,
            fee_account_id: nz(r.fee_account_id),
            payee: r.payee.clone(),
            notes: r.notes.clone(),
            from_contra_account_id: nz(r.from_contra_account_id),
            to_contra_account_id: nz(r.to_contra_account_id),
            status: EntryStatus::parse(&r.status).map_err(status)?,
            origin: "transfer".into(),
            external: None,
        };
        let (a, b) = self.run(move |c| journal::create_transfer(c, t)).await?;
        Ok(Response::new(pb::CreateTransferResponse { entry: Some(entry_pb(&a)), counterpart: b.as_ref().map(entry_pb) }))
    }

    async fn update_journal_entry(&self, req: Request<pb::UpdateJournalEntryRequest>) -> Result<Response<pb::JournalEntryResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let e = r.entry.ok_or_else(|| Status::invalid_argument("entry is required"))?;
        let input = entry_input_from_pb(&e)?;
        let id = r.id;
        let out = self
            .run(move |c| {
                let e = journal::update_entry(c, id, input)?;
                // A human's edit is a review.
                journal::mark_reviewed(c, &[id])?;
                Ok(e)
            })
            .await?;
        Ok(Response::new(pb::JournalEntryResponse { entry: Some(entry_pb(&out)), counterpart: None }))
    }

    async fn review_posting(&self, req: Request<pb::ReviewPostingRequest>) -> Result<Response<pb::ReviewPostingResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        if r.capture.is_some() == r.categorize.is_some() {
            return Err(Status::invalid_argument("choose one posting proposal"));
        }
        let proposal = if let Some(entry) = r.capture {
            journal::PostingProposal::Capture { entry: simple_from_pb(&entry)?, tags: r.tags }
        } else {
            let p = r.categorize.unwrap();
            let legs = p.splits.iter().map(|s| Ok(means_core::splits::Leg { account_id: s.account_id, quantity: dec_opt(&s.quantity)?, memo: s.memo.clone() })).collect::<Result<Vec<_>, Status>>()?;
            journal::PostingProposal::Categorize { id: p.id, legs, payee: p.payee, existing: r.existing }
        };
        let (entry, confirmation) = self.run(move |c| journal::confirm_posting(c, proposal, if r.confirmation.is_empty() { None } else { Some(r.confirmation.as_str()) })).await?;
        Ok(Response::new(pb::ReviewPostingResponse { entry: Some(entry_pb(&entry)), confirmation }))
    }

    async fn post_draft(&self, req: Request<pb::PostDraftRequest>) -> Result<Response<pb::JournalEntryResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let splits = r.splits.iter().map(|s| Ok(means_core::splits::Leg { account_id: s.account_id, quantity: dec_opt(&s.quantity)?, memo: s.memo.clone() })).collect::<Result<Vec<_>, Status>>()?;
        let out = self.run(move |c| journal::post_draft_to(c, r.id, &splits, r.payee.as_deref())).await?;
        Ok(Response::new(pb::JournalEntryResponse { entry: Some(entry_pb(&out)), counterpart: None }))
    }

    async fn post_journal_entry(&self, req: Request<pb::PostJournalEntryRequest>) -> Result<Response<pb::JournalEntryResponse>, Status> {
        self.writable()?;
        let id = req.into_inner().id;
        let out = self.run(move |c| journal::post_entry(c, id)).await?;
        Ok(Response::new(pb::JournalEntryResponse { entry: Some(entry_pb(&out)), counterpart: None }))
    }

    async fn void_journal_entry(&self, req: Request<pb::VoidJournalEntryRequest>) -> Result<Response<pb::JournalEntryResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let d = date_opt(&r.date)?;
        let out = self.run(move |c| journal::void_entry(c, r.id, d, &r.reason)).await?;
        Ok(Response::new(pb::JournalEntryResponse { entry: Some(entry_pb(&out)), counterpart: None }))
    }

    async fn delete_journal_entry(&self, req: Request<pb::DeleteJournalEntryRequest>) -> Result<Response<pb::Empty>, Status> {
        self.writable()?;
        let id = req.into_inner().id;
        self.run(move |c| journal::delete_entry(c, id)).await?;
        Ok(Response::new(pb::Empty {}))
    }

    async fn list_templates(&self, req: Request<pb::ListTemplatesRequest>) -> Result<Response<pb::ListTemplatesResponse>, Status> {
        let r = req.into_inner();
        let list = self
            .run(move |c| {
                let mut ts = templates::list_templates(c, nz(r.entity_id), r.include_inactive)?;
                let index = accounts::path_index(c)?;
                let mut out = Vec::new();
                for t in ts.drain(..) {
                    let mut p = template_pb(&t);
                    for l in p.lines.iter_mut() {
                        l.account_path = index.get(&l.account_id).map(|b| b.path.clone()).unwrap_or_default();
                    }
                    out.push(p);
                }
                Ok(out)
            })
            .await?;
        Ok(Response::new(pb::ListTemplatesResponse { templates: list }))
    }

    async fn save_template(&self, req: Request<pb::SaveTemplateRequest>) -> Result<Response<pb::TemplateResponse>, Status> {
        self.writable()?;
        let t = req.into_inner().template.ok_or_else(|| Status::invalid_argument("template is required"))?;
        let core = template_from_pb(&t)?;
        let out = self.run(move |c| templates::save_template(c, &core)).await?;
        Ok(Response::new(pb::TemplateResponse { template: Some(template_pb(&out)) }))
    }

    async fn delete_template(&self, req: Request<pb::DeleteTemplateRequest>) -> Result<Response<pb::Empty>, Status> {
        self.writable()?;
        let id = req.into_inner().id;
        self.run(move |c| templates::delete_template(c, id)).await?;
        Ok(Response::new(pb::Empty {}))
    }

    async fn apply_template(&self, req: Request<pb::ApplyTemplateRequest>) -> Result<Response<pb::JournalEntryResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let d = if r.date.is_empty() { means_core::today() } else { date(&r.date)? };
        let mut inputs: HashMap<usize, Decimal> = HashMap::new();
        for (k, v) in &r.inputs {
            if !v.trim().is_empty() {
                inputs.insert(*k as usize, dec(v)?);
            }
        }
        let st = EntryStatus::parse(&r.status).map_err(status)?;
        let out = self.run(move |c| templates::apply_template(c, r.template_id, d, &inputs, &r.payee, st, "manual")).await?;
        Ok(Response::new(pb::JournalEntryResponse { entry: Some(entry_pb(&out)), counterpart: None }))
    }

    async fn run_schedules(&self, req: Request<pb::RunSchedulesRequest>) -> Result<Response<pb::RunSchedulesResponse>, Status> {
        self.writable()?;
        let until = date_opt(&req.into_inner().until)?;
        let n = self.run(move |c| templates::run_schedules(c, until)).await?;
        Ok(Response::new(pb::RunSchedulesResponse { created: n as i32 }))
    }

    async fn list_imports(&self, req: Request<pb::ListImportsRequest>) -> Result<Response<pb::ListImportsResponse>, Status> {
        let r = req.into_inner();
        let list = self.run(move |c| imports::list_imports(c, nz(r.account_id), r.limit as i64)).await?;
        Ok(Response::new(pb::ListImportsResponse { imports: list.iter().map(import_pb).collect() }))
    }

    async fn upload_import(&self, req: Request<pb::UploadImportRequest>) -> Result<Response<pb::UploadImportResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        if r.content.is_empty() {
            return Err(Status::invalid_argument("the file is empty"));
        }
        let mapping = r.mapping.map(|m| imports::CsvMapping {
            delimiter: m.delimiter,
            header_row: m.header_row,
            date_column: m.date_column,
            date_format: m.date_format,
            amount_column: m.amount_column,
            debit_column: m.debit_column,
            credit_column: m.credit_column,
            description_column: m.description_column,
            reference_column: m.reference_column,
            balance_column: m.balance_column,
            currency_column: m.currency_column,
            decimal_separator: m.decimal_separator,
            invert_sign: m.invert_sign,
            currency: m.currency,
            extra_description_columns: m.extra_description_columns,
        });
        let options = json_opt(&r.options)?;
        let out = self
            .run(move |c| {
                let source = if r.source.is_empty() { "auto".to_string() } else { r.source.clone() };
                if source == "account_tracker" || imports::detect_source(&r.filename, &r.content) == "account_tracker" && source == "auto" {
                    return Err(means_core::Error::Invalid("this is an Account Tracker backup: use the Account Tracker import".into()));
                }
                imports::run_import(c, imports::ImportRequest::new(&source, nz(r.account_id), &r.filename, &r.content).mapping(mapping.as_ref()).preview(r.preview).options(options))
            })
            .await?;
        let lines: Vec<pb::StatementLine> = out
            .lines
            .iter()
            .enumerate()
            .take(if out.import.status == "preview" { 200 } else { 2000 })
            .map(|(i, line)| {
                let mut wire = line_pb(line);
                if out.import.status == "preview" && line.account_id.is_none() {
                    // An unplaced file has no accounting unit yet; display parsed values
                    // without assigning an invented commodity or rounding them.
                    if let Some(parsed) = out.parse.lines.get(i) {
                        wire.amount = parsed.amount.map(plain).unwrap_or_default();
                        wire.balance_after = parsed.balance_after.map(plain).unwrap_or_default();
                    }
                }
                wire
            })
            .collect();
        Ok(Response::new(pb::UploadImportResponse {
            import: Some(import_pb(&out.import)),
            lines,
            headers: out.parse.headers.clone(),
            sample_rows: out.parse.sample_rows.clone(),
            detected_source: out.parse.detected_source.clone(),
        }))
    }

    async fn get_import(&self, req: Request<pb::GetImportRequest>) -> Result<Response<pb::GetImportResponse>, Status> {
        let id = req.into_inner().id;
        let (i, lines) = self.run(move |c| imports::get_import(c, id)).await?;
        Ok(Response::new(pb::GetImportResponse { import: Some(import_pb(&i)), lines: lines.iter().map(line_pb).collect() }))
    }

    async fn complete_import(&self, req: Request<pb::CompleteImportRequest>) -> Result<Response<pb::UploadImportResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let out = self.run(move |c| imports::inbox::complete_import(c, r.id, r.account_id)).await?;
        let lines: Vec<pb::StatementLine> = out.lines.iter().take(2000).map(line_pb).collect();
        Ok(Response::new(pb::UploadImportResponse { import: Some(import_pb(&out.import)), lines, headers: vec![], sample_rows: vec![], detected_source: out.import.source.clone() }))
    }

    async fn delete_import(&self, req: Request<pb::DeleteImportRequest>) -> Result<Response<pb::Empty>, Status> {
        self.writable()?;
        let r = req.into_inner();
        self.run(move |c| imports::delete_import(c, r.id, r.force)).await?;
        Ok(Response::new(pb::Empty {}))
    }

    async fn rematch_import(&self, req: Request<pb::RematchImportRequest>) -> Result<Response<pb::RematchImportResponse>, Status> {
        self.writable()?;
        let id = req.into_inner().id;
        let r = self.run(move |c| imports::rematch_import(c, id)).await?;
        Ok(Response::new(pb::RematchImportResponse { lines: r.lines as i32, rematched: r.rematched as i32, category_kept: r.category_kept as i32, untouched: r.untouched as i32, notes: r.notes }))
    }

    async fn list_statement_lines(&self, req: Request<pb::ListStatementLinesRequest>) -> Result<Response<pb::ListStatementLinesResponse>, Status> {
        let r = req.into_inner();
        let list = self.run(move |c| imports::list_lines(c, nz(r.account_id), &r.status, nz(r.import_id), r.limit as i64)).await?;
        Ok(Response::new(pb::ListStatementLinesResponse { lines: list.iter().map(line_pb).collect() }))
    }

    async fn match_line(&self, req: Request<pb::MatchLineRequest>) -> Result<Response<pb::StatementLine>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let l = self.run(move |c| matcher::match_line(c, r.line_id, r.posting_id)).await?;
        Ok(Response::new(line_pb(&l)))
    }

    async fn create_entry_from_line(&self, req: Request<pb::CreateEntryFromLineRequest>) -> Result<Response<pb::JournalEntryResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let mut splits = Vec::new();
        for s in &r.splits {
            splits.push((s.account_id, dec(&s.quantity)?, s.memo.clone()));
        }
        let out = self.run(move |c| rules::create_entry_from_line(c, r.line_id, nz(r.contra_account_id), &r.payee, nz(r.template_id), r.post, &splits)).await?;
        Ok(Response::new(pb::JournalEntryResponse { entry: Some(entry_pb(&out)), counterpart: None }))
    }

    async fn skip_line(&self, req: Request<pb::SkipLineRequest>) -> Result<Response<pb::StatementLine>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let l = self.run(move |c| imports::skip_line(c, r.line_id, r.unskip)).await?;
        Ok(Response::new(line_pb(&l)))
    }

    async fn suggest_matches(&self, req: Request<pb::SuggestMatchesRequest>) -> Result<Response<pb::SuggestMatchesResponse>, Status> {
        let id = req.into_inner().line_id;
        let (cands, matching, suggested, refunds) = self
            .run(move |c| {
                let sl = imports::get_line(c, id)?;
                let Some(acc_id) = sl.account_id else { return Ok((vec![], vec![], 0, vec![])) };
                let account = accounts::get_account(c, acc_id)?;
                let mut entries = Vec::new();
                if let (Some(d), Some(a)) = (sl.date, sl.amount) {
                    for cand in matcher::candidates_for(c, acc_id, a, d, 7, &sl.description)? {
                        entries.push(journal::get_entry(c, cand.journal_entry_id)?);
                    }
                }
                let all = rules::list_rules(c, Some(account.entity_id))?;
                let canonical = means_core::payees::resolve(c, account.entity_id, &sl.description)?;
                let matching: Vec<Rule> = all.into_iter().filter(|r| rules::matches_with_payee(r, &sl, canonical.as_ref().map(|p| p.name.as_str()).unwrap_or(&sl.description))).collect();
                let suggested = matcher::suggest_account(c, account.entity_id, acc_id, &sl.description)?.unwrap_or(0);
                let refunds = means_core::refunds::candidates(c, id, means_core::refunds::DEFAULT_WINDOW_DAYS)?;
                Ok((entries, matching, suggested, refunds))
            })
            .await?;
        Ok(Response::new(pb::SuggestMatchesResponse {
            candidates: cands.iter().map(entry_pb).collect(),
            matching_rules: matching.iter().map(rule_pb).collect(),
            suggested_account_id: suggested,
            refund_candidates: refunds.iter().map(entry_pb).collect(),
        }))
    }

    async fn refund_candidates(&self, req: Request<pb::RefundCandidatesRequest>) -> Result<Response<pb::RefundCandidatesResponse>, Status> {
        let r = req.into_inner();
        let days = if r.window_days == 0 { means_core::refunds::DEFAULT_WINDOW_DAYS } else { i64::from(r.window_days) };
        let entries = self.run(move |c| means_core::refunds::candidates(c, r.line_id, days)).await?;
        Ok(Response::new(pb::RefundCandidatesResponse { candidates: entries.iter().map(entry_pb).collect() }))
    }

    async fn link_refund(&self, req: Request<pb::LinkRefundRequest>) -> Result<Response<pb::JournalEntryResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let entry = self.run(move |c| means_core::refunds::link(c, r.line_id, r.original_entry_id)).await?;
        Ok(Response::new(pb::JournalEntryResponse { entry: Some(entry_pb(&entry)), counterpart: None }))
    }

    async fn inspect_account_tracker(&self, req: Request<pb::InspectAccountTrackerRequest>) -> Result<Response<pb::InspectAccountTrackerResponse>, Status> {
        self.writable()?;
        let content = req.into_inner().content;
        let insp = tokio::task::spawn_blocking(move || imports::account_tracker::inspect(&content)).await.map_err(|e| Status::internal(e.to_string()))?.map_err(status)?;
        Ok(Response::new(pb::InspectAccountTrackerResponse {
            accounts: insp
                .accounts
                .iter()
                .map(|a| pb::AccountTrackerAccount {
                    external_id: a.external_id.clone(),
                    name: a.name.clone(),
                    currency: a.currency.clone(),
                    group: a.group.clone(),
                    hidden: a.hidden,
                    closed: a.closed,
                    balance: plain(a.balance),
                    opening: plain(a.opening),
                    transactions: a.transactions as i32,
                    credit_limit: a.credit_limit.map(plain).unwrap_or_default(),
                    due_day: a.due_day as i32,
                    exclude: a.exclude,
                })
                .collect(),
            categories: insp.categories.iter().map(|c| pb::AccountTrackerCategory { name: c.name.clone(), uses: c.uses as i32, inflows: c.inflows as i32, refunds: c.refunds as i32 }).collect(),
            groups: insp.groups.clone(),
            transactions: insp.transactions as i32,
            first_date: odate(insp.first_date),
            last_date: odate(insp.last_date),
            base_currency: insp.base_currency.clone(),
            rates: insp.rates.clone().into_iter().collect(),
            recurring: insp.recurring as i32,
        }))
    }

    async fn import_account_tracker(&self, req: Request<pb::ImportAccountTrackerRequest>) -> Result<Response<pb::ImportAccountTrackerResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        if r.default_entity_id <= 0 {
            return Err(Status::invalid_argument("default_entity_id is required"));
        }
        let mappings: Vec<imports::account_tracker::Mapping> = r
            .mapping
            .iter()
            .map(|m| imports::account_tracker::Mapping { external_id: m.external_id.clone(), entity_id: m.entity_id, r#type: m.r#type.clone(), subtype: m.subtype.clone(), skip: m.skip })
            .collect();
        let expand = r.expand_recurring || r.mapping.is_empty() && !r.expand_recurring && true;
        let out = self.run(move |c| imports::account_tracker::import(c, &r.content, &r.filename, &mappings, r.default_entity_id, expand, r.schedules)).await?;
        Ok(Response::new(pb::ImportAccountTrackerResponse {
            import: Some(import_pb(&out.import)),
            warnings: out.warnings,
            checks: out.checks.iter().map(|c| pb::AccountCheck { name: c.name.clone(), expected: plain(c.expected), actual: plain(c.actual), ok: c.ok }).collect(),
        }))
    }

    async fn list_rules(&self, req: Request<pb::ListRulesRequest>) -> Result<Response<pb::ListRulesResponse>, Status> {
        let r = req.into_inner();
        let list = self.run(move |c| rules::list_rules(c, nz(r.entity_id))).await?;
        Ok(Response::new(pb::ListRulesResponse { rules: list.iter().map(rule_pb).collect() }))
    }

    async fn save_rule(&self, req: Request<pb::SaveRuleRequest>) -> Result<Response<pb::RuleResponse>, Status> {
        self.writable()?;
        let r = req.into_inner().rule.ok_or_else(|| Status::invalid_argument("rule is required"))?;
        let core = rule_from_pb(&r);
        let out = self.run(move |c| rules::save_rule(c, &core)).await?;
        Ok(Response::new(pb::RuleResponse { rule: Some(rule_pb(&out)) }))
    }

    async fn delete_rule(&self, req: Request<pb::DeleteRuleRequest>) -> Result<Response<pb::Empty>, Status> {
        self.writable()?;
        let id = req.into_inner().id;
        self.run(move |c| rules::delete_rule(c, id)).await?;
        Ok(Response::new(pb::Empty {}))
    }

    async fn run_rules(&self, req: Request<pb::RunRulesRequest>) -> Result<Response<pb::RunRulesResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let (d, rd) = self.run(move |c| rules::run_rules(c, nz(r.entity_id), nz(r.account_id), r.include_drafts)).await?;
        Ok(Response::new(pb::RunRulesResponse { drafted: d as i32, redrafted: rd as i32 }))
    }

    async fn trial_balance(&self, req: Request<pb::TrialBalanceRequest>) -> Result<Response<pb::ReportResponse>, Status> {
        let r = req.into_inner();
        let as_of = date_opt(&r.as_of)?;
        let rep = self.run(move |c| reports::trial_balance(c, r.entity_id, as_of)).await?;
        Ok(Response::new(report_pb(rep)))
    }

    async fn balance_sheet(&self, req: Request<pb::BalanceSheetRequest>) -> Result<Response<pb::ReportResponse>, Status> {
        let r = req.into_inner();
        let as_of = date_opt(&r.as_of)?;
        let rep = self.run(move |c| reports::balance_sheet(c, nz(r.entity_id), as_of, &r.currency)).await?;
        Ok(Response::new(report_pb(rep)))
    }

    async fn list_connections(&self, _req: Request<pb::ListConnectionsRequest>) -> Result<Response<pb::ListConnectionsResponse>, Status> {
        let manager = self.connections.clone().ok_or_else(|| Status::permission_denied("received vault has no bank connections"))?;
        let result = tokio::task::spawn_blocking(move || manager.list()).await.map_err(|_| Status::internal("connection list failed"))?.map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(result))
    }
    async fn start_connection_job(&self, req: Request<pb::StartConnectionJobRequest>) -> Result<Response<pb::ConnectionJobResponse>, Status> {
        self.writable()?;
        let manager = self.connections.clone().ok_or_else(|| Status::permission_denied("received vault has no bank connections"))?;
        let r = req.into_inner();
        let job = tokio::task::spawn_blocking(move || manager.start(r))
            .await
            .map_err(|_| Status::internal("connection operation failed to start"))?
            .map_err(|e| Status::failed_precondition(e.to_string()))?;
        Ok(Response::new(pb::ConnectionJobResponse { job: Some(job) }))
    }
    async fn configure_connection(&self, req: Request<pb::ConfigureConnectionRequest>) -> Result<Response<pb::BankConnectionResponse>, Status> {
        self.writable()?;
        let manager = self.connections.clone().ok_or_else(|| Status::permission_denied("received vault has no bank connections"))?;
        let r = req.into_inner();
        let account =
            tokio::task::spawn_blocking(move || manager.configure(r)).await.map_err(|_| Status::internal("connection mapping failed"))?.map_err(|e| Status::invalid_argument(e.to_string()))?;
        Ok(Response::new(pb::BankConnectionResponse { account: Some(account) }))
    }

    async fn list_payees(&self, req: Request<pb::ListPayeesRequest>) -> Result<Response<pb::ListPayeesResponse>, Status> {
        let r = req.into_inner();
        let rows = self.run(move |c| means_core::payees::list(c, r.entity_id)).await?;
        Ok(Response::new(pb::ListPayeesResponse { payees: rows.into_iter().map(payee_pb).collect() }))
    }
    async fn save_payee(&self, req: Request<pb::SavePayeeRequest>) -> Result<Response<pb::PayeePreview>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let p = r.payee.ok_or_else(|| Status::invalid_argument("payee is required"))?;
        let out = self
            .run(move |c| {
                means_core::payees::save(
                    c,
                    means_core::payees::Change { id: p.id, entity_id: p.entity_id, name: p.name, active: p.active, aliases: p.aliases },
                    (!r.confirmation.is_empty()).then_some(r.confirmation.as_str()),
                )
            })
            .await?;
        Ok(Response::new(payee_preview_pb(out)))
    }
    async fn link_payees(&self, req: Request<pb::LinkPayeesRequest>) -> Result<Response<pb::PayeePreview>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let out = self.run(move |c| means_core::payees::backfill(c, r.entity_id, (!r.confirmation.is_empty()).then_some(r.confirmation.as_str()), r.chart_reviewed)).await?;
        Ok(Response::new(payee_preview_pb(out)))
    }
    async fn reassign_payee(&self, req: Request<pb::ReassignPayeeRequest>) -> Result<Response<pb::PayeePreview>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let out = self.run(move |c| means_core::payees::reassign(c, r.source_id, r.target_id, nz(r.entry_id), (!r.confirmation.is_empty()).then_some(r.confirmation.as_str()))).await?;
        Ok(Response::new(payee_preview_pb(out)))
    }
    async fn payee_expenses(&self, req: Request<pb::PayeeExpensesRequest>) -> Result<Response<pb::PayeeExpensesResponse>, Status> {
        let r = req.into_inner();
        let from = date_opt(&r.from)?;
        let to = date_opt(&r.to)?;
        let out = self.run(move |c| means_core::payees::expenses(c, r.entity_id, from, to, (!r.tag.is_empty()).then_some(r.tag.as_str()))).await?;
        Ok(Response::new(pb::PayeeExpensesResponse {
            rows: out.rows.into_iter().map(|r| pb::PayeeExpenseRow { payee_id: r.payee_id.unwrap_or(0), name: r.name, amount: plain(r.amount.major()) }).collect(),
            total: plain(out.total.major()),
            currency: out.total.commodity().into(),
        }))
    }

    async fn save_budget(&self, req: Request<pb::SaveBudgetRequest>) -> Result<Response<pb::BudgetResponse>, Status> {
        self.writable()?;
        let b = req.into_inner().budget.ok_or_else(|| Status::invalid_argument("budget is required"))?;
        let starts_on = date_opt(&b.starts_on)?;
        let ends_on = date_opt(&b.ends_on)?;
        let amount = means_core::money::parse(&b.amount).map_err(status)?;
        let saved = self
            .run(move |c| {
                if !b.currency.is_empty() && means_core::entities::get_entity(c, b.entity_id)?.currency != b.currency {
                    return Err(means_core::Error::Invalid("budget currency must match entity".into()));
                }
                means_core::budgets::save(
                    c,
                    nz(b.id),
                    means_core::budgets::BudgetInput {
                        entity_id: b.entity_id,
                        name: b.name,
                        scope: b.scope,
                        account_id: nz(b.account_id),
                        class: b.class,
                        tag: if b.tag.is_empty() { None } else { Some(b.tag) },
                        starts_on,
                        ends_on,
                        amount,
                    },
                )
            })
            .await?;
        Ok(Response::new(pb::BudgetResponse { budget: Some(budget_pb(saved)) }))
    }

    async fn delete_budget(&self, req: Request<pb::DeleteBudgetRequest>) -> Result<Response<pb::Empty>, Status> {
        self.writable()?;
        let id = req.into_inner().id;
        self.run(move |c| means_core::budgets::delete(c, id)).await?;
        Ok(Response::new(pb::Empty {}))
    }

    async fn list_budgets(&self, req: Request<pb::ListBudgetsRequest>) -> Result<Response<pb::ListBudgetsResponse>, Status> {
        let r = req.into_inner();
        let on = date_opt(&r.on)?;
        let rows = self.run(move |c| means_core::budgets::list(c, r.entity_id, on, if r.tag.is_empty() { None } else { Some(&r.tag) })).await?;
        Ok(Response::new(pb::ListBudgetsResponse {
            rows: rows
                .into_iter()
                .map(|r| pb::BudgetProgress { budget: Some(budget_pb(r.budget)), spent: plain(r.spent.major()), remaining: plain(r.remaining.major()), group: r.group, target: r.target })
                .collect(),
        }))
    }

    async fn expenses_by_tag(&self, req: Request<pb::ExpensesByTagRequest>) -> Result<Response<pb::ExpensesByTagResponse>, Status> {
        let r = req.into_inner();
        let from = date_opt(&r.from)?;
        let to = date_opt(&r.to)?;
        let rep = self.run(move |c| reports::expenses_by_tag(c, r.entity_id, from, to, if r.tag.is_empty() { None } else { Some(&r.tag) })).await?;
        Ok(Response::new(pb::ExpensesByTagResponse {
            rows: rep.rows.into_iter().map(|row| pb::TagExpenseRow { tag: row.tag, amount: plain(row.amount.major()) }).collect(),
            total: plain(rep.total.major()),
            currency: rep.total.commodity().into(),
            entity_id: rep.entity_id,
            from: rep.from.map(|d| d.to_string()).unwrap_or_default(),
            to: rep.to.map(|d| d.to_string()).unwrap_or_default(),
            tag: rep.tag.unwrap_or_default(),
            overlapping: rep.overlapping,
        }))
    }

    async fn expenses_by_class(&self, req: Request<pb::ExpensesByClassRequest>) -> Result<Response<pb::ExpensesByClassResponse>, Status> {
        let r = req.into_inner();
        let from = date_opt(&r.from)?;
        let to = date_opt(&r.to)?;
        let rep = self.run(move |c| reports::expenses_by_class(c, r.entity_id, from, to, if r.tag.is_empty() { None } else { Some(&r.tag) })).await?;
        Ok(Response::new(pb::ExpensesByClassResponse {
            rows: rep.rows.into_iter().map(|row| pb::ClassExpenseRow { class: row.class, amount: plain(row.amount.major()) }).collect(),
            total: plain(rep.total.major()),
            currency: rep.total.commodity().into(),
            entity_id: rep.entity_id,
            from: rep.from.map(|d| d.to_string()).unwrap_or_default(),
            to: rep.to.map(|d| d.to_string()).unwrap_or_default(),
            tag: rep.tag.unwrap_or_default(),
        }))
    }

    async fn income_statement(&self, req: Request<pb::IncomeStatementRequest>) -> Result<Response<pb::ReportResponse>, Status> {
        let r = req.into_inner();
        let from = date_opt(&r.from)?;
        let to = date_opt(&r.to)?;
        let rep = self.run(move |c| reports::income_statement(c, r.entity_id, from, to)).await?;
        Ok(Response::new(report_pb(rep)))
    }

    async fn split_category(&self, req: Request<pb::SplitCategoryRequest>) -> Result<Response<pb::SplitCategoryResponse>, Status> {
        self.writable()?;
        let r = req.into_inner();
        let report = self.run(move |c| accounts::split_category(c, r.source_id, r.target_id, &r.query, r.preview)).await?;
        Ok(Response::new(pb::SplitCategoryResponse { entries: report.entries as i64, postings: report.postings as i64 }))
    }

    async fn general_ledger(&self, req: Request<pb::GeneralLedgerRequest>) -> Result<Response<pb::GeneralLedgerResponse>, Status> {
        let r = req.into_inner();
        let from = date_opt(&r.from)?;
        let to = date_opt(&r.to)?;
        let l = self.run(move |c| reports::general_ledger_ordered(c, r.account_id, from, to, r.limit as i64, true, r.newest_first)).await?;
        Ok(Response::new(pb::GeneralLedgerResponse {
            rows: l.rows.iter().map(ledger_row_pb).collect(),
            opening_balance: plain(l.opening_balance.major()),
            closing_balance: plain(l.closing_balance.major()),
            commodity: l.commodity,
        }))
    }

    async fn reconciliation(&self, req: Request<pb::ReconciliationRequest>) -> Result<Response<pb::ReconciliationResponse>, Status> {
        let id = req.into_inner().account_id;
        let rec = self.run(move |c| reports::reconciliation(c, id)).await?;
        Ok(Response::new(pb::ReconciliationResponse {
            statement_balance: rec.statement_balance.map(plain).unwrap_or_default(),
            statement_date: odate(rec.statement_date),
            ledger_balance: plain(rec.ledger_balance),
            difference: rec.difference.map(plain).unwrap_or_default(),
            unmatched_lines: rec.unmatched_lines.len() as i32,
            unreconciled_postings: rec.unreconciled.len() as i32,
            lines: rec.unmatched_lines.iter().map(line_pb).collect(),
            postings: rec.unreconciled.iter().map(ledger_row_pb).collect(),
        }))
    }

    async fn cashflow(&self, req: Request<pb::CashflowRequest>) -> Result<Response<pb::CashflowResponse>, Status> {
        let r = req.into_inner();
        let from = if r.from.is_empty() { means_core::today() - chrono::Duration::days(30) } else { date(&r.from)? };
        let to = if r.to.is_empty() { means_core::today() + chrono::Duration::days(60) } else { date(&r.to)? };
        let days = self.run(move |c| reports::cashflow(c, r.entity_id, from, to)).await?;
        Ok(Response::new(pb::CashflowResponse {
            days: days
                .iter()
                .map(|d| pb::CashflowDay {
                    date: d.date.to_string(),
                    posted_in: plain(d.posted_in),
                    posted_out: plain(d.posted_out),
                    draft_in: plain(d.draft_in),
                    draft_out: plain(d.draft_out),
                    balance: plain(d.balance),
                })
                .collect(),
        }))
    }

    async fn net_worth(&self, req: Request<pb::NetWorthRequest>) -> Result<Response<pb::NetWorthResponse>, Status> {
        let r = req.into_inner();
        let as_of = date_opt(&r.as_of)?;
        let nw = self.run(move |c| reports::net_worth(c, r.entity_id, &r.currency, as_of)).await?;
        Ok(Response::new(pb::NetWorthResponse {
            by_entity: nw.by_entity.iter().map(report_row_pb).collect(),
            total: plain(nw.total.major()),
            currency: nw.currency,
            by_account: nw.by_account.iter().map(report_row_pb).collect(),
        }))
    }
}

#[allow(dead_code)]
fn _unused(conn: &means_core::rusqlite::Connection) -> means_core::Result<()> {
    let _ = hashchain::verify(conn, 0);
    Ok(())
}

fn budget_pb(b: means_core::budgets::Budget) -> pb::Budget {
    pb::Budget {
        id: b.id,
        entity_id: b.entity_id,
        name: b.name,
        scope: b.scope,
        account_id: b.account_id.unwrap_or(0),
        class: b.class,
        tag: b.tag.unwrap_or_default(),
        starts_on: b.starts_on.to_string(),
        ends_on: b.ends_on.to_string(),
        amount: plain(b.limit.major()),
        currency: b.limit.commodity().into(),
    }
}

fn payee_pb(p: means_core::payees::Payee) -> pb::Payee {
    pb::Payee { id: p.id, uid: p.uid, entity_id: p.entity_id, name: p.name, active: p.active, aliases: p.aliases }
}
fn payee_preview_pb(p: means_core::payees::Preview) -> pb::PayeePreview {
    pb::PayeePreview {
        token: p.token,
        applied: p.applied,
        payee: p.payee.map(payee_pb),
        impacts: p
            .impacts
            .into_iter()
            .map(|r| pb::PayeeImpact {
                entry_id: r.entry_id,
                line_id: r.line_id,
                booked: r.booked,
                evidence: r.evidence,
                candidates: r.candidates,
                before_rule: r.before_rule.unwrap_or(0),
                after_rule: r.after_rule.unwrap_or(0),
            })
            .collect(),
        warnings: p.warnings,
        backup: p.backup.unwrap_or_default(),
    }
}

#[cfg(test)]
mod replica_tests {
    use super::*;
    #[tokio::test]
    async fn closing_replica_view_releases_its_listener() {
        let owner = MeansService::new(Arc::new(Db::open_memory().unwrap()), std::env::temp_dir()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://{address}");
        let task = tokio::spawn(async move {
            let _listener = listener;
            std::future::pending::<()>().await;
        });
        owner.viewers.lock().await.insert(url.clone(), crate::sharing::Viewer::new(task));
        owner.sharing(Request::new(pb::SharingRequest { operation: "close-view".into(), arguments: vec![url], ..Default::default() })).await.unwrap();
        assert!(owner.viewers.lock().await.is_empty());
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while tokio::net::TcpStream::connect(address).await.is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn every_mutating_rpc_rejects_a_replica_before_side_effects() {
        let path = std::env::temp_dir().join(format!("means-replica-rpc-{}.sqlite", means_core::new_uid()));
        let db = Db::open(&path).unwrap();
        let entity = entities::create_entity(&mut db.conn(), "Shared", "person", "PT", "EUR").unwrap();
        let snapshot = means_sharing::snapshot::capture(&db.conn(), entity.id).unwrap();
        drop(db);
        // Install in a distinct empty database as the production importer does.
        let replica_path = path.with_extension("replica");
        let staging = Db::open(&replica_path).unwrap();
        means_sharing::snapshot::install(&staging, &snapshot, &entity.uid, "", 0).unwrap();
        drop(staging);
        let db = Arc::new(Db::open_replica(&replica_path).unwrap());
        let service = MeansService { db, connections: None, replica: None, viewers: Default::default() };
        macro_rules! denied {
            ($method:ident,$request:ty) => {
                assert_eq!(service.$method(Request::new(<$request>::default())).await.unwrap_err().code(), tonic::Code::PermissionDenied, stringify!($method));
            };
        }
        denied!(sharing, pb::SharingRequest);
        denied!(create_entity, pb::CreateEntityRequest);
        denied!(update_entity, pb::UpdateEntityRequest);
        denied!(create_commodity, pb::CreateCommodityRequest);
        denied!(set_price, pb::SetPriceRequest);
        denied!(fetch_rates, pb::FetchRatesRequest);
        denied!(create_account, pb::CreateAccountRequest);
        denied!(update_account, pb::UpdateAccountRequest);
        denied!(close_account, pb::CloseAccountRequest);
        denied!(merge_accounts, pb::MergeAccountsRequest);
        denied!(apply_chart, pb::ApplyChartRequest);
        denied!(remap_accounts, pb::RemapAccountsRequest);
        denied!(confirm_entries, pb::ConfirmEntriesRequest);
        denied!(set_tags, pb::SetTagsRequest);
        denied!(create_journal_entry, pb::CreateJournalEntryRequest);
        denied!(create_simple_entry, pb::CreateSimpleEntryRequest);
        denied!(create_transfer, pb::CreateTransferRequest);
        denied!(update_journal_entry, pb::UpdateJournalEntryRequest);
        denied!(post_journal_entry, pb::PostJournalEntryRequest);
        denied!(post_draft, pb::PostDraftRequest);
        denied!(review_posting, pb::ReviewPostingRequest);
        denied!(void_journal_entry, pb::VoidJournalEntryRequest);
        denied!(delete_journal_entry, pb::DeleteJournalEntryRequest);
        denied!(save_template, pb::SaveTemplateRequest);
        denied!(delete_template, pb::DeleteTemplateRequest);
        denied!(apply_template, pb::ApplyTemplateRequest);
        denied!(run_schedules, pb::RunSchedulesRequest);
        denied!(upload_import, pb::UploadImportRequest);
        denied!(complete_import, pb::CompleteImportRequest);
        denied!(delete_import, pb::DeleteImportRequest);
        denied!(rematch_import, pb::RematchImportRequest);
        denied!(match_line, pb::MatchLineRequest);
        denied!(create_entry_from_line, pb::CreateEntryFromLineRequest);
        denied!(skip_line, pb::SkipLineRequest);
        denied!(link_refund, pb::LinkRefundRequest);
        denied!(inspect_account_tracker, pb::InspectAccountTrackerRequest);
        denied!(import_account_tracker, pb::ImportAccountTrackerRequest);
        denied!(save_rule, pb::SaveRuleRequest);
        denied!(delete_rule, pb::DeleteRuleRequest);
        denied!(run_rules, pb::RunRulesRequest);
        denied!(start_connection_job, pb::StartConnectionJobRequest);
        denied!(configure_connection, pb::ConfigureConnectionRequest);
        denied!(save_payee, pb::SavePayeeRequest);
        denied!(link_payees, pb::LinkPayeesRequest);
        denied!(reassign_payee, pb::ReassignPayeeRequest);
        denied!(save_budget, pb::SaveBudgetRequest);
        denied!(delete_budget, pb::DeleteBudgetRequest);
        denied!(split_category, pb::SplitCategoryRequest);
        assert_eq!(service.list_entities(Request::new(pb::ListEntitiesRequest::default())).await.unwrap().into_inner().entities.len(), 1);
        drop(service);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&replica_path);
    }
}
