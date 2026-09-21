# Offline sharing wire format, version 1

Version 1 uses separate replica databases, passphrase-encrypted identity files
with manual backup, age recipient encryption, and Ed25519 JWS signatures. Existing
copies remain readable after expiry/revocation; imports/exports of new snapshots
are refused. Transfers use files, and grants provide read-only access.

## Encodings and limits

All signed JSON uses RFC 8785 JCS, UTF-8, with no trailing newline. A receiver
re-encodes and compares the exact bytes before accepting them; duplicate keys,
noncanonical encodings, unknown fields and unsupported versions are rejected.
Counters, SQLite integers and quantities use canonical decimal strings. The
SHA-256 digest is lowercase hex. Record UIDs are UUID strings; opaque external references are at most 256
bytes; local SQLite IDs never identify transferred objects.

The container is binary age v1 encrypted to exactly the intended X25519 recipient.
Its complete plaintext is a JWS compact serialization, with unpadded base64url
segments and protected header exactly
`{"alg":"Ed25519","typ":"means-vault-v1+jws"}`. The signing input is the RFC
7515 ASCII `BASE64URL(header).BASE64URL(payload)`. Ed25519 signs that input directly;
strict verification uses the previously pinned signing key, never message-supplied
keys or URLs. There is no algorithm negotiation or EdDSA fallback. Rotation uses
`means-rotation-v1+jws`; acknowledgement uses `means-receipt-v1+jws`.

Encrypted snapshots are limited to 128 MiB; decoded JWS and JSON are independently
bounded. Identity files are limited to 64 KiB; public identities to 64 KiB. Private
keys use independent OS randomness. age scrypt protects identity files, with
log-N 18 at creation and a maximum accepted log-N of 18. No plaintext key files,
passphrase command-line arguments, or key/passphrase logging are permitted.

## Identity and payload records

Public identity fields are `version` (`"1"`), `signing_algorithm` (`"Ed25519"`),
`signing_key` (32 bytes, base64url), `encryption_algorithm` (`"X25519-age"`),
`recipient` (age recipient string). The fingerprint is SHA-256 of the complete
canonical public record. Both parties explicitly pin the independently checked
fingerprint. Display labels have no authentication role.

The private identity record contains `version`, `signing_secret` (base64url seed),
`encryption_secret` (age secret identity string), and `public`. The encrypted
manual backup must be restored and fingerprint-checked before first use.

A signed delivery has `manifest` and `snapshot`. The manifest contains `version`,
`kind` (`snapshot` or `revocation`), `owner`, `recipient`, `vault`, `grant`,
`grant_revision`, `revision`, `previous`, `not_before`, `expires_at`, `capability`
(`read-only`), `payload_digest`, `ledger_head`, `ledger_count`. Times are RFC3339; generated timestamps use UTC.
The manifest digest is SHA-256 of its canonical JSON. A revocation has no snapshot
and advances grant revision; it retains the last readable snapshot.

A snapshot is a vector of schema-defined tables. A table has `kind` and `records`;
a record has a stable `key` and `fields`. Field cells are typed (`text`, `integer`,
`null`, `reference`, `opaque`) and carry their `value` where applicable. The fixed
allowlist in `snapshot.rs` is the normative record schema; arbitrary SQL, table
names, columns, schema changes and unknown cell types are never executed. Required
fields and cell types are checked before inserting into a freshly created database.
Reference cells contain stable keys; their target table is fixed by the field schema; opaque references retain only the UID
of an excluded counterpart/template, with no traversal or metadata disclosure.

Shared tables: one entity, its accounts, entries, postings, entry tags, canonical
payees, budgets, necessary commodities and prices. UIDs remain unchanged; commodity
codes, price `(commodity,currency,date)` and tag `(entry UID,key)` tuples are
versioned natural keys. Alias/rule configuration, schedules, audit logs, provider
sessions, local settings, imports and source attachments are excluded. Posting
metadata remains an opaque JSON string belonging to the selected vault.

## Revisions and publication

Trust state is a separate local SQLite file. Every recipient has an independent
grant stream. A new grant starts at revision 1 with an empty predecessor. The owner
retains each exported bundle for safe retry. An import creates a recipient-signed
receipt binding owner, recipient, vault, grant, revision and manifest digest. The
owner must accept that receipt before exporting the next revision. A missing
predecessor is an error, not a silent rebootstrap. A replacement recipient key
requires a new grant. Revocation stops new owner exports immediately and can be
sent as an authenticated delivery after the prior checkpoint is acknowledged.

Import validates recipient, pinned owner, capability, validity, revisions, prior
digest, payload digest, references, commodity precision, balanced postings, dates,
lock date and complete ledger chain. Staging uses newly allocated local IDs. The
replica's SQLite file is flushed under a unique immutable name before the trust
store commits the active pointer and accepted checkpoint in one transaction.
Failed commits leave the old snapshot active. Unreferenced files are never opened
as accepted replicas. Keep immutable snapshots and outbound bundles until explicit
manual removal; no automatic retention deletion is performed in version 1.

Replicas carry a marker and deny-write triggers on all tables. `Db::open` refuses
them before migrations. The replica reader uses SQLite read-only mode, no
migrations and query-only enforcement. The version-1 replica schema is fixed at
16 independently of future owned-book migrations; future core changes must keep
its read queries compatible (or add a versioned reader). Replica mutations fail even through direct core calls. The server disables side-effecting integration/inbox jobs for replica
sessions. Replica views remain separate and never join owned-vault totals.
Recovery rechecks the installed file digest and pinned manifest before serving it.

Rotation is a signed transition from old public identity to new public identity,
explicitly confirmed by the operator. Owner rotation is refused while any outbound
delivery awaits a receipt, so no old-key pending bundle becomes unverifiable.
Existing streams preserve their original
owner namespace and checkpoints; the current signing pin changes. Lost-key
recovery requires independent fingerprint verification and a new grant/replica,
never an unauthenticated reset. A recipient key change always needs a replacement
grant. Local-clock rollback and compromise of the trust store are outside the
offline guarantee. Revocation cannot erase received copies.

## Security review checklist and fixed vectors

Before release, verify strict fixed-header JWS interoperability with an independent
implementation, RFC 8032 Ed25519 vectors, canonical duplicate-key rejection, wrong
recipient/owner/grant failures, complete age stream authentication, bounded inputs
and scrypt work, snapshot allowlists and reference isolation, read-only paths,
replay/rollback/conflict rejection and crash-safe pointer publication. Fixed vectors
live in `crates/means-sharing/test-vectors/ed25519-jws.json`, generated and verified
independently with joserfc 1.5.0 and the RFC 8032 test seed; no production keys
appear there. The Rust test requires byte-for-byte matching compact JWS output.
