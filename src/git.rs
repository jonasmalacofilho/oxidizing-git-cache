use std::io::Result;
use std::path::PathBuf;
use std::process::{Output, Stdio};

use axum::body::Bytes;
use axum::http::Uri;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::process::Command;

#[cfg(test)]
use mockall::automock;

#[derive(Default, Debug)]
pub struct Git {}

type AsyncOutput = Box<dyn AsyncRead + Send + Sync + 'static>;

#[cfg_attr(test, automock, allow(dead_code))]
impl Git {
    pub async fn init(&self, local: PathBuf) -> Result<Output> {
        // TODO: store stdout/stderr and log/return on errors
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

    pub fn advertise_refs(&self, local: PathBuf) -> Result<AsyncOutput> {
        // FIXME: no control over child termination and reaping
        // TODO: store stdout/stderr and log/return on errors
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

    pub async fn upload_pack(&self, local: PathBuf, input: Bytes) -> Result<AsyncOutput> {
        // FIXME: no control over child termination and reaping
        // TODO: store stdout/stderr and log/return on errors
        // TODO: try to unbox

        let mut child = Command::new("git-upload-pack")
            .arg("--stateless-rpc")
            .arg(local)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;

        // In general, this could cause issues, since we're writing to stdin without attempting to
        // read from stdout: the child could block on not being able to write, stop reading, and by
        // consequence block us to.
        //
        // However, by the nature of the git protocol, assume git-upload-pack specifically needs to
        // read the entire input (wants and haves) before being able to write back. This should be
        // true at least in the happy path, where all wants are valid.
        // FIXME: make this robust to git-upload-pack blocking, or ensure it can't happen
        child
            .stdin
            .take()
            .expect("stdin should be piped")
            .write_all(&input)
            .await?;

        Ok(Box::new(
            child.stdout.take().expect("stdout should be piped"),
        ))
    }
}
