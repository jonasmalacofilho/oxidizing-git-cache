use std::io::{self, Result};

use clap::Parser;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::EnvFilter;

use git_cache_http_server::server::{start, Options};

#[tokio::main]
async fn main() -> Result<()> {
    // The default logging/tracing level is `INFO`. Note that `tracing_journald`, if in use, would
    // translate `Level::INFO` to syslog `Notice`; syslog `Informational` would require
    // `Level::DEBUG`.
    tracing_subscriber::fmt::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("GIT_CACHE_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .with_writer(io::stderr)
        .with_target(false)
        .compact()
        .init();

    let options = Options::parse();

    start(&options).await
}
