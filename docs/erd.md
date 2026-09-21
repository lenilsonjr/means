# Entity-relationship diagram

Generated from `crates/means-core/migrations/` by `scripts/gen_erd.py`; regenerate after a migration, never edit.
Field semantics live in `docs/data-model.md` section 3. Money columns are INTEGER counts of their commodity's minor unit (D14); dates are TEXT `YYYY-MM-DD`.

```mermaid
erDiagram
  entities {
    INTEGER id PK
    TEXT uid UK
    TEXT name UK
    TEXT kind
    TEXT country
    TEXT currency
    TEXT lock_date
    TEXT archived_at
    TEXT created_at
    TEXT updated_at
  }
  commodities {
    INTEGER id PK
    TEXT code UK
    TEXT kind
    TEXT name
    INTEGER precision
    TEXT isin
  }
  prices {
    INTEGER id PK
    INTEGER commodity_id FK "commodities"
    INTEGER currency_id FK "commodities"
    TEXT on_date
    TEXT price
    TEXT source
  }
  accounts {
    INTEGER id PK
    TEXT uid UK
    INTEGER entity_id FK "entities"
    INTEGER parent_id FK "accounts"
    TEXT code
    TEXT name
    TEXT type
    TEXT subtype
    INTEGER commodity_id FK "commodities"
    TEXT system_role
    INTEGER placeholder
    INTEGER in_net_worth
    TEXT credit_limit
    INTEGER statement_day
    INTEGER due_day
    TEXT external_ids
    TEXT notes
    INTEGER position
    TEXT closed_at
    TEXT created_at
    TEXT updated_at
    TEXT class
  }
  journal_entries {
    INTEGER id PK
    TEXT uid UK
    INTEGER entity_id FK "entities"
    TEXT date
    TEXT payee
    TEXT description
    TEXT notes
    TEXT status
    INTEGER reverses_id FK "journal_entries"
    INTEGER counterpart_id FK "journal_entries"
    INTEGER template_id FK "entry_templates"
    INTEGER template_version
    TEXT origin
    TEXT posted_at
    INTEGER seq
    TEXT prev_hash
    TEXT hash
    TEXT created_at
    TEXT updated_at
  }
  postings {
    INTEGER id PK
    TEXT uid UK
    INTEGER journal_entry_id FK "journal_entries"
    INTEGER account_id FK "accounts"
    INTEGER quantity
    INTEGER amount
    TEXT rate
    TEXT rate_source
    TEXT memo
    TEXT metadata
    TEXT external_id
    TEXT fingerprint
    TEXT reconciled_at
    INTEGER position
  }
  entry_templates {
    INTEGER id PK
    TEXT uid UK
    INTEGER entity_id FK "entities"
    TEXT name
    TEXT payee
    TEXT description
    TEXT lines
    TEXT rrule
    TEXT starts_on
    TEXT next_on
    TEXT ends_on
    INTEGER auto_post
    INTEGER lead_days
    INTEGER version
    INTEGER active
    TEXT created_at
    TEXT updated_at
  }
  rules {
    INTEGER id PK
    INTEGER entity_id FK "entities"
    TEXT name
    INTEGER position
    INTEGER enabled
    TEXT conditions
    INTEGER account_id FK "accounts"
    INTEGER template_id FK "entry_templates"
    TEXT payee
    INTEGER hits_count
    TEXT created_at
    TEXT tags
  }
  imports {
    INTEGER id PK
    TEXT uid UK
    TEXT source
    INTEGER account_id FK "accounts"
    TEXT filename
    TEXT checksum
    TEXT status
    TEXT period_from
    TEXT period_to
    TEXT opening_balance
    TEXT closing_balance
    INTEGER lines_count
    INTEGER created_count
    INTEGER matched_count
    INTEGER duplicate_count
    INTEGER skipped_count
    INTEGER unmatched_count
    INTEGER error_count
    TEXT error
    TEXT options
    BLOB content
    TEXT created_at
  }
  statement_lines {
    INTEGER id PK
    INTEGER import_id FK "imports"
    INTEGER account_id FK "accounts"
    INTEGER position
    TEXT raw
    TEXT date
    INTEGER amount
    TEXT currency
    TEXT description
    TEXT reference
    INTEGER balance_after
    TEXT fingerprint
    INTEGER posting_id FK "postings"
    INTEGER journal_entry_id FK "journal_entries"
    INTEGER duplicate_of_id FK "statement_lines"
    TEXT status
    TEXT note
  }
  import_profiles {
    INTEGER id PK
    TEXT source
    TEXT filename_glob
    INTEGER account_id FK "accounts"
    INTEGER hits_count
    TEXT updated_at
  }
  audit_log {
    INTEGER id PK
    TEXT at
    TEXT table_name
    INTEGER row_id
    TEXT action
    TEXT before
    TEXT after
  }
  settings {
    TEXT key PK
    TEXT value
  }
  entry_tags {
    INTEGER entry_id PK "journal_entries"
    TEXT key PK
    TEXT value PK
  }
  accounts |o--o{ accounts : "parent_id"
  accounts |o--o{ imports : "account_id"
  accounts |o--o{ rules : "account_id"
  accounts |o--o{ statement_lines : "account_id"
  accounts ||--o{ import_profiles : "account_id"
  accounts ||--o{ postings : "account_id"
  commodities ||--o{ accounts : "commodity_id"
  commodities ||--o{ prices : "commodity_id"
  commodities ||--o{ prices : "currency_id"
  entities ||--o{ accounts : "entity_id"
  entities ||--o{ entry_templates : "entity_id"
  entities ||--o{ journal_entries : "entity_id"
  entities ||--o{ rules : "entity_id"
  entry_templates |o--o{ journal_entries : "template_id"
  entry_templates |o--o{ rules : "template_id"
  imports ||--o{ statement_lines : "import_id"
  journal_entries |o--o{ journal_entries : "counterpart_id"
  journal_entries |o--o{ journal_entries : "reverses_id"
  journal_entries |o--o{ statement_lines : "journal_entry_id"
  journal_entries ||--o{ entry_tags : "entry_id"
  journal_entries ||--o{ postings : "journal_entry_id"
  postings |o--o{ statement_lines : "posting_id"
  statement_lines |o--o{ statement_lines : "duplicate_of_id"
```
