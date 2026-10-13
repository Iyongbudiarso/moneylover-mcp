use std::{
    io::{self, BufRead, Write},
    sync::Arc,
};

use anyhow::{Context, bail};
use moneylover_mcp::{
    client::MoneyLover,
    config::Config,
    token_store::{delete_tokens, display_path, load_tokens, mark_revoked},
    tools::MoneyLoverServer,
};
use rmcp::{ServiceExt, transport::stdio};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
moneylover-mcp - unofficial MCP server for Money Lover (stdio)

USAGE:
    moneylover-mcp           Run the MCP server (this is what MCP clients start)
    moneylover-mcp login     Log in once from a terminal and store the token,
                             so no password has to be put in any config file
    moneylover-mcp logout    Delete the stored token
    moneylover-mcp --version

Configuration is read from MONEYLOVER_* environment variables; see README.md.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();
    let command = std::env::args().nth(1);
    match command.as_deref() {
        None => serve().await,
        Some("login") => login().await,
        Some("logout") => logout(),
        Some("-V" | "--version") => {
            println!("moneylover-mcp {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("unknown command '{other}'\n\n{USAGE}"),
    }
}

/// stdout carries the MCP protocol, so every log line goes to stderr. rmcp logs full
/// tool arguments and results (balances, notes) at debug level, so it stays at info
/// unless the user asks for rmcp logs by name.
fn init_logging() {
    let mut directives = std::env::var("MONEYLOVER_LOG").unwrap_or_else(|_| "info".into());
    if !directives.contains("rmcp") {
        directives.push_str(",rmcp=info");
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_new(&directives).unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(io::stderr)
        .with_ansi(false)
        .init();
}

async fn serve() -> anyhow::Result<()> {
    let config = Config::from_env().context("invalid configuration")?;
    tracing::info!(
        "starting moneylover-mcp {} (write tools {})",
        env!("CARGO_PKG_VERSION"),
        if config.allow_write {
            "ENABLED"
        } else {
            "disabled"
        }
    );
    let client = Arc::new(MoneyLover::new(config)?);
    let service = MoneyLoverServer::new(client)
        .serve(stdio())
        .await
        .context("failed to start the MCP server")?;
    service.waiting().await?;
    Ok(())
}

async fn login() -> anyhow::Result<()> {
    let mut config = Config::from_env().context("invalid configuration")?;
    let token_path = display_path(&config.token_path);
    // An unreadable or untrusted token file is exactly what a fresh login replaces.
    if matches!(load_tokens(&config.token_path), Ok(Some(_))) {
        eprintln!("A token is already stored at {token_path}.");
        eprintln!("Logging in again uses another Money Lover device slot (about 5 per account).");
        if !confirm("Continue? [y/N] ")? {
            return Ok(());
        }
    }
    if config.email.is_none() {
        config.email = Some(prompt("Money Lover email: ")?);
    }
    if config.password.is_none() {
        config.password = Some(rpassword::prompt_password("Money Lover password: ")?);
    }
    MoneyLover::new(config)?.log_in().await?;
    eprintln!("Logged in. Token stored at {token_path}.");
    eprintln!("Your MCP client config no longer needs MONEYLOVER_PASSWORD.");
    Ok(())
}

fn logout() -> anyhow::Result<()> {
    let config = Config::from_env().context("invalid configuration")?;
    // The marker also stops server processes that are running right now.
    mark_revoked(&config.token_path);
    delete_tokens(&config.token_path);
    eprintln!(
        "Signed out: deleted {}. Running servers stop using it immediately.",
        display_path(&config.token_path)
    );
    eprintln!(
        "The token itself stays valid at Money Lover until it expires; remove this device \
         in the Money Lover app if you want to revoke it now."
    );
    Ok(())
}

fn prompt(question: &str) -> anyhow::Result<String> {
    eprint!("{question}");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    let answer = answer.trim().to_string();
    if answer.is_empty() {
        bail!("no input given");
    }
    Ok(answer)
}

fn confirm(question: &str) -> anyhow::Result<bool> {
    Ok(prompt(question).is_ok_and(|a| a.eq_ignore_ascii_case("y")))
}
