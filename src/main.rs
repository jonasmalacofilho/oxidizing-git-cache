use std::io::Result;

use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::EnvFilter;

use clap::Parser;

use git_cache_http_server::{start, Options};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE) // FIXME only if level >= debug or trace
        .compact()
        .init();

    let options = Options::parse();

    start(&options).await
}
