use std::io::Result;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::process::Stdio;
use std::str::FromStr;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Request, State},
    handler::HandlerWithoutStateExt,
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{any, get},
    Router,
};
use clap::Parser;
use git::{GitCli, GitOps, Repo};
use serde::Deserialize;
use tokio::{fs, io::AsyncReadExt, net::TcpListener};
use tokio_util::io::ReaderStream;
use tracing::{info, instrument};

mod git;

/// A caching Git HTTP server.
///
/// Serve and update local mirrors of Git repositories over HTTP.
#[derive(Clone, Debug, Parser)]
#[command(version)]
pub struct Options {
    /// Location of the git cache.
    #[arg(short, long, default_value = "/var/cache/git", name = "PATH")]
    cache_dir: PathBuf,

    /// Bind to port.
    #[arg(short, long, default_value = "8080")]
    port: u16,
}

#[instrument(level = "debug", skip_all)]
pub async fn start(options: &Options) -> Result<()> {
    let app = app::<GitCli>(options).await?;

    let listener = TcpListener::bind(("0.0.0.0", options.port)).await?;
    info!("Listening on {}", listener.local_addr()?);

    axum::serve(listener, app).await
}

struct Repos<G> {
    cache_dir: PathBuf,
    _phantom: PhantomData<G>,
}

impl<G: GitOps> Repos<G> {
    fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            _phantom: PhantomData,
        }
    }

    fn open(&self, upstream: PathBuf) -> Repo<G> {
        // FIXME: upstream must be sanitized.
        Repo::new(
            upstream.clone(),
            self.cache_dir
                .join(upstream.strip_prefix("https://").unwrap()),
        )
    }
}

#[instrument(level = "debug", skip_all)]
async fn app<G: GitOps>(options: &Options) -> Result<Router> {
    // Ensure `cache_dir` exists and acquire a lock on it.
    fs::create_dir_all(&options.cache_dir).await?;
    fs::write(&options.cache_dir.join(".git-cache"), "").await?; // FIXME: lock
    info!("Cache directory is {:?}", options.cache_dir);

    let repos: Repos<G> = Repos::new(options.cache_dir.clone());

    Ok(Router::new()
        .route("/*req", any(router))
        .with_state(Arc::new(repos)))
}

#[instrument(level = "debug", skip_all)]
async fn router<G: GitOps>(State(repos): State<Arc<Repos<G>>>, request: Request<Body>) -> Response {
    // TODO: handle missing scheme; use http (localhost) or https (else)
    let enclosed_uri: Uri = request.uri().to_string()[1..].parse().unwrap();

    if enclosed_uri.query() != Some("service=git-upload-pack") {
        return StatusCode::NOT_FOUND.into_response();
    }

    // Only `git-upload-pack` (`clone` and `fetch` operations) is currently supported.
    let path = enclosed_uri.path();

    // TODO: extract authentication

    if request.method() == Method::GET && path.ends_with("/info/refs") {
        // "Smart" protocol client step 1: ref discovery.
        let upstream = enclosed_uri
            .to_string()
            .strip_suffix("/info/refs?service=git-upload-pack")
            .unwrap()
            .to_string(); // FIXME: untyped mess

        let repo = repos.open(upstream.into());
        handle_ref_discovery(repo).await
    } else if request.method() == Method::POST && path.ends_with("/git-upload-pack") {
        // "Smart" protocol client step 2: compute.
        handle_upload_pack().await
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[instrument(level = "debug", skip(repo))]
async fn handle_ref_discovery<G: GitOps>(mut repo: Repo<G>) -> Response {
    // TODO: update

    // TODO: serve
    // TODO: enable kill on drop

    let stdout = repo.advertise_refs().unwrap(); // FIXME: unwrap
    let body = b"001e# service=git-upload-pack\n0000".chain(stdout);
    let stream = ReaderStream::new(body);

    Response::builder()
        .status(StatusCode::OK)
        .header(
            "Content-Type",
            "application/x-git-upload-pack-advertisement",
        )
        .header("Cache-Control", "no-cache")
        .body(Body::from_stream(stream))
        .unwrap()
}

#[instrument(level = "debug")]
async fn handle_upload_pack() -> Response {
    Response::builder()
        .status(StatusCode::NOT_IMPLEMENTED)
        .body(Body::empty())
        .unwrap()
}

#[cfg(test)]
mod unit_tests {
    use http_body_util::BodyExt;
    use std::net::SocketAddr;
    use tower::ServiceExt;

    use super::*;
    use crate::git::GitMock;

    #[tokio::test]
    async fn ref_discovery() {
        // FIXME: use single-use temporary dirs.
        let app = app::<GitMock>(&Options {
            cache_dir: "/tmp/git-cache-test-unit-tests".into(),
            port: 0,
        })
        .await
        .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/https://example.com/a/b/c/info/refs?service=git-upload-pack")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        assert_eq!(
            response
                .headers()
                .get_all("content-type")
                .into_iter()
                .count(),
            1
        );
        assert_eq!(
            response.headers().get("content-type"),
            Some(
                &"application/x-git-upload-pack-advertisement"
                    .parse()
                    .unwrap()
            )
        );

        assert_eq!(
            response
                .headers()
                .get_all("cache-control")
                .into_iter()
                .count(),
            1
        );
        assert_eq!(
            response.headers().get("cache-control"),
            Some(&"no-cache".parse().unwrap())
        );

        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            body,
            "001e# service=git-upload-pack\n0000mock git-upload-pack output"
        );
    }

    #[test]
    #[ignore]
    fn upload_pack() {
        assert!(false);
    }

    #[test]
    #[ignore]
    fn authentication() {
        assert!(false);
    }
}
