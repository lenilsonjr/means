# Vault grants and sync design

Status: **first-version direction approved**, 2026-09-19. Beads: `repo-09h`.
This page satisfies D9's requirement to design sharing before writing network code.
It records the agreed first-version scope and the implementation gates; it does not
change the current local-only API policy.
The questions below restate the issue's three named topics; the original Linear
question text is not present in the repository.

## Current implementation and its limits

A vault is one entity's books. SQLite remains the canonical local store, and the
same accounting core serves CLI, TUI and gRPC. Offline grants and isolated read-only replicas are implemented by `means-sharing`.
There is no remote authentication or network sync endpoint. See the
[user workflow](vault-sharing-usage.md) and [wire format](vault-sharing-wire-v1.md).

| Concern | Evidence in the repository | Consequence for sharing |
| --- | --- | --- |
| Stable identifiers | `0001_init.sql` gives entities, accounts, entries, postings, templates and imports a `uid`; `new_uid` creates UUID v7 values | Preserve these identities across replicas; local integer primary keys cannot travel as identities |
| Incomplete UID coverage | Statement lines, rules, prices and commodities lack a `uid` column in that schema | Define stable identities for every shared record or an explicit versioned natural-key representation |
| Public contract | `proto/means/v1/means.proto` exposes integer IDs; `Entity`, `Account` and `JournalEntry` omit their stored UIDs | A sharing contract needs stable external references and a local-ID translation boundary |
| Vault scope | `GetAccount` and `GetJournalEntry` accept just an integer ID; consolidated reports can select all entities | A future remote boundary must check ownership of each referenced object, not merely accept an entity ID supplied by the caller |
| Ledger chain | `hashchain::canonical` covers entry UID, date, payee, description and posting account UID, quantity, amount and commodity | The head is a fingerprint of this accounting content, not a commitment to the whole database |
| Editable history | `hashchain::rechain` deliberately recalculates descendants after permitted edits | A chain sequence is not a replication revision or an append-only change log |
| Audit history | `audit_log` records local row IDs, before/after values and timestamps, without authenticated actors or replication revisions | It cannot be reused unchanged as an authenticated sync log |

The ledger hash excludes, among other things, notes, tags, account names, import
blobs, rules, schedules and permission records. A party controlling a database can
rewrite it and calculate a consistent chain. Verifying that database against its
own newly supplied head cannot prove that previously seen history was preserved.
A separately trusted checkpoint can detect changes to the accounting content it
commits to; it does not prove the bank evidence was true or that all activity was
recorded.

## Decisions

The user explicitly selected all three first-version choices on 2026-09-19.
These decisions refine D9; broader alternatives remain outside the first version.

| Question | Approved first version | Alternatives and their extra scope |
| --- | --- | --- |
| Q-V1: first grant | Read-only replication of one complete vault's ledger | Editing adds writer authorization and conflict handling; selected-account access needs a projection model that preserves privacy and explains incomplete books |
| Q-V2: sync transport | Explicit encrypted file export and import; no listening socket | Authenticated HTTPS pull needs a separately approved hosted deployment; bidirectional peers also need discovery, availability and writer conflict semantics |
| Q-V3: identity | Locally generated keys, with fingerprints checked outside the transferred file | An external login provider adds an account service and its availability/recovery policy |

The following design describes that combination. The [wire specification](vault-sharing-wire-v1.md) defines the implemented container.

## First-grant flow

1. The recipient creates a local identity and supplies its public identity material
   and fingerprint. The owner verifies the fingerprint through an already trusted
   channel. Merely attaching a new key to the first bundle does not establish trust.
2. The owner chooses one vault and previews what will be disclosed: chart, posted
   and draft entries, voids/reversals, posting metadata, notes, tags, and the
   commodity/price definitions needed to reproduce its reports. No other vault's
   books, credentials, bank sessions, inbox routing, local settings or private keys
   are included. Source attachments are excluded in the first version: a single
   original statement can include other accounts or entities. Their checksums may
   be retained as references but do not imply that the recipient has the evidence.
3. The owner issues a grant binding a unique grant ID, vault UID, recipient identity,
   owner identity, read-only capability, grant revision, validity interval and
   export schema. Grant records and their revocations are authenticated by the
   owner; an email address or display name is only a label.
4. An export produces a signed, recipient-encrypted snapshot plus a manifest. The
   manifest binds the vault, grant revision, snapshot revision, previous accepted
   manifest digest, content digest, schema/canonicalization versions and ledger
   checkpoint. The payload digest covers every exported record, including content
   outside the existing accounting hash. Private keys never enter the bundle.
5. Import verifies the pinned owner, recipient binding, signature, permission,
   validity, schema, revision and content before making data visible. It stages and
   validates the complete snapshot, then publishes it atomically as a read-only
   replica, with the verified manifest retained locally.

