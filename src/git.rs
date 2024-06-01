use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Output, Stdio};

use anyhow::{anyhow, Context};
use axum::body::Bytes;
use axum::http::Uri;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::process::Command;

use crate::error::Result;

#[cfg(test)]
use mockall::automock;

#[derive(Default, Debug)]
pub struct Git {}

type AsyncOutput = Box<dyn AsyncRead + Send + Sync + 'static>;

fn check_child_exit_status(
    output: &Output,
    process_name: &'static str,
    error_message: &'static str,
) -> Result<()> {
    if !output.status.success() {
        tracing::error!(
            status = output.status.into_raw(),
            stdout = String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr = String::from_utf8_lossy(&output.stderr).into_owned(),
            "`{}` exited with non-zero status",
            process_name
        );
        return Err(anyhow!(error_message).into());
    }
    Ok(())
}

#[cfg_attr(test, automock, allow(dead_code))]
impl Git {
    pub async fn init(&self, local: PathBuf) -> Result<()> {
        let output = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg("--bare")
            .arg(local)
            .stdin(Stdio::null())
            .output()
            .await
            .context("failed to execute `git init`")?;

        check_child_exit_status(&output, "git init", "failed to initialize repository")
    }

    pub async fn remote_head(&self, upstream: Uri) -> Result<String> {
        // HACK: this is a quite brittle and inneficient way to get the remote HEAD
        // TODO: handle a valid but empty remote with no refs

        let output = Command::new("git")
            .arg("ls-remote")
            .arg("--symref")
            .arg(upstream.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .output()
            .await
            .context("failed to execute `git ls-remote`")?;

        check_child_exit_status(&output, "git ls-remote", "failed to fetch remote refs")?;

        let output = String::from_utf8(output.stdout)
            .context("output from `git ls-remote` is not valid utf-8")?;

        Ok(output
            .lines()
            .next()
            .ok_or(anyhow!("failed to fetch remote refs"))?
            .strip_suffix("\tHEAD")
            .ok_or(anyhow!("failed to find HEAD in remote refs"))?
            .to_owned())
    }

    pub async fn fetch(&self, upstream: Uri, local: PathBuf) -> Result<()> {
        // TODO: set up authentication

        let output = Command::new("git")
            .arg("-C")
            .arg(local)
            .arg("fetch")
            .arg("--quiet")
            .arg("--prune-tags")
            .arg(upstream.to_string())
            .arg("+refs/*:refs/*") // Map all upstream refs to local refs.
            .stdin(Stdio::null())
            .output()
            .await
            .context("failed to execute `git fetch`")?;

        check_child_exit_status(&output, "git fetch", "failed to fetch from upstream")?;

        Ok(())
    }

    pub fn advertise_refs(&self, local: PathBuf) -> Result<AsyncOutput> {
        let mut child = Command::new("git-upload-pack")
            .arg("--stateless-rpc")
            .arg("--http-backend-info-refs")
            .arg(local)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn `git-upload-pack`")?;

        let stdout = child.stdout.take().expect("stdout should be piped");

        // The stdout output will be handed off to axum to transmit it to the client. Therefore,
        // spawn a separete task to wait for and reape the child process when its done, instead of
        // relying on tokio doing that on a best-effort-only basis. This also allow us to log any
        // errors.
        tokio::spawn(async move {
            match child.wait_with_output().await {
                Ok(output) => {
                    let _ = check_child_exit_status(
                        &output,
                        "git-upload-pack",
                        "failed to advertise refs",
                    );
                }
                Err(err) => tracing::error!(?err, "failed to wait for `git-upload-pack` to exit"),
            };
        });

        // TODO: try to unbox
        Ok(Box::new(stdout))
    }

    pub async fn upload_pack(&self, local: PathBuf, input: Bytes) -> Result<AsyncOutput> {
        let mut child = Command::new("git-upload-pack")
            .arg("--stateless-rpc")
            .arg(local)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn `git-upload-pack`")?;

        let mut stdin = child.stdin.take().expect("stdin should be piped");
        let stdout = child.stdout.take().expect("stdout should be piped");

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
            if let Err(err) = stdin.write_all(&input).await {
                tracing::error!(?err, "i/o error while writing to git-upload-pack");
            }
        });

        // The stdout output will be handed off to axum to transmit it to the client. Therefore,
        // spawn a separete task to wait for and reape the child process when its done, instead of
        // relying on tokio doing that on a best-effort-only basis. This also allow us to log any
        // errors.
        tokio::spawn(async move {
            match child.wait_with_output().await {
                Ok(output) => {
                    let _ = check_child_exit_status(
                        &output,
                        "git-upload-pack",
                        "failed to advertise refs",
                    );
                }
                Err(err) => tracing::error!(?err, "failed to wait for `git-upload-pack` to exit"),
            };
        });

        // TODO: try to unbox
        Ok(Box::new(stdout))
    }
}
