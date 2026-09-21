# Share a read-only vault

A vault is one entity’s books. Sharing sends an encrypted file containing that
vault’s chart, journal (including drafts and reversals), tags, notes, canonical
payees, budgets and the commodities/prices needed for its reports. Bank sessions,
credentials, source attachments, import rules and aliases are excluded.

Use **0 → Sharing** in the TUI. Choose an action with `j/k` and `Enter`; `Tab`
moves between fields. `Ctrl-P` submits an action or previews a grant/revocation;
`Ctrl-Y` confirms the displayed preview. Passphrases are masked. **View** opens
received books separately; `Esc` returns to your own books. Received copies never
enter your own consolidated balances. The CLI exposes the same actions through
`means share list` and `means share --help`.

## Set up identities

Each collaborator runs these commands against their own installation:

```sh
means share identity-create /safe/identity.age /backup/identity.age
means share public collaborator-public.json
```

The passphrase is prompted without echo and must contain at least 12 characters.
Keep the encrypted backup somewhere separate and keep the passphrase recoverable;
there is no reset service. Creation decrypts the backup and verifies its fingerprint.
Neither private keys nor passphrases belong in command arguments or exchanged files.

Exchange the public identity files. Compare the **entire fingerprint** over an
independent trusted channel, then pin the verified identity:

```sh
means share pin collaborator-public.json FULL_VERIFIED_FINGERPRINT
```

Both parties pin each other before exchanging snapshots. File contents alone do
not prove the identity of the person who sent them.

## Grant, export, import and acknowledge

The owner previews the full scope for entity `1`:

```sh
means share grant 1 RECIPIENT_FINGERPRINT 2027-01-01T00:00:00Z
```

Review the vault, recipient, validity and record counts. Repeat the command with
`--confirm TOKEN` from the preview to create the grant. Changing the scope makes
an old token invalid. Use the returned grant ID:

```sh
means share export GRANT_ID snapshot-1.age
```

Send the encrypted file through your chosen channel. On the recipient’s machine:

```sh
means share import snapshot-1.age OWNER_FINGERPRINT receipt-1.jws
means share view GRANT_ID
```

Return the small signed receipt to the owner:

```sh
means share acknowledge GRANT_ID receipt-1.jws
```

The next export then advances the revision. Before acknowledgement, exporting
again returns the same pending bundle. Reimporting an accepted bundle regenerates
its receipt without replacing the copy. Older, conflicting or skipped revisions
are rejected. Use a new output filename for every new revision; commands refuse
to overwrite files with different contents.

Received vaults live in their own SQLite files under `sharing/` beside the ledger.
The trust store there records pins and accepted checkpoints. Back up this directory
as well as your identity. Do not replace trust state with an older backup to bypass
a revision error. Old snapshots and outbound files are retained; automatic cleanup
is intentionally absent. Never delete the active snapshot or pending outbound file.
The CLI can use `--store DIRECTORY`; the TUI uses the directory beside its server’s
ledger.

## Expiry, revocation and recovery

Expiry prevents accepting new snapshots. The owner can preview and confirm
revocation, which immediately stops new snapshot exports:

```sh
means share revoke GRANT_ID
means share revoke GRANT_ID --confirm TOKEN
means share revocation GRANT_ID revoked.age
```

The recipient imports the notice like a snapshot and returns its receipt. A notice
can be sent after the preceding delivery is acknowledged. Previously received
copies stay readable, labeled **expired** or **revoked**. A recipient learns of
revocation only after importing the notice; offline delivery cannot erase copies.

Restore an encrypted identity into an installation without a configured identity:

```sh
means share identity-restore /backup/identity.age FULL_VERIFIED_FINGERPRINT
```

If only the configured identity file was lost, restore the identical encrypted
backup to that original path; retain the existing trust directory and checkpoints.
Loss of both the identity and its backup, or of the passphrase, requires a new
identity and replacement grants. Losing recipient trust state requires a new grant,
not importing an old stream as if it were new.

An owner with the old key can rotate it while preserving grant streams. Every
pending delivery must first have its signed receipt acknowledged:

```sh
means share rotate /safe/new-identity.age /backup/new-identity.age rotation.jws
means share public new-owner-public.json
```

Deliver the signed proof and verify the new fingerprint independently. Recipients:

```sh
means share accept-rotation ORIGINAL_OWNER_FINGERPRINT rotation.jws NEW_FINGERPRINT
```

The original owner fingerprint remains the stream namespace. A recipient key
change always requires a replacement grant; existing grants address the old key.

The API remains local-only. The received-vault viewer binds only to loopback,
rejects every mutation RPC and never runs inbox watching or bank jobs. See the
[wire specification](vault-sharing-wire-v1.md) for validation and trust boundaries.
