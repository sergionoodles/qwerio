<p align="center">
  <img src="resources/favicon.png" alt="Qwerio logo" width="112">
</p>

# Qwerio

**Your databases. Your queries. That note explaining why the column is called `thing2`.**

Qwerio is a desktop database workspace for PostgreSQL and SQLite, built with Rust
and GPUI for Linux and macOS. It feels like a file manager: browse tables, follow
relationships, and keep SQL files and Markdown notes right beside the data they
explain.

For the days when you just want to find a row, answer a question, and get on with
building something. No enterprise sales call required.

## Make yourself at home

- **Poke around.** Browse tables and views, filter and sort rows, inspect records,
  and follow foreign keys when one table inevitably leads to another.
- **Keep the context.** Put queries and notes in ordinary workspace folders, and
  link documents to the database objects they belong to.
- **Write some SQL.** Choose a connection, run statements, inspect results, and
  manage transactions with explicit commit and rollback controls.
- **Spread out.** Tabs, split panes, back/forward navigation, and light or dark
  themes for whichever hour this debugging session has reached.

Browsing starts read-only. Writable SQL sessions are an explicit choice.

Still early and actively taking shape. Expect a few rough edges; bring curiosity.

## Take it for a spin

You'll need [Rust via rustup](https://rustup.rs/) and the native build tools for
your platform. The repo pins its Rust toolchain. On Linux, you'll also need working
Vulkan drivers and the development libraries listed in the
[build workflow](.github/workflows/build-apps.yml).

```sh
cargo run --package qwerio-desktop
```

Open a workspace in the app, or pass a folder directly:

```sh
cargo run --package qwerio-desktop -- /path/to/workspace
```

Working on Qwerio? Run `scripts/check.sh` for formatting, Clippy, and tests. The
test helper starts a disposable local PostgreSQL server; it needs `initdb`,
`pg_ctl`, and `openssl` on your `PATH`.

## Running on a server over SSH

Download and unpack the SSH package in your home directory on the server,
replacing `X.Y.Z` with the release version:

```sh
curl -LO https://github.com/sergionoodles/qwerio/releases/download/vX.Y.Z/qwerio-linux-over-ssh-X.Y.Z-x86_64.tar.gz
tar xzf qwerio-linux-over-ssh-*.tar.gz
# then, from kitty or Ghostty on your machine:
ssh -t <user>@<host> '~/qwerio-linux-over-ssh-X.Y.Z-x86_64/qwerio --tui'
```

## License

[MIT](LICENSE). Go make something useful. Or investigate `thing2`.
