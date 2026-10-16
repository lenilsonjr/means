//! The year-end questionnaire: which flags of a jurisdiction the ledger has, and which it lacks.
//!
//! `close_questions.json` holds a short list of questions for each jurisdiction. Each question
//! names an entity flag key. An agent asks a human the open questions once and stores each
//! answer as a flag with `entity_flags::set_flags`.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{entity_flags, Error, Result};

const DATA: &str = include_str!("close_questions.json");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub key: String,
    /// A hint for the answer: bool, date, integer or text.
    pub kind: String,
    pub question: String,
}

#[derive(Deserialize)]
struct Data {
    jurisdictions: BTreeMap<String, Vec<Question>>,
}

/// A flag of the jurisdiction that the ledger has on the date.
#[derive(Debug, Clone, Serialize)]
pub struct Answered {
    pub key: String,
    pub value: Value,
    /// The question of the key, or `None` when the key is not in the list.
    pub question: Option<Question>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Questionnaire {
    pub entity_id: i64,
    pub jurisdiction: String,
    pub as_of: NaiveDate,
    /// Every flag with the prefix of the jurisdiction, with its value on the date.
    pub answered: Vec<Answered>,
    /// The questions whose flag the ledger does not have on the date.
    pub missing: Vec<Question>,
}

/// The question list of every jurisdiction, by uppercase code.
pub fn all() -> Result<BTreeMap<String, Vec<Question>>> {
    let data: Data = serde_json::from_str(DATA)?;
    Ok(data.jurisdictions)
}

/// The question list of one jurisdiction.
pub fn for_jurisdiction(jurisdiction: &str) -> Result<Vec<Question>> {
    let code = jurisdiction.trim().to_ascii_uppercase();
    let mut lists = all()?;
    let known = lists.keys().cloned().collect::<Vec<_>>().join(", ");
    lists.remove(&code).ok_or_else(|| Error::Invalid(format!("no year-end questions for jurisdiction {code:?}; lists exist for {known}")))
}

/// The flags of an entity that belong to a jurisdiction on a date, and the questions without
/// an answer. A flag belongs to a jurisdiction when its key starts with the lowercase code and
/// a dot, for example `sg.`.
pub fn questionnaire(conn: &Connection, entity_id: i64, jurisdiction: &str, as_of: NaiveDate) -> Result<Questionnaire> {
    let questions = for_jurisdiction(jurisdiction)?;
    let code = jurisdiction.trim().to_ascii_uppercase();
    let prefix = format!("{}.", code.to_ascii_lowercase());
    let flags = entity_flags::flags_as_of(conn, entity_id, as_of)?;
    let answered =
        flags.iter().filter(|(k, _)| k.starts_with(&prefix)).map(|(k, v)| Answered { key: k.clone(), value: v.clone(), question: questions.iter().find(|q| &q.key == k).cloned() }).collect();
    let missing = questions.into_iter().filter(|q| !flags.contains_key(&q.key)).collect();
    Ok(Questionnaire { entity_id, jurisdiction: code, as_of, answered, missing })
}
