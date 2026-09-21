//! A lazy gRPC client: connects on first use, forgets the channel on transport errors.

use anyhow::{anyhow, Result};
use means_proto::v1 as pb;
use means_proto::v1::means_client::MeansClient;
use tonic::transport::Channel;

const MAX_MESSAGE: usize = 128 * 1024 * 1024;

#[derive(Clone)]
pub struct Client {
    addr: String,
    inner: Option<MeansClient<Channel>>,
}

macro_rules! rpc {
    ($name:ident, $req:ty, $resp:ty) => {
        pub async fn $name(&mut self, req: $req) -> Result<$resp> {
            let c = self.get().await?;
            match c.$name(req).await {
                Ok(r) => Ok(r.into_inner()),
                Err(status) => {
                    if matches!(status.code(), tonic::Code::Unavailable | tonic::Code::Unknown | tonic::Code::Internal) && status.message().contains("connect") {
                        self.inner = None;
                    }
                    Err(anyhow!("{}", status.message()))
                }
            }
        }
    };
}

impl Client {
    rpc!(review_posting, pb::ReviewPostingRequest, pb::ReviewPostingResponse);
    rpc!(post_draft, pb::PostDraftRequest, pb::JournalEntryResponse);
    rpc!(sharing, pb::SharingRequest, pb::SharingResponse);
    pub fn new(addr: &str) -> Client {
        let addr = if addr.starts_with("http") { addr.to_string() } else { format!("http://{addr}") };
        Client { addr, inner: None }
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    async fn get(&mut self) -> Result<&mut MeansClient<Channel>> {
        if self.inner.is_none() {
            let endpoint = Channel::from_shared(self.addr.clone()).map_err(|e| anyhow!("bad server address {}: {e}", self.addr))?;
            let channel = endpoint.connect().await.map_err(|e| anyhow!("cannot reach {}: {e}", self.addr))?;
            self.inner = Some(MeansClient::new(channel).max_decoding_message_size(MAX_MESSAGE).max_encoding_message_size(MAX_MESSAGE));
        }
        Ok(self.inner.as_mut().unwrap())
    }

    rpc!(list_payees, pb::ListPayeesRequest, pb::ListPayeesResponse);
    rpc!(save_payee, pb::SavePayeeRequest, pb::PayeePreview);
    rpc!(link_payees, pb::LinkPayeesRequest, pb::PayeePreview);
    rpc!(reassign_payee, pb::ReassignPayeeRequest, pb::PayeePreview);
    rpc!(payee_expenses, pb::PayeeExpensesRequest, pb::PayeeExpensesResponse);
    rpc!(split_category, pb::SplitCategoryRequest, pb::SplitCategoryResponse);
    rpc!(get_status, pb::GetStatusRequest, pb::GetStatusResponse);
    rpc!(list_entities, pb::ListEntitiesRequest, pb::ListEntitiesResponse);
    rpc!(list_commodities, pb::ListCommoditiesRequest, pb::ListCommoditiesResponse);
    rpc!(list_accounts, pb::ListAccountsRequest, pb::ListAccountsResponse);
    rpc!(list_journal_entries, pb::ListJournalEntriesRequest, pb::ListJournalEntriesResponse);
    rpc!(get_journal_entry, pb::GetJournalEntryRequest, pb::JournalEntryResponse);
    rpc!(update_journal_entry, pb::UpdateJournalEntryRequest, pb::JournalEntryResponse);
    rpc!(confirm_entries, pb::ConfirmEntriesRequest, pb::ConfirmEntriesResponse);
    rpc!(delete_journal_entry, pb::DeleteJournalEntryRequest, pb::Empty);
    rpc!(create_simple_entry, pb::CreateSimpleEntryRequest, pb::JournalEntryResponse);
    rpc!(list_statement_lines, pb::ListStatementLinesRequest, pb::ListStatementLinesResponse);
    rpc!(refund_candidates, pb::RefundCandidatesRequest, pb::RefundCandidatesResponse);
    rpc!(link_refund, pb::LinkRefundRequest, pb::JournalEntryResponse);
    rpc!(skip_line, pb::SkipLineRequest, pb::StatementLine);
    rpc!(list_imports, pb::ListImportsRequest, pb::ListImportsResponse);
    rpc!(upload_import, pb::UploadImportRequest, pb::UploadImportResponse);
    rpc!(inspect_account_tracker, pb::InspectAccountTrackerRequest, pb::InspectAccountTrackerResponse);
    rpc!(import_account_tracker, pb::ImportAccountTrackerRequest, pb::ImportAccountTrackerResponse);
    rpc!(complete_import, pb::CompleteImportRequest, pb::UploadImportResponse);
    rpc!(list_connections, pb::ListConnectionsRequest, pb::ListConnectionsResponse);
    rpc!(start_connection_job, pb::StartConnectionJobRequest, pb::ConnectionJobResponse);
    rpc!(configure_connection, pb::ConfigureConnectionRequest, pb::BankConnectionResponse);
    rpc!(save_budget, pb::SaveBudgetRequest, pb::BudgetResponse);
    rpc!(delete_budget, pb::DeleteBudgetRequest, pb::Empty);
    rpc!(list_budgets, pb::ListBudgetsRequest, pb::ListBudgetsResponse);
    rpc!(expenses_by_class, pb::ExpensesByClassRequest, pb::ExpensesByClassResponse);
    rpc!(expenses_by_tag, pb::ExpensesByTagRequest, pb::ExpensesByTagResponse);
    rpc!(trial_balance, pb::TrialBalanceRequest, pb::ReportResponse);
    rpc!(income_statement, pb::IncomeStatementRequest, pb::ReportResponse);
    rpc!(balance_sheet, pb::BalanceSheetRequest, pb::ReportResponse);
    rpc!(net_worth, pb::NetWorthRequest, pb::NetWorthResponse);
    rpc!(general_ledger, pb::GeneralLedgerRequest, pb::GeneralLedgerResponse);
    rpc!(create_entity, pb::CreateEntityRequest, pb::EntityResponse);
    rpc!(create_account, pb::CreateAccountRequest, pb::AccountResponse);
    rpc!(update_account, pb::UpdateAccountRequest, pb::AccountResponse);
    rpc!(merge_accounts, pb::MergeAccountsRequest, pb::MergeAccountsResponse);
    rpc!(close_account, pb::CloseAccountRequest, pb::AccountResponse);
    rpc!(apply_chart, pb::ApplyChartRequest, pb::ApplyChartResponse);
    rpc!(save_rule, pb::SaveRuleRequest, pb::RuleResponse);
    rpc!(run_rules, pb::RunRulesRequest, pb::RunRulesResponse);
    rpc!(set_tags, pb::SetTagsRequest, pb::JournalEntryResponse);
}
