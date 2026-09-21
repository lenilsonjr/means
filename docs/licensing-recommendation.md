# License decision

Status: adopted on 2026-09-20 by the project owner.

## Decision

means code and original documentation use **Apache-2.0**.
The repository includes the [full license text](../LICENSE).
All five Rust crates inherit the workspace license.

means is a local accounting tool with a reusable Rust core. A permissive license
supports personal use, business use, and integration with other tools. Apache-2.0
adds an explicit contributor patent grant. It also defines when that grant ends
after patent litigation. It permits closed derivatives. It does not require a
hosted fork to publish its changes. These are useful terms if adoption is the
main goal. See the [Apache license](https://www.apache.org/licenses/LICENSE-2.0).

If the goal is to require hosted forks to share their changes, choose AGPL-3.0
instead. That is a product policy decision. Do not select it only because means
has a server process. The current server is local-only.

## State before this decision

The root `Cargo.toml` declared `license = "MIT"` under `[workspace.package]`.
No repository `LICENSE` file was found. All five member crates omitted
`license.workspace = true`. Cargo metadata therefore reported no license for those
packages. These metadata gaps are now fixed. Members must opt into shared metadata. See the
[Cargo workspace reference](https://doc.rust-lang.org/cargo/reference/workspaces.html#the-package-table).

The existing MIT declaration must be considered before a change. This review did
not establish copyright ownership or contribution rights. A new license does not
remove rights already granted for earlier copies.

## Options

| License | Main benefit | Main tradeoff |
| --- | --- | --- |
| MIT | Short terms. Permits commercial reuse and closed derivatives. Preserves the current declared choice. | No explicit patent grant in its text. Recipients must retain the copyright and permission notice. |
| Apache-2.0 | Permissive reuse with explicit patent terms. | More notice requirements. Closed forks can keep their changes private. |
| GPL-3.0-only | Requires corresponding source when covered binaries are distributed, subject to its terms. | More obligations for combined distributed software. Network use alone does not add an AGPL-style source offer. |
| AGPL-3.0-only | Adds a source offer for users who interact with a modified version over a network. | More obligations for hosted integrations. It does not prohibit commercial use. |

Sources: [MIT text](https://opensource.org/license/mit),
[Apache text](https://www.apache.org/licenses/LICENSE-2.0),
[GPLv3 text](https://www.gnu.org/licenses/gpl.en.html), and
[GNU explanation of AGPL](https://www.gnu.org/licenses/why-affero-gpl.en.html).

`MIT OR Apache-2.0` is another option. Recipients can choose either license.
Use it if that choice is an explicit goal. It does not make the Apache conditions
apply to a recipient who chooses MIT.

## Dependency findings

The review used `cargo metadata --locked --offline --format-version 1` on
2026-09-20. It returned 483 packages: five means crates and 478 external packages.
Every external package had a license declaration. No declaration required a
GPL-only or AGPL-only choice. For example, `self_cell` offers Apache-2.0 as an
alternative to GPL-2.0-only.

Most declarations offer MIT or Apache-2.0. Other terms still matter:

- `ring 0.17.14` declares `Apache-2.0 AND ISC`.
- `encoding_rs 0.8.41` declares `(Apache-2.0 OR MIT) AND BSD-3-Clause`.
- `matchit 0.8.4` declares `MIT AND BSD-3-Clause`.
- `ed25519-dalek 2.2.0` declares `BSD-3-Clause`.
- `webpki-roots 1.0.9` declares `CDLA-Permissive-2.0`.
- ICU packages declare `Unicode-3.0`.

These declarations show no obvious reason to require copyleft for means. This is
a metadata review, not complete clearance of a release binary. It includes
packages for other targets and build tools. It does not verify all embedded
source, native libraries, fonts, or future site dependencies. SQLite and TLS
packaging need review for each distributed target. Keep the dependency licenses
and required notices in release distributions.

## Release scope

This change adds the official license text, updates Cargo metadata, and states
the license in the README. It does not establish copyright ownership or complete
a dependency compliance review. Keep existing third-party notices. Check license
files for each release target before distributing binaries.
