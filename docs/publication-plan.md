# Future public release

Status: planning only. Do not rewrite history, change repository visibility, or publish backups as part of normal development.

The intended public shape is one reviewed `main` branch with one initial commit. Preserve the original repository and tracker in private backups first.

## Back up before preparing a public tree

Choose a private backup destination outside the repository. Use encrypted storage. Backups can contain private data even if the eventual release does not.

1. Pause code and tracker writes for a consistent backup window.
2. Record all local refs and remote refs. Include branches, tags, notes, and any `refs/dolt/*` refs.
3. Make a Git bundle of **all local refs**, not only `main`. Record its checksum. `git bundle verify` checks the bundle, but a restore rehearsal must also compare the ref inventory and commit IDs.
4. Make a separate private mirror of the remote. This captures refs present remotely but absent locally. Do not push that mirror to a public destination.
5. Back up the live Beads store with `bd backup init <private-path>` and `bd backup sync` in its actual runtime. Beads can run in a container or use an external Dolt server. The backup path must be a persistent mounted location. `bd backup status` reports its state.
6. Preserve tracker configuration and any uncommitted working-set data. The native Beads backup is meant to include tables, Dolt branches, history, and working sets. Do not substitute `.beads/issues.jsonl` for the database backup.
7. Restore code and Beads into isolated temporary locations. Compare refs, issue counts, and representative issue records. Record checksums and the restore results privately.

A Git bundle includes only objects reachable through the refs selected for that bundle. It does not save untracked files, the working tree, or a live external Dolt database. A Git remote with no `refs/dolt/data` is not proof that no tracker exists.

## Prepare a clean snapshot

Use a separate staging directory. Leave the private working repository and its branches intact.

Export a reviewed source tree without `.git`. Exclude private tracker data, interaction logs, machine-specific agent configuration, personal remap plans, local databases, bank exports, build artifacts, and audit reports. Review the remaining documentation and test fixtures. Use synthetic names, account identifiers, and transactions.

Keep the Apache-2.0 license and required third-party notices. Preserve any applicable attribution. Confirm that the public commit author identity is the one the maintainer intends to expose.

Run the build, tests, link checks, and a secret scan against this exact staged tree. Inspect the generated website as well as its sources. Re-run these checks if the staged tree changes.

## Check the current tree

Run `python3 scripts/check-public-tree.py` to catch tracked local configuration,
private database artifacts, and host-specific home paths. Run
`python3 charts/validate_charts.py` to validate the generic chart examples.
Review test fixture contents and run a secret scan separately.

Private-backup tests require an explicit `MEANS_ATB` path and `--ignored`.
Ordinary tests use synthetic data. Public test keys support local mock servers;
see the fixture READMEs for their scope.

These checks cover the working tree. Existing Git history and private local
archives still need the separate backup and clean-snapshot procedure below.

## Create the public repository later

After the owner approves the reviewed snapshot:

1. Initialize a new Git repository in the staging directory.
2. Create `main` with one initial commit from the approved tree.
3. Verify that the repository contains exactly that intended history and no private refs.
4. Push only `main` to a new empty public remote. Do not use `--mirror`, `--all`, or a tracker sync command against it.
5. Configure public CI, documentation hosting, and a public issue workflow separately.

A new public remote is the cleanest boundary. Reusing the old host repository may retain pull-request refs, cached commit pages, release assets, CI artifacts, or other data outside normal branch history. A force-push is not a privacy eraser. Review host-retained data before changing the existing repository's visibility.

No backup, history rewrite, force-push, or visibility change is authorized by this plan alone.
