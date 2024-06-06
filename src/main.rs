use std::io::{self, Result};

use clap::Parser;
//use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::EnvFilter;

use git_cache_http_server::server::{start, Options};

#[tokio::main]
async fn main() -> Result<()> {
    // Follow/assume tracing-journald mapping from `Level` to journald/syslog priorities:
    // - `INFO`  => Notice (5)        => normal, but significant, condition (unusual, but not errors)
    // - `DEBUG` => Informational (6) => information message (normal operational messages)
    // - `TRACE` => Debug (7)         => debug-level message (may need to be enabled)
    // Therefore, default to `DEBUG`.

    // TODO: log span new/close events without unnecessary verboseness in INFO
    // TODO: more compact output (may require changing how request ID and headers are logged)
    // TODO: enable specific support journald
    tracing_subscriber::fmt::fmt()
        .with_env_filter(
            EnvFilter::try_from_env("GIT_CACHE_LOG").unwrap_or_else(|_| EnvFilter::new("debug")),
        )
        //.with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .with_writer(io::stderr)
        .pretty()
        .init();

    let options = Options::parse();

    start(&options).await
}
