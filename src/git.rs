use std::io::Result;
use std::path::PathBuf;
use std::process::{Output, Stdio};

use axum::http::Uri;
use tokio::io::AsyncRead;
use tokio::process::Command;

#[cfg(test)]
use mockall::automock;

#[derive(Default, Debug)]
pub struct Git {}

#[cfg_attr(test, automock, allow(dead_code))]
impl Git {
    pub async fn init(&self, local: PathBuf) -> Result<Output> {
        let child = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg("--bare")
            .arg(local)
            .spawn()?;
        child.wait_with_output().await
    }

    pub async fn fetch(&self, upstream: Uri, local: PathBuf) -> Result<Output> {
        // TODO: set up authentication
        // TODO: store stdout/stderr and log/return on errors
        let child = Command::new("git")
            .arg("-C")
            .arg(local)
            .arg("fetch")
            .arg("--quiet")
            .arg("--prune-tags")
            .arg(upstream.to_string())
            .arg("+refs/*:refs/*") // Map all upstream refs to local refs.
            .spawn()?;
        child.wait_with_output().await
    }

    pub fn advertise_refs(
        &self,
        local: PathBuf,
    ) -> Result<Box<dyn AsyncRead + Send + Sync + 'static>> {
        // TODO: enable kill on drop (prob. requires returning the child)
        // TODO: try to unbox
        let mut child = Command::new("git-upload-pack")
            .arg("--stateless-rpc")
            .arg("--http-backend-info-refs")
            .arg(local)
            .stdout(Stdio::piped())
            .spawn()?;
        Ok(Box::new(
            child.stdout.take().expect("stdout should be piped"),
        ))
    }
}
