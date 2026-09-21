# Statement fixtures

These are small parser examples. Account-holder names use fictional placeholders.
Account numbers and document numbers are dummy values; IBAN-shaped examples use
invalid check digits. Bank names, field names, statuses, and merchant formats
exercise provider behavior. Amounts and dates exercise arithmetic, ordering,
rounding, and duplicate checks; they are not a sample ledger to reconcile.

Keep new fixtures synthetic. Construct the minimum records needed to reproduce a
parser shape or accounting edge case. Replace identities, references, account
numbers, and transaction details before adding a regression from a reported bug.
Keep both CSV and OFX versions consistent when a test compares them. Preserve the
legacy fixture encodings and line endings.

Private Account Tracker checks are ignored by default. To run one deliberately:

```sh
MEANS_ATB=/path/to/backup.atb cargo test -p means-core --test pipeline account_tracker_real_backup -- --ignored --exact
```

These checks use temporary databases but can print the supplied backup's data on
failure. Run them in a private environment. They require an explicit path and
never search the home directory. The ordinary test suite uses synthetic fixtures.
