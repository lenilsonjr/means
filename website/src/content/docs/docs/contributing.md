---
title: Contribute to means
description: Work on the accounting core, terminal UI, connectors, or documentation.
---

Contributions to the accounting core, terminal UI, connectors, and documentation are welcome. The code and documentation use the [Apache License 2.0](https://github.com/lenilsonjr/means/blob/main/LICENSE).

Start with a small, reproducible problem. For a connector issue, include a redacted sample and the expected result. For a terminal issue, include the launch command, screen, and key sequence. Use synthetic data and remove credentials and personal details from every sample.

## Repository structure

| Path                   | Purpose                                     |
| ---------------------- | ------------------------------------------- |
| `crates/means-core`    | Accounting, valuation, imports, and reports |
| `crates/means-proto`   | Generated protobuf types                    |
| `crates/means-server`  | CLI and local gRPC service                  |
| `crates/means-tui`     | Ratatui terminal client                     |
| `crates/means-sharing` | Encrypted and signed vault exchange         |
| `proto/`               | Shared API contract                         |
| `charts/`              | Account chart templates                     |
| `docs/`                | Maintained guides and design records        |
| `website/`             | Documentation website                       |

Read the [accounting model](../guides/data-model/) before changing booking behavior. Preserve native amounts, functional book values, and statement evidence. Use synthetic fixtures in tests.

## Run the Rust checks

Install stable Rust and `protoc`, then run:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests use synthetic fixtures or temporary ledgers. Some start local mock servers. Private-backup checks are ignored by default and require both `--ignored` and an explicit `MEANS_ATB` path.

## Work on the docs site

Use Node.js 22.12 or newer. Node.js 24 LTS is the recommended build environment.

```sh
cd website
npm ci
npm run dev
```

The development server prints its local address. For production checks:

```sh
npm run check
npm run build
npx playwright install chromium
npm test
```

The site prepares existing guides from `docs/` before each development or production build. Edit those source files to update the generated pages. New introductory pages live in `website/src/content/docs/docs/`.

See the [website README](https://github.com/lenilsonjr/means/blob/main/website/README.md) for deployment and base-path configuration.

## Submit a change

Describe the problem, the resulting behavior, and the checks you ran. Keep unrelated ledger changes out of the patch. Include tests for accounting and import behavior.

Project maintainers use Beads for durable task tracking. Follow the current repository instructions for tracker access. Propose changes through [GitHub](https://github.com/lenilsonjr/means).

## Acknowledgements

Thank you to Account Tracker for the care put into personal finance software and for the inspiration it gave means. Its backup export provides a path for users to bring their existing books into means.

means also builds on the work of these open-source communities:

- **Rust and Cargo**, for the language and development tools.
- **SQLite**, for local, portable ledger storage.
- **Ratatui and Crossterm**, for the terminal interface.
- **Tokio, Tonic, and Prost**, for the local service and typed protocol.
- **age and the Rust cryptography ecosystem**, for encrypted vault exchange.
- **Beancount and Fava**, for accounting tools that help users inspect exported books.
- **Astro, Starlight, Pagefind, and xterm.js**, for this documentation site, search, and terminal recordings.

Thank you to everyone who reports bugs, contributes statement formats, reviews accounting behavior, and improves the documentation. The site’s [third-party notices](../../third-party-notices.txt) contain the bundled website and font license notices.
