use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Output, Stdio};

use anyhow::{anyhow, ensure, Context};
use axum::body::Bytes;
use axum::http::header;
use axum::http::{HeaderMap, HeaderValue, Uri};
use reqwest::{Client, StatusCode};
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::process::Command;
use tracing::{instrument, Instrument};

use crate::error::{Error, Result};
use crate::APP_NAME;

#[cfg(test)]
use mockall::automock;

// A trait object is required because mockall can't handle `Result<impl Trait>` (in return
// position) just yet. Otherwise we should be able to get by with `impl AsyncRead + Send + Unpin`.
pub type GitAsyncRead = Box<dyn AsyncRead + Send + Unpin>;

#[derive(Default, Debug)]
pub struct Git {}

#[cfg_attr(test, automock, allow(dead_code))]
impl Git {
    #[instrument(skip(self))]
    pub async fn init(&self, local: PathBuf) -> Result<()> {
        let output = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg("--bare")
            .arg(local)
            .stdin(Stdio::null())
            .output()
            .await
            .expect("failed to execute `git init`");

        exited_ok_with_stdout(output, "git init", "failed to initialize repository")?;

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn authenticate_with_head(
        &self,
        upstream: Uri,
        auth: Option<HeaderValue>,
    ) -> Result<Option<String>> {
        let mut extra_headers = HeaderMap::new();

        if let Some(auth) = auth {
            assert!(auth.is_sensitive());
            extra_headers.insert(header::AUTHORIZATION, auth);
        }

        let response = Client::builder()
            .user_agent(APP_NAME)
            .build()
            .expect("failed to build reqwest client")
            .get(format!("{upstream}/info/refs?service=git-upload-pack"))
            .headers(extra_headers)
            .send()
            .await
            .context("failed to get upstream /info/refs")?;

        match response.status() {
            StatusCode::OK => { /* keep going */ }
            StatusCode::NOT_FOUND => return Err(Error::NotFound),
            StatusCode::UNAUTHORIZED => {
                let authenticate = response
                    .headers()
                    .get(header::WWW_AUTHENTICATE)
                    .cloned()
                    .ok_or(anyhow!(
                    "missing WWW-Authenticate header for 401 Unauthorized response from upstream"
                ))?;
                return Err(Error::MissingAuth(authenticate));
            }
            code => {
                return Err(anyhow!("upstream responded to /info/refs with status {code}").into())
            }
        };

        // FIXME: check content-type, we only support smart responses

        let response = response
            .bytes()
            .await
            .context("failed to read full response from upstream /info/refs")?;

        Ok(parse_smart_refs(response)
            .context("failed to parse response from upstream /info/refs")?)
    }

    #[instrument(skip(self))]
    pub async fn fetch(
        &self,
        upstream: Uri,
        local: PathBuf,
        auth: Option<HeaderValue>,
    ) -> Result<()> {
        let mut command = Command::new("git");

        if let Some(auth) = auth {
            assert!(auth.is_sensitive());

            if let Ok(auth) = auth.to_str() {
                command.env("AUTHORIZATION", format!("authorization: {auth}"));
                command.arg("--config-env");
                command.arg("http.extraHeader=AUTHORIZATION");
            } else {
                // FIXME: report error, since we don't support this case
            }
        }

        let output = command
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
            .expect("failed to execute `git fetch`");

        exited_ok_with_stdout(output, "git fetch", "failed to fetch from upstream")?;

        Ok(())
    }

    #[instrument(skip(self))]
    pub fn advertise_refs(&self, local: PathBuf) -> Result<GitAsyncRead> {
        let mut child = Command::new("git-upload-pack")
            .arg("--stateless-rpc")
            .arg("--http-backend-info-refs")
            .arg(local)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn `git-upload-pack`");

        let stdout = child.stdout.take().expect("stdout should be piped");

        // The stdout output will be handed off to axum to transmit it to the client. Therefore,
        // spawn a separete task to wait for and reape the child process when its done, instead of
        // relying on tokio doing that on a best-effort-only basis. This also allow us to log any
        // errors.
        tokio::spawn(
            async move {
                let output = child
                    .wait_with_output()
                    .await
                    .expect("failed to wait for `git-upload-pack` to exit");
                if !output.status.success() {
                    tracing::error!(
                        status = output.status.into_raw(),
                        stderr = ?Bytes::from(output.stderr),
                        "`git-upload-pack` exited with non-zero status",
                    );
                } else {
                    tracing::trace!("`git-upload-pack` exited with 0");
                }
            }
            .in_current_span(),
        );

        Ok(Box::new(stdout))
    }

    #[instrument(skip(self, input))]
    pub async fn upload_pack(&self, local: PathBuf, input: Bytes) -> Result<GitAsyncRead> {
        let mut child = Command::new("git-upload-pack")
            .arg("--stateless-rpc")
            .arg(local)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn `git-upload-pack`");

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
        tokio::spawn(
            async move {
                if let Err(err) = stdin.write_all(&input).await {
                    tracing::error!(error = ?err, "i/o error while writing to git-upload-pack");
                } else {
                    tracing::trace!("done writing to `git-upload-pack`");
                }
            }
            .in_current_span(),
        );

        // The stdout output will be handed off to axum to transmit it to the client. Therefore,
        // spawn a separete task to wait for and reape the child process when its done, instead of
        // relying on tokio doing that on a best-effort-only basis. This also allow us to log any
        // errors.
        tokio::spawn(
            async move {
                let output = child
                    .wait_with_output()
                    .await
                    .expect("failed to wait for `git-upload-pack` to exit");
                if !output.status.success() {
                    tracing::error!(
                        status = output.status.into_raw(),
                        stderr = ?Bytes::from(output.stderr),
                        "`git-upload-pack` exited with non-zero status",
                    );
                } else {
                    tracing::trace!("`git-upload-pack` exited with 0");
                }
            }
            .in_current_span(),
        );

        Ok(Box::new(stdout))
    }
}

fn exited_ok_with_stdout(
    output: Output,
    process_name: &'static str,
    error_message: &'static str,
) -> Result<Vec<u8>> {
    if !output.status.success() {
        tracing::error!(
            status = output.status.into_raw(),
            stderr = ?Bytes::from(output.stderr),
            "`{process_name}` exited with non-zero status",
        );
        return Err(anyhow!(error_message).into());
    } else {
        tracing::trace!("`{process_name}` exited with 0");
    }
    Ok(output.stdout)
}

fn parse_smart_refs(input: Bytes) -> anyhow::Result<Option<String>> {
    fn pkt_line(mut input: Bytes) -> anyhow::Result<(Bytes, Bytes)> {
        let pkt_len = input.split_to(4);
        if pkt_len == "0000" {
            Ok((pkt_len, input))
        } else {
            // FIXME: subsctraction can overflow and panic/wraparound
            let pkt_len = u16::from_str_radix(std::str::from_utf8(&pkt_len)?, 16)? - 4;
            Ok((input.split_to(pkt_len.into()), input))
        }
    }

    let (header, input) = pkt_line(input)?;
    ensure!(header == "# service=git-upload-pack\n");

    let (flush, input) = pkt_line(input)?;
    ensure!(flush == "0000");

    let (mut input, next) = pkt_line(input)?;
    if input.starts_with(b"version".as_slice()) {
        tracing::debug!(version_line = ?input);
        input = next;
    }

    if input == "0000" {
        return Ok(None);
    }

    tracing::debug!(first_ref_list_item = ?input);

    // FIXME: simplify and review corner cases
    let _obj_id = input.split_to(40);
    let _sp = input.split_to(1);
    let nul_pos = input.partition_point(|&c| c == 0);
    let _name = input.split_to(nul_pos);
    let lf_pos = input.partition_point(|&c| c == b'\n');
    let cap_list = input.split_off(lf_pos);

    for cap in cap_list
        .split(|&c| c == b' ')
        .map(|b| std::str::from_utf8(b))
    {
        if let Some(symref) = cap?.strip_prefix("symref=HEAD:") {
            return Ok(Some(symref.to_string()));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use axum::body::Bytes;

    use super::parse_smart_refs;

    #[test]
    fn parse_info_refs_response() {
        assert_eq!(
            parse_smart_refs(Bytes::from_static(include_bytes!(
                "../doc/example-info-refs-response"
            )))
            .unwrap(),
            Some(String::from("refs/heads/master"))
        );
    }

    #[test]
    fn parse_info_refs_response_with_version() {
        assert_eq!(
            parse_smart_refs(Bytes::from_static(include_bytes!(
                "../doc/example-info-refs-response-with-version"
            )))
            .unwrap(),
            Some(String::from("refs/heads/master"))
        );
    }

    #[test]
    fn parse_empty_repo_info_refs_response() {
        assert_eq!(
            parse_smart_refs(Bytes::from_static(include_bytes!(
                "../doc/example-empty-info-refs-response"
            )))
            .unwrap(),
            None
        );
    }
}
