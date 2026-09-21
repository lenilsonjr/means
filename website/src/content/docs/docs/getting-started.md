---
title: Installation & first run
description: Build means, create a personal vault, and start the local terminal UI.
---

## Install the prerequisites

Install a recent stable [Rust toolchain](https://www.rust-lang.org/tools/install), Git, and the [Protocol Buffers compiler](https://protobuf.dev/installation/) (`protoc`).

Check that the tools are on your path:

```sh
cargo --version
protoc --version
```

## Build from source

```sh
git clone https://github.com/lenilsonjr/means.git
cd means
cargo install --path crates/means-server --locked
means --help
```

Cargo normally installs the binary in `~/.cargo/bin`. Add that directory to your `PATH` if `means` is not found.

There is no published binary download linked here. These instructions build the version in your checkout.

## Create your first vault

This example creates a USD personal vault in a new database. Run it from the repository root so the chart file is available.

```sh
means --db ./demo.db entity add Personal --currency USD --country US
means --db ./demo.db chart apply --entity Personal charts/personal-nomad.json
means --db ./demo.db status
```

Choose your accounting currency when you create the vault. Individual accounts can use other currencies. Read [core concepts](../concepts/) before you import a large history.

## Start the local server

In one terminal, run:

```sh
means --db ./demo.db serve --listen 127.0.0.1:7771 --inbox ./demo-inbox
```

Keep that terminal open. In another terminal, run:

```sh
means tui --server http://127.0.0.1:7771
```

The TUI uses the server's database. Its `--server` address must match the server you started. CLI commands need the same `--db` path when you work on that ledger.

Without an explicit path, means uses `~/.means/ledger.db`. `MEANS_DB` also selects the database. The default TUI server address is `http://127.0.0.1:7770`.

## Bring one account

Start with a statement for one account. In the TUI, open **Imports** with `6`, then press `i` to select a file. Select the destination account and inspect the import preview.

For a bank API, open **Connections** with `8`. Follow the [provider setup guide](../guides/connections/). Credentials must be available to the process running `means serve`.

For foreign-currency history, fetch rates for the period you need:

```sh
means --db ./demo.db rates fetch --from 2026-01-01
```

Use the start date that fits your own records. Then open **Review** with `4` to inspect new entries. See [terminal workflows](../terminal/) for posting and splitting.

## Update the binary you launch

```sh
git pull
cargo install --path crates/means-server --locked --force
command -v means
```

Restart both the server and TUI. Keep the same database and server settings.

`cargo build` updates `target/debug/means`. It does **not** replace an installed `means` command. To test a debug build directly, run `./target/debug/means`.

## Start with a company

The same commands work for companies. These examples use fictional names and one shared operating chart:

```sh
means --db ./demo.db entity add 'Example Studio Pte. Ltd.' --kind company --country SG --currency SGD
means --db ./demo.db chart apply --entity 'Example Studio Pte. Ltd.' charts/company-services.json
means --db ./demo.db entity add 'Example Robotics Inc.' --kind company --country US --currency USD
means --db ./demo.db chart apply --entity 'Example Robotics Inc.' charts/company-services.json
means --db ./demo.db entity add 'Example Software OÜ' --kind company --country EE --currency EUR
means --db ./demo.db chart apply --entity 'Example Software OÜ' charts/company-services.json
```

Each vault has its own chart and balances. Add bank accounts in their native currencies, then set up imports for each account. The company chart is a general starting point; adapt it to the company's reporting needs.
