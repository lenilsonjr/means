# Agent interface

An AI agent or a script can write to the ledger through `means entry`. The agent creates drafts and reads entries. A human posts each draft after a review. The `means policy` commands control whether automatic routes can post.

The commands use the accounting core directly. They do not need a running server.

## Workflow

1. The agent creates a draft with `means entry draft`.
2. The agent or the human runs `means entry propose` to validate the draft and preview it.
3. The human reads the preview.
4. The human runs `means entry confirm` with the digest from the preview. The engine posts the draft only when the digest still matches.

A change to the draft after the proposal changes the digest. The confirmation then fails with `stale_proposal`. Propose the draft again and review the new preview.

## Commands

| Command | Writes | Human only |
| --- | --- | --- |
| `means entry draft --entity <name\|id\|uid> (--json-file <path> \| --stdin)` | A draft | No |
| `means entry propose <uid>` | Nothing | No |
| `means entry confirm <uid> --digest <proposal_digest> [--actor <name>]` | Posts the draft | Yes |
| `means entry void <uid> --reason <text> [--date YYYY-MM-DD]` | A reversal | Yes |
| `means entry list --entity <e> [--status draft\|unreviewed\|posted\|void] [--since D] [--until D] [--limit N] [--json]` | Nothing | No |
| `means entry show <uid> [--json]` | Nothing | No |
| `means policy show [--json]` | Nothing | No |
| `means policy set normal\|draft-only [--json]` | The policy | Yes |

`--actor` defaults to `$USER`. The `unreviewed` status selects posted entries that no human has confirmed.

## Draft document

```json
{
  "date": "2026-04-02",
  "payee": "Sample Taxi",
  "description": "Airport transfer",
  "notes": "",
  "tags": ["trip=sample"],
  "postings": [
    { "account": "Assets:Bank:Operating", "quantity": "-23.40", "commodity": "USD", "memo": "card" },
    { "account": "Expenses:Travel" }
  ],
  "metadata": { "source": "receipt-scan" }
}
```

- `account` is an account path in the entity, or an account uid. The account must be a leaf account and open.
- Exactly one posting can omit `quantity`. That posting balances the entry.
- Write every number as a decimal string. The engine refuses JSON numbers.
- `quantity` is in the account commodity. When `commodity` names another commodity, the engine converts the value at the entry-date rate.
- `amount` is the value in the functional currency of the entity. `rate` is the functional currency per unit of the account commodity. When you give both, the quantity times the rate must equal the amount after rounding.
- Tags use `key=value` or `key:value`. The engine stores them as `key:value`.
- The posting `metadata` goes on the posting. The entry `metadata` goes in the audit event of the draft.
- The engine refuses unknown fields.

The draft has the origin `agent`. It waits for review.

## Output

Each command writes exactly one JSON object to stdout. Each object has `"schema_version": 1` and `"ok"`. Records use `uid` strings. Money uses the `Money` form: `{"minor": "-2340", "commodity": "USD", "precision": 2}`. Rates are decimal strings. Statement lines have no uid, so evidence uses `statement_line_id`.

`draft`:

```json
{"schema_version": 1, "ok": true, "entry": {"uid": "…", "status": "draft", "date": "2026-04-02", "entity_uid": "…", "hash": null}, "audit_event_id": 41}
```

`propose` returns the preview at the top level: `entry_uid`, `entity_uid`, `date`, `status`, `payee`, `description`, `notes`, `tags`, `functional_currency`, `postings`, `totals`, `warnings` and `proposal_digest`. Each posting has `account_uid`, `account_path`, `quantity`, `amount`, `rate`, `rate_source`, `memo` and `metadata`. `totals` has `debit`, `credit` and `balance` in the functional currency.

`proposal_digest` is the SHA-256 of the canonical JSON of the preview without the digest. Canonical JSON has sorted object keys and no whitespace.

`confirm` returns `entry`, `actor`, `proposal_digest` and `audit_event_id`. The audit event records the actor, the action and the digest. The entry gets its review mark.

`void` returns `entry`, `reversal`, `already_void` and `evidence_lines_returned`.

`list --json` returns `entity_uid`, `count`, `total` and `entries`. `show --json` returns `entry`. Each entry has its postings, `evidence`, `review` (`state` and `reviewed_at`), `hash`, and the uids of linked entries. Without `--json`, `list` and `show` print text.

## Errors

A refusal writes one JSON object to stdout and exits with code 1:

```json
{"schema_version": 1, "ok": false, "error": {"code": "stale_proposal", "message": "…"}}
```

| Code | Meaning |
| --- | --- |
| `unbalanced` | The postings do not sum to zero in the functional currency. |
| `missing_rate` | A posting needs an exchange rate that the ledger does not have. Add the rate, then revalue the draft. |
| `locked_period` | The date is on or before the lock date of the entity. |
| `unknown_account` | The account does not exist in the entity. |
| `not_leaf_account` | The account groups other accounts. |
| `not_found` | The entry or the entity does not exist. |
| `stale_proposal` | The draft changed after the proposal, or the digest is wrong. |
| `agent_not_allowed` | The command is a human action and `MEANS_AGENT=1` is set. |
| `invalid_input` | The input is not valid. The message tells you why. |
| `internal` | The engine failed for an unexpected reason, such as a database error. |

Command-line usage errors, such as a missing argument, come from the argument parser. They print text and exit with code 2.

## Agent sessions

Set `MEANS_AGENT=1` in the environment of an agent. The engine then refuses `entry confirm`, `entry void` and `policy set` with `agent_not_allowed`.

`MEANS_AGENT=1` is a workflow aid. It is not an access control. A restricted client needs a separate service boundary.

## Posting policy

The ledger-wide setting `posting_policy` has two values. The engine stores it in the `settings` table. A ledger without the setting uses `normal`.

- `normal`: rules, schedules, the matcher and imports can post, as before.
- `draft_only`: no automatic route posts. Each route creates or keeps a draft instead.

Under `draft_only`:

- A rule that matches an imported statement line creates a draft with the rule's account.
- A rule that runs again on an Uncategorized draft keeps it a draft.
- A scheduled template with automatic posting creates drafts.
- The matcher links a statement line to a draft and keeps the entry a draft.
- An import that would create or update a posted entry creates or keeps a draft.
- `CreateJournalEntry`, `CreateSimpleEntry`, `UpdateJournalEntry` and capture over gRPC store a posted request as a draft. The status in the response shows the stored status.
- `CreateTransfer` creates a draft pair. Both halves are written in one transaction.
- A refund link creates a draft refund.
- The audit event of each held entry records `requested_status` and `posting_policy`.

The engine reads the policy inside the transaction that writes each entry. An unknown stored value stops the write, so a damaged setting never permits posting.

These routes still post an existing draft under `draft_only`, because each one is a human decision on one entry: `means entry confirm`, `means post`, `PostJournalEntry`, `PostDraft` and the categorize proposal of `ReviewPosting`. A void reversal also stays posted, because it corrects an entry that a human already posted.
