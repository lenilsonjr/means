//! The ledger-wide posting policy.
//!
//! `normal` keeps the usual behaviour: rules, schedules, the matcher and imports can post.
//! `draft_only` stops every automatic route from posting. Those routes create or keep drafts
//! instead, and a human posts each draft after review. Void reversals stay posted, because a
//! void corrects an entry that a human already posted.

use rusqlite::Connection;

use crate::model::EntryStatus;
use crate::{Error, Result};

/// The `settings` key that holds the policy.
pub const SETTING_KEY: &str = "posting_policy";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostingPolicy {
    Normal,
    DraftOnly,
}

impl PostingPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            PostingPolicy::Normal => "normal",
            PostingPolicy::DraftOnly => "draft_only",
        }
    }

    /// Accept `normal`, `draft_only` and `draft-only`.
    pub fn parse(s: &str) -> Result<PostingPolicy> {
        match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "normal" => Ok(PostingPolicy::Normal),
            "draft_only" => Ok(PostingPolicy::DraftOnly),
            other => Err(Error::Invalid(format!("unknown posting policy {other:?}; use normal or draft-only"))),
        }
    }
}

/// Read the policy. A ledger without the setting uses `normal`.
/// An unknown stored value is an error, so a damaged setting never permits posting.
pub fn current(conn: &Connection) -> Result<PostingPolicy> {
    match crate::db::get_setting(conn, SETTING_KEY)? {
        None => Ok(PostingPolicy::Normal),
        Some(value) => PostingPolicy::parse(&value),
    }
}

/// Store the policy and write an audit event. Returns the policy before the change.
pub fn set(conn: &mut Connection, policy: PostingPolicy) -> Result<PostingPolicy> {
    let tx = conn.transaction()?;
    let before = current(&tx)?;
    crate::db::set_setting(&tx, SETTING_KEY, policy.as_str())?;
    crate::audit::log(&tx, "settings", 0, "posting_policy", Some(serde_json::json!({ "posting_policy": before.as_str() })), Some(serde_json::json!({ "posting_policy": policy.as_str() })))?;
    tx.commit()?;
    Ok(before)
}

/// The status an automatic route may give a new entry. Under `draft_only`, a request to post
/// becomes a draft. Call it inside the transaction that writes the entry.
pub fn admit(conn: &Connection, requested: EntryStatus) -> Result<EntryStatus> {
    if requested == EntryStatus::Posted && current(conn)? == PostingPolicy::DraftOnly {
        return Ok(EntryStatus::Draft);
    }
    Ok(requested)
}

/// True when automatic routes may post.
pub fn allows_auto_post(conn: &Connection) -> Result<bool> {
    Ok(current(conn)? == PostingPolicy::Normal)
}
