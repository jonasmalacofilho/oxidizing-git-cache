mod error;
mod git;
mod repo;
pub mod server;

const APP_NAME: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
