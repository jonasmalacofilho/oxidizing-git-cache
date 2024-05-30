use std::io::Result;
use std::path::PathBuf;
use std::process::{Output, Stdio};

use axum::body::Bytes;
use axum::http::Uri;
use tokio::io::AsyncRead;
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

        let mut stdin = child.stdin.take().expect("stdin should be piped");

        // While in general we expect git-upload-pack to process its entire input before writing
        // anything to its output, that's might not be necessarily true in all cases.
        //
        // For robusness, we need to write to `child` concurrently with reading its output. But its
        // output will be forwarded by axum to the client, *after* the HTTP status code has already
        // been sent (200 OK).
        //
        // Therefore we don't really have to return write errors to the client. And with the
        // current git op abstraction, it wouldn't be possible to do it (changing the abstraction
        // is hard because it has to be easily mockable in tests). So instead just log any such
        // errors.
        tokio::spawn(async move {
            if let Err(err) = tokio::io::copy_buf(&mut &*input, &mut stdin).await {
                tracing::info!(?err, "i/o error while writing to git-upload-pack");
            }
        });

        Ok(Box::new(
            child.stdout.take().expect("stdout should be piped"),
        ))
    }
}