The cryptographic container, algorithms, key storage and recovery format need a
separate implementation specification using maintained libraries before coding.
The [implementation proposal](vault-sharing-implementation.md) makes these gates concrete; its choices are approved and implemented.
This design deliberately supplies no home-grown encryption or signature format.
Fingerprint exchange, signing identity and encryption keys must have defined roles
in that specification; a key must not be repurposed across algorithms by assumption.

## Replication and conflict semantics

The originating vault is the sole writer in this first version. A recipient can
read a replica but cannot post, import into, recategorize or edit it through any
client. Choosing to fork a writable local copy would create a new vault identity;
such a workflow is outside the first version. A recipient's other local vaults remain
independent and cannot be overwritten by incoming data.

An initial full snapshot establishes the replica. Each subsequent full snapshot
names the previous manifest revision and digest that this recipient accepted. An
exact repeat is a no-op. Older revisions, the same revision with different bytes,
a different owner, or a missing predecessor are errors. If snapshots are skipped,
the owner must issue a snapshot based on the recipient's last acknowledged manifest
or offer an explicit rebootstrap with a reviewed checkpoint. No last-write-wins
merge and no silent reset of the pinned checkpoint is permitted.

Snapshot revisions are owner-issued monotonic replication revisions, independent
of dates, integer row IDs and ledger sequence numbers. An edit dated on or before the lock date
is rejected by accounting invariants; a permitted edit after it produces a new
snapshot revision even if the ledger chain is recalculated. A full snapshot defines
the complete shared set, so removing a draft does not require guessing from missing
delta records. Incremental sync would need a separate operation/tombstone design.

Incoming records resolve only within the tuple of owner identity, vault UID and
record UID. Local IDs are allocated locally. References, account ownership,
commodity precision, balanced postings and lock constraints are validated before
publication. Two vaults using the same commodity code with different definitions
must not silently rewrite shared local commodity rows: namespace the replica's
reference data, or reject the import until the conflict is explicitly resolved.
The storage implementation must make that isolation concrete before release.

Cross-vault transfers export only the selected vault's side. They retain an opaque
counterpart reference where appropriate, never automatically authorize fetching
or exporting the other side. An absent counterpart must not make the local entry
unbalanced, reveal the other vault's private metadata, or trigger a network fetch.

## Trust, rotation and revocation

The recipient pins the owner's identity independently of the storage or delivery
host. A newer signed manifest binds the previous trusted digest. The recipient
stores its last accepted revision/checkpoint separately from a replaceable imported
snapshot. A host cannot reset that state by supplying a fresh copy of the database.
An attacker controlling both the recipient's data and its trust store is outside
this verification guarantee; a signature also does not prove an owner has not
issued different snapshots to different recipients.

Grant revocation prevents future exports once the owner applies it. It cannot erase
plaintext already delivered, and an offline recipient cannot learn about revocation
until it receives a new authenticated record. The UI must show the last verified
revision and grant validity, not claim real-time permission freshness.

Owner-key rotation requires authorization by the previously trusted identity and
explicit pin migration. Loss of that key requires an out-of-band re-verification
flow; a bundle claiming a new owner key is insufficient. Recipient-key rotation
requires a replacement grant. Recovery and expiration enforcement, including the
limits of an offline local clock, must be specified before any shipped key workflow.

## Gates before implementation and release

Q-V1 through Q-V3 are approved. Implementation needs stable wire identities,
replica isolation/write enforcement, snapshot and grant schemas, key lifecycle,
serialization/cryptography, CLI/TUI flows and validation. That work is tracked
separately as `repo-f5v`. Choosing HTTPS or multiple writers requires a
new design decision rather than quietly extending this file-transfer design.

The implementation's acceptance tests must cover these observable cases:

- Identical local integer IDs in two stores cannot conflate records; references
  resolve to the correct vault and preserved UID.
- Granting one vault never discloses a second vault through direct IDs, reports,
  shared imports, cross-vault entries or global reference tables.
- Read-only enforcement holds for every mutation path, including imports, scheduled
  templates, rules and direct core commands, not only hidden UI buttons.
- A tampered signature, recipient, payload, grant, prior digest or version rejects
  the whole snapshot with no partially installed records.
- Replaying an accepted snapshot is harmless; rollback and conflicting revisions
  are rejected; valid post-lock edits can advance to a new verified revision.
- An interrupted import leaves either the old complete replica or the new complete
  replica. The checkpoint cannot advance ahead of durable payload publication.
- Imported reports match the owner snapshot, including Money precision, drafts,
  reversals and lock semantics, without changing another vault's commodity data.
- Revocation, expiration, key rotation, missing predecessors and recovery have
  explicit outcomes and tests; already delivered data is never claimed recoverable.

No network deployment, remote API access or cryptographic implementation is
approved by this page. The current loopback binding and trusted-origin checks
continue to apply.
