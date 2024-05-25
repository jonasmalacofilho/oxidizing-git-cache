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
    pub async fn clone_repo(&self, upstream: Uri, local: PathBuf) -> Result<Output> {
        // TODO: store stdout/stderr and log/return on errors
        let child = Command::new("git")
            .arg("clone")
            .arg("--quiet")
            .arg("--mirror")
            .arg(upstream.to_string())
            .arg(local)
            .spawn()?;
        child.wait_with_output().await
    }

    pub fn advertise_refs(
        &self,
        local: PathBuf,
    ) -> Result<Box<dyn AsyncRead + Send + Sync + 'static>> {
        // TODO: enable kill on drop (prob. requires returning the child)
        let mut child = Command::new("git-upload-pack")
            .arg("--stateless-rpc")
            .arg("--http-backend-info-refs")
            .arg(local) // FIXME bytes or path?
            .stdout(Stdio::piped())
            .spawn()?;
        Ok(Box::new(child.stdout.take().unwrap())) // FIXME (or not) unwrap
    }
}
