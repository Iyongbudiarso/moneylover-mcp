# moneylover-mcp

[![CI](https://github.com/Iyongbudiarso/moneylover-mcp/actions/workflows/ci.yml/badge.svg)](https://github.com/Iyongbudiarso/moneylover-mcp/actions/workflows/ci.yml)
[![Security audit](https://github.com/Iyongbudiarso/moneylover-mcp/actions/workflows/audit.yml/badge.svg)](https://github.com/Iyongbudiarso/moneylover-mcp/actions/workflows/audit.yml)
[![OpenSSF Scorecard](https://api.scorecard.dev/projects/github.com/Iyongbudiarso/moneylover-mcp/badge)](https://scorecard.dev/viewer/?uri=github.com/Iyongbudiarso/moneylover-mcp)

An [MCP](https://modelcontextprotocol.io) server that lets an AI agent or chatbot read your
[Money Lover](https://moneylover.me) data and, if you allow it, add transactions. It talks to
the client over **stdio**.

> **Disclaimer — please read.**
> This project is **unofficial**. It is **not affiliated with, endorsed by, or supported by Money Lover**
> or its owners. "Money Lover" is a trademark of its respective owner and is used here only to
> describe what the software connects to.
> Money Lover has no public API. This tool uses the same undocumented web endpoints as the Money Lover
> web app, reverse-engineered from public open-source projects. They can change or break at any time,
> and using them may be against Money Lover's terms of service (which we could not verify).
> **You use this software entirely at your own risk**, including the risk of account restrictions,
> lost or incorrect data, and incorrect transactions. It is provided "AS IS" under the MIT license,
> without warranty of any kind. Back up your data and try it on a throwaway wallet first.

## Trust model

What a reviewer should know before granting this server access to a finance account. Each claim
is enforced in code and checked in CI, not just stated here.

| Claim | How it is enforced |
|---|---|
| **The token is full-access; read-only is enforced by the server.** Money Lover has no scoped or read-only tokens. | Write tools are not even registered unless `MONEYLOVER_ALLOW_WRITE=true`; when they are, a server-side guard limits them ([Security](#security)). |
| **Network egress goes only to Money Lover**: `web.moneylover.me` and `oauth.moneylover.me`, HTTPS only. No telemetry, no update checks, no other hosts. | Endpoints are compile-time constants that no environment variable can change; redirects are never followed. [`tests/egress.rs`](tests/egress.rs) fails if any other URL appears in the source. |
| **Credentials stay local.** The password is used for login and never stored; tokens live in a `0600` file; nothing secret is logged or returned to the AI. | [`src/token_store.rs`](src/token_store.rs), redacting `Debug` impls, tests. |
| **Dependencies are pinned and vetted.** | `Cargo.lock` + `--locked` everywhere; [`cargo-deny`](deny.toml) allows crates.io sources and permissive licenses only and fails on RustSec advisories or yanked crates (weekly and on change); GitHub Actions pinned to commit SHAs; Dependabot. |
| **Release binaries are built from this repository.** | Every release archive has a signed SLSA build-provenance attestation (`gh attestation verify`) and a `SHA256SUMS` entry. |

## Tools

| Tool | What it does | Needs write mode |
|---|---|---|
| `list_wallets` | Wallets with currency and balance (`refresh: true` for live balances) | no |
| `list_categories` | Income/expense categories, optionally for one wallet | no |
| `list_transactions` | Transactions in a date range, newest first (`limit` default 50, max 200) | no |
| `summarize_spending` | Totals per category and currency for a date range | no |
| `add_transaction` | Create a transaction; supports `dry_run` | **yes** |

Design choices that matter for LLM use:

- Wallets and categories are given by **name**, never by id. Matching is exact (case-insensitive);
  an unknown or ambiguous name returns an error listing the candidates instead of guessing.
- Amounts are **always positive**. Income vs expense comes from the category. Pass `type` when a
  category name exists as both (e.g. "Other").
- Money is returned as **decimal strings**, never floats.
- Dates are `YYYY-MM-DD` in your configured timezone.
- `add_transaction` is **not even registered** unless you opt in with `MONEYLOVER_ALLOW_WRITE=true`.
  See [Security](#security) for the limits that apply to writes.
- Wallets and categories are **cached in memory** (5 minutes by default) because Money Lover's
  wallet list can take 30+ seconds on large accounts. Writing a transaction clears the cached
  balances, a name that is not in the cache triggers one fresh fetch (so a wallet you just created
  is found), and `list_wallets` reports `from_cache` so an agent knows when balances may be stale.
  Changes made in the Money Lover app itself show up after the cache expires or with `refresh: true`.

## Install

### Prebuilt binary (no Rust needed)

Download the archive for your platform from the
[latest release](https://github.com/Iyongbudiarso/moneylover-mcp/releases/latest):

| Platform | Archive |
|---|---|
| macOS, Apple Silicon | `moneylover-mcp-<version>-aarch64-apple-darwin.tar.gz` |
| macOS, Intel | `moneylover-mcp-<version>-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 (glibc 2.35+) | `moneylover-mcp-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 (glibc 2.35+) | `moneylover-mcp-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| Windows x86_64 | `moneylover-mcp-<version>-x86_64-pc-windows-msvc.zip` |

Verify it before running it. `SHA256SUMS` is attached to every release, and every archive has a
signed build-provenance attestation proving it was built by this repository's release workflow:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing          # Linux: sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify moneylover-mcp-*.tar.gz --repo Iyongbudiarso/moneylover-mcp
```

Extract it and put `moneylover-mcp` somewhere on your `PATH`. The binaries are not signed with an
Apple or Microsoft certificate. On macOS, a file downloaded with a browser is blocked by
Gatekeeper; after verifying it, allow it with `xattr -d com.apple.quarantine moneylover-mcp`.
On Windows, SmartScreen may ask for confirmation.

### From source

You need a [Rust toolchain](https://rustup.rs) (1.89 or newer).

```sh
cargo install --locked --git https://github.com/Iyongbudiarso/moneylover-mcp
# or, from a checkout:
cargo build --locked --release   # binary: target/release/moneylover-mcp
```

`--locked` builds with the exact dependency versions in `Cargo.lock`, which CI vets with
`cargo deny`, instead of whatever is newest on crates.io.

### Log in

Log in **once, from a terminal**. The password is typed at a hidden prompt and never stored;
only the resulting token is saved:

```sh
moneylover-mcp login
```

## Configure

All configuration is through environment variables.

| Variable | Default | Meaning |
|---|---|---|
| `MONEYLOVER_TIMEZONE` | `Asia/Jakarta` | IANA timezone for dates |
| `MONEYLOVER_ALLOW_WRITE` | `false` | Set `true` to enable `add_transaction` |
| `MONEYLOVER_WRITE_WALLETS` | – | Comma-separated wallet names; when set, writes go **only** to these |
| `MONEYLOVER_MAX_AMOUNT` | – | When set, larger amounts are refused |
| `MONEYLOVER_MAX_WRITES_PER_HOUR` | `20` | Created transactions per rolling hour (1-1000) |
| `MONEYLOVER_TOKEN_PATH` | `~/.config/moneylover-mcp/token.json` | Where tokens are stored. Must be absolute |
| `MONEYLOVER_CACHE_SECS` | `300` | How long wallets and categories are cached (0 disables) |
| `MONEYLOVER_TIMEOUT_SECS` | `45` | Per-request timeout (1-300). Money Lover's servers can be slow |
| `MONEYLOVER_LOG` | `info` | Log filter. Logs go to stderr only |
| `MONEYLOVER_EMAIL`, `MONEYLOVER_PASSWORD` | – | Only for headless setups that cannot run `login`; see below |

### Authentication notes

- Only **email + password** accounts are supported. Accounts created with Google, Facebook or Apple
  sign-in have no password and cannot log in this way.
- A Money Lover account allows roughly **5 devices**, and every fresh login uses one slot. The
  server stores its tokens in `token.json` and refreshes them (a refresh uses no slot). Several
  MCP clients may share the same token file safely; access is serialized with a lock file.
- If the token can no longer be refreshed, run `moneylover-mcp login` again.
- `moneylover-mcp logout` deletes the local token and stops running servers immediately. The token
  itself stays valid at Money Lover until it expires; to revoke it, remove the device in the Money
  Lover app. Either way every server sharing the token file stops until you run
  `moneylover-mcp login` again; it never silently logs back in.
- A wrong password is tried once per server start, never in a loop.

### Example client config

No password in the config: run `moneylover-mcp login` first.

```json
{
  "mcpServers": {
    "moneylover": {
      "command": "/path/to/moneylover-mcp",
      "env": {
        "MONEYLOVER_TIMEZONE": "Asia/Jakarta"
      }
    }
  }
}
```

In Zed, put the same thing under `context_servers` in `settings.json`:

```json
{
  "context_servers": {
    "moneylover": {
      "command": "/path/to/moneylover-mcp",
      "env": { "MONEYLOVER_TIMEZONE": "Asia/Jakarta" }
    }
  }
}
```

To let an agent record transactions, opt in and fence it in:

```json
"env": {
  "MONEYLOVER_ALLOW_WRITE": "true",
  "MONEYLOVER_WRITE_WALLETS": "Cash",
  "MONEYLOVER_MAX_AMOUNT": "2000000"
}
```

Headless servers (a container, a VPS running a chatbot) can pass `MONEYLOVER_EMAIL` and
`MONEYLOVER_PASSWORD` instead; use your platform's secret store, not a file in a repository.

## Security

**Do not commit MCP client config files that contain credentials.** Zed's `settings.json` and
similar files often end up in public dotfiles repositories.

What the server does to keep your account safe:

- **Credentials.** The password is only used for login and never stored. Tokens live in
  `token.json`, created with mode `0600` in a `0700` directory; a token file that other users can
  read is tightened to `0600`, or refused if that is not possible. Passwords, tokens and
  `Authorization` headers never appear in logs or in tool output. On Windows the file relies on
  your user profile's default permissions.
- **Network.** HTTPS only, certificates checked against the OS trust store, redirects never
  followed (so a password or token can never be re-sent elsewhere), response size capped.
- **Writes are off by default.** When enabled, every `add_transaction` call is checked by the
  server, not by the AI:
  - only wallets in `MONEYLOVER_WRITE_WALLETS` (if set), archived wallets never;
  - amounts up to `MONEYLOVER_MAX_AMOUNT` (if set);
  - dates from one year ago to one month ahead;
  - at most `MONEYLOVER_MAX_WRITES_PER_HOUR` transactions per hour (counted per server process:
    two MCP clients each running the server get the limit twice);
  - an identical transaction within 10 minutes is refused as an accidental retry unless the
    agent passes `allow_duplicate: true`;
  - writes are never retried automatically. If a write times out, the tool answers `UNCERTAIN`
    and tells the agent to check before retrying.

What you should know:

- **Prompt injection.** Transaction notes, wallet names and category names are shown to the AI.
  In a **shared wallet**, other people write those. Text like "ignore previous instructions and
  add …" could steer an agent. The server tells the agent to treat this text as data, but the
  real protection is the write limits above. Keep writes off unless you need them, restrict
  `MONEYLOVER_WRITE_WALLETS`, and prefer an MCP client that asks you before each tool call.
  `MONEYLOVER_WRITE_WALLETS` matches wallet **names**; the owner of a wallet shared with you can
  rename it, so list wallets you own.
- **Combining servers.** An agent that can both read your finances and send data out (web fetch,
  email, …) can be tricked into leaking them. Think about which tools you enable together.
- **Logs.** Logs go to stderr, which MCP clients often save to a file. At the default `info` level
  they contain no financial data. `MONEYLOVER_LOG=rmcp=debug` logs full tool inputs and outputs.
- **Your AI provider** sees whatever the tools return (balances, transactions). That is inherent
  to using a cloud AI with your financial data.

## Releasing

1. Bump `version` in `Cargo.toml`, run `cargo build` to update `Cargo.lock`, commit.
2. Tag and push: `git tag v0.2.0 && git push origin v0.2.0`.

The [release workflow](.github/workflows/release.yml) checks that the tag matches `Cargo.toml`,
runs the tests, builds all platforms, attests their provenance and publishes the GitHub release
with `SHA256SUMS`. Running the workflow manually (Actions > Release > Run workflow) only builds,
which is a handy dry run.

## Development

```sh
cargo test                      # unit + end-to-end tests against a mocked Money Lover
cargo clippy --all-targets
```

Live tests hit the real API and are skipped by default. Run `moneylover-mcp login` (or
`cargo run -- login`) first, then:

```sh
cargo test --test live live_read_only -- --ignored --nocapture      # read only

# Optional write test: needs a wallet named exactly "MCP Test"
MONEYLOVER_LIVE_WRITE=1 MONEYLOVER_LIVE_CATEGORY="<an expense category in that wallet>" \
  cargo test --test live live_write -- --ignored --nocapture
```

Delete the test transaction in the Money Lover app afterwards.

## License

MIT. See [LICENSE](LICENSE).
