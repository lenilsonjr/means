# Offline vault-sharing implementation proposal

Status: approved architecture, implemented in `means-sharing`. See the
[user workflow](vault-sharing-usage.md) and [wire specification](vault-sharing-wire-v1.md).
The proposal below records the rationale for the implementation.
Beads: `repo-f5v`. The approved scope is in [vault-sharing.md](vault-sharing.md).

## Approved product choices

- Store each replica in a separate database, or place replicas in the main ledger.
  This proposal uses separate databases. That isolates commodity definitions and
  makes accidental writes through existing accounting commands easier to prevent.
- Store private identity material in a passphrase-encrypted file with a manual
  backup, or use the operating-system keychain with an explicit recovery export.
  This proposal uses a portable encrypted file. This choice is approved.

## Proposed cryptographic boundary

Use the standard [age file format](https://age-encryption.org/v1) through the
[Rust age library](https://docs.rs/age/latest/age/) for recipient encryption.
Use separate Ed25519 signing keys for owner authentication. The proposed signing
library is [ed25519-dalek](https://docs.rs/ed25519-dalek/latest/ed25519_dalek/),
with strict signature verification. Do not convert signing keys into encryption
keys or reuse one secret for both roles.

The public identity record binds the signing public key, encryption recipient,
identity format version and algorithm names. Its fingerprint covers that whole
record. Compare the fingerprint over an independent trusted channel. A signature
made with a key supplied inside the same untrusted bundle does not establish
owner identity.

An age file protects the contents for its recipients. Inside it, use a standard
[JWS compact message](https://www.rfc-editor.org/rfc/rfc7515) with
[the Ed25519 algorithm identifier](https://www.rfc-editor.org/rfc/rfc9864). Its signed JSON
payload contains the manifest and typed snapshot records. Require a protected
`alg: Ed25519` header and a vault-specific protected type. Do not silently
downgrade to the deprecated polymorphic `EdDSA` identifier. Pin the expected owner key; do not
follow key URLs or trust a key supplied by the message. Encryption and a valid
ledger hash alone are not owner authentication.

The proposed manifest uses [RFC 8785 canonical JSON](https://www.rfc-editor.org/rfc/rfc8785).
Use decimal strings for revisions, IDs and Money minor units to avoid numeric
precision loss. Reject duplicate keys and invalid canonical input. Use SHA-256
for manifest and record-set digests. The implementation specification must freeze
exact field names, protected headers, encoding rules and test vectors before
code is written. The JWS signing input and age framing follow their published
formats. A conforming JWS adapter is required; do not invent a signature format.

## Snapshot contents and identity

Export typed records, not a copy of the owner's SQLite file. Include one vault's
chart, entries, postings, tags, notes and the reference data needed by its reports.
Do not execute SQL from a bundle or install an incoming database as trusted data.
Exclude credentials, bank sessions, routing rules, settings and source attachments.

Preserve existing record UIDs. Allocate local integer IDs during import. Every
reference resolves within the pinned owner and vault. Records without UIDs need
versioned natural keys or new stable IDs before they can be shared. The importer
must reject duplicate identities and unresolved required references. An opaque
cross-vault reference must not cause another vault's content to be exported.

The manifest must bind owner identity, recipient identity, vault UID, grant ID and
revision, snapshot revision, previous accepted manifest digest, schema version,
payload digest and ledger checkpoint. It must also bind the grant capability and
validity interval. The recipient checks every binding before installation.

## Proposed replica installation

Existing `Db::open` opens writable storage and runs migrations. It cannot be used
unchanged to open a replica. Add an explicit replica read path that does neither.
Every CLI, TUI and RPC mutation must reject replica targets before it writes.
A database-level write restriction is a second boundary, not a hidden-button rule.

For the separate-database option, build each validated snapshot in a new local
file. Its reference data belongs to that snapshot. Owned vaults remain in their
existing stores. A later consolidated view must state which replicas it includes;
replicas must not silently enter personal net worth totals.

Keep trust state outside the replaceable snapshot. The proposed installation
sequence is:

1. Read a bounded encrypted file and authenticate its complete plaintext.
2. Verify owner signature, grant, recipient, revision and prior digest.
3. Validate all records in staging. Check entity ownership, references, Money
   precision, balanced postings and the accounting hash chain.
4. Flush the completed replica under a new immutable snapshot name.
5. Commit the new trusted revision and active snapshot reference in one local
   trust-store transaction. Only that reference makes the snapshot visible.
6. Retain or remove old unreferenced snapshots under a defined retention policy.

A crash before step 5 leaves the old replica active. An orphan staging file is not
an accepted checkpoint. Recovery must verify the referenced snapshot before it
serves reports. Disk failure must not advance the trusted revision. A repeated
accepted bundle is a no-op; an older or conflicting revision is an error.

## Proposed key lifecycle

Generate signing and encryption keys independently with the library's operating-
system random source. Never put private keys in vault snapshots. Never pass a
passphrase as a command-line argument or log it. If the encrypted-file option is
chosen, use the age passphrase format through its library. Bound decryption work
and input sizes. Do not write temporary plaintext key files.

A manual backup must be restored and fingerprint-checked before it is considered
usable. Losing both the private material and its backup requires a new identity.
Losing the passphrase has no server-side reset. These recovery consequences must
be accepted before shipping the key workflow.

With the old owner key available, rotation can authenticate the new public record
and require an explicit pin update. Without the old key, use independent
fingerprint verification. A recipient-key change requires a replacement grant.
Revocation stops future exports but cannot remove copies already delivered.

## Review gates and validation

Storage, key recovery and the cryptographic container are approved. The exact
wire format needs fixed vectors and a security review. Maintained libraries must
be pinned and their enabled features reviewed before implementation.

Tests must cover signature substitution, changed recipient/grant fields, malformed
canonical data, truncated encryption, missing final authentication, oversized
inputs, wrong passphrases, duplicate UIDs and precision conflicts. They must also
cover rollback, replay, conflicting revisions, interrupted publication and failed
trust-store commits. Exercise every mutation path against a replica, including
core calls, rules, imports and schedules. Compare complete owner and replica
reports across drafts, refunds, reversals and mixed commodities.

This architecture does not authorize remote API access.
