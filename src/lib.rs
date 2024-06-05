use std::io;
use std::iter::once;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use clap::Parser;
use http_body_util::BodyExt;
use tokio::fs;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;
use tower::ServiceBuilder;
use tower_http::decompression::RequestDecompressionLayer;
use tower_http::request_id::MakeRequestUuid;
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tower_http::ServiceBuilderExt;

mod error;
mod git;

use crate::error::{Error, Result};

#[cfg(not(test))]
use crate::git::{Git, GitAsyncRead};
#[cfg(test)]
use crate::git::{GitAsyncRead, MockGit as Git};

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

pub async fn start(options: &Options) -> io::Result<()> {
    let app = app(options, Git::default()).await?;

    let listener = TcpListener::bind(("0.0.0.0", options.port)).await?;
    tracing::info!("Listening on {}", listener.local_addr()?);

    axum::serve(listener, app).await
}

#[derive(Debug)]
struct Index {
    cache_dir: PathBuf,
    git: Arc<Git>,
}

impl Index {
    fn new(cache_dir: PathBuf, git: Arc<Git>) -> Self {
        Self { cache_dir, git }
    }

    async fn open(&self, upstream: Uri) -> Result<Repo> {
        // FIXME: upstream must be sanitized:
        // - upstreams that escape out of the cache-dir
        // - upstreams that turn into subpaths of existing cached repos
        // - upstreams that result in absolute-looking paths being feed into Path::join

        let local = self
            .cache_dir
            .join(upstream.host().ok_or(Error::NotFound)?)
            .join(&upstream.path()[1..])
            .with_extension("git");

        fs::create_dir_all(&local)
            .await
            .context("failed to create directory for repository")?;
        self.git.init(local.clone()).await?;

        Ok(Repo {
            git: self.git.clone(),
            upstream: upstream.clone(),
            local,
        })
    }
}

#[derive(Debug)]
pub struct Repo {
    git: Arc<Git>,
    upstream: Uri,
    local: PathBuf,
}

impl Repo {
    pub async fn fetch(&mut self) -> Result<()> {
        // Assume we (the server) has a modern git that supports symrefs.
        let remote_head = self.git.remote_head(self.upstream.clone()).await?;
        tokio::fs::write(self.local.join("HEAD"), remote_head)
            .await
            .context("failed to update HEAD")?;

        self.git
            .fetch(self.upstream.clone(), self.local.clone())
            .await
    }

    pub fn advertise_refs(&mut self) -> Result<GitAsyncRead> {
        self.git.advertise_refs(self.local.clone())
    }

    pub async fn upload_pack(&mut self, input: Bytes) -> Result<GitAsyncRead> {
        self.git.upload_pack(self.local.clone(), input).await
    }
}

async fn app(options: &Options, git: Git) -> io::Result<Router> {
    // Ensure `cache_dir` exists and acquire a lock on it.
    fs::create_dir_all(&options.cache_dir).await?;
    fs::write(&options.cache_dir.join(".git-cache"), "").await?; // FIXME: lock
    tracing::info!("Cache directory is {:?}", options.cache_dir);

    let repos = Index::new(options.cache_dir.clone(), Arc::new(git));

    // TODO: delegate more to the axum router
    Ok(Router::new()
        .route("/*req", any(router))
        .with_state(Arc::new(repos))
        .layer(
            ServiceBuilder::new()
                // WARN: Will *not* overwrite `x-request-id` if already present.
                .set_x_request_id(MakeRequestUuid)
                .layer(SetSensitiveRequestHeadersLayer::new(once(AUTHORIZATION)))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(DefaultMakeSpan::new().include_headers(true))
                        .on_response(DefaultOnResponse::new().include_headers(true)),
                )
                .layer(RequestDecompressionLayer::new())
                .propagate_x_request_id(),
        ))
}

async fn router(State(repos): State<Arc<Index>>, request: Request<Body>) -> Result<Response> {
    // TODO: extract credentials

    if request.method() == Method::GET {
        // "Smart" protocol client step 1: ref discovery.

        if request.uri().query() != Some("service=git-upload-pack") {
            return Err(Error::NotFound);
        }

        let upstream = request
            .uri()
            .path()
            .strip_suffix("/info/refs")
            .ok_or(Error::NotFound)?;
        let upstream: Uri = format!("https:/{}", upstream)
            .parse()
            .map_err(|_| Error::NotFound)?;

        let repo = repos.open(upstream).await?;
        handle_ref_discovery(repo).await
    } else if request.method() == Method::POST {
        // "Smart" protocol client step 2: compute.

        let upstream = request
            .uri()
            .path()
            .strip_suffix("/git-upload-pack")
            .ok_or(Error::NotFound)?;
        let upstream: Uri = format!("https:/{}", upstream)
            .parse()
            .map_err(|_| Error::NotFound)?;

        let repo = repos.open(upstream).await?;
        handle_upload_pack(repo, request).await
    } else {
        Err(Error::NotFound)
    }
}

async fn handle_ref_discovery(mut repo: Repo) -> Result<Response> {
    // TODO: authenticate on upstream

    // Clone or update local copy from upstream.
    repo.fetch().await?;

    // Advertise refs to client.
    let stdout = repo.advertise_refs()?;
    let output = b"001e# service=git-upload-pack\n0000".chain(stdout);
    let output = ReaderStream::new(output);
    Ok((
        StatusCode::OK,
        [
            (
                "content-type",
                "application/x-git-upload-pack-advertisement",
            ),
            ("cache-control", "no-cache"),
        ],
        Body::from_stream(output),
    )
        .into_response())
}

async fn handle_upload_pack(mut repo: Repo, request: Request) -> Result<Response> {
    // TODO: authenticate on upstream

    // Assume this request immediately follows a ref-discovery step, in which we updated our copy
    // of the repository. If this isn't the case (if the client is broken), we'll simply reply with
    // outdated or no data.

    // FIXME: missing any type of safety limit on the body size
    // TODO: pipe the client body into git-upload-pack stdin instead of reading all beforehand

    // Proxy git-upload-pack.
    let input = request
        .into_body()
        .collect()
        .await
        .context("failed to collect the request body")?
        .to_bytes();
    let output = repo.upload_pack(input).await?;
    let output = ReaderStream::new(output);
    Ok((
        StatusCode::OK,
        [
            ("content-type", "application/x-git-upload-pack-result"),
            ("cache-control", "no-cache"),
        ],
        Body::from_stream(output),
    )
        .into_response())
}

#[cfg(test)]
mod unit_tests {
    use std::io::Write;

    use axum::http::header;
    use flate2::{write::GzEncoder, Compression};
    use http_body_util::BodyExt;
    use mockall::predicate::eq;
    use tempfile::tempdir;

    use tower::{Service, ServiceExt};

    use super::*;

    #[tokio::test]
    async fn ref_discovery_new_repo() {
        let config = Options {
            cache_dir: tempdir().unwrap().into_path(),
            port: 0,
        };

        // TODO: check sequence of git ops?
        let mut mock_git = Git::default();

        mock_git
            .expect_init()
            .with(eq(config.cache_dir.join("example.com/a/b/c.git")))
            .times(1)
            .returning(|_| Ok(()));

        mock_git
            .expect_remote_head()
            .with(eq(Uri::from_static("https://example.com/a/b/c")))
            .times(1)
            .returning(|_| Ok(String::from("ref: refs/heads/mock")));

        mock_git
            .expect_fetch()
            .with(
                eq(Uri::from_static("https://example.com/a/b/c")),
                eq(config.cache_dir.join("example.com/a/b/c.git")),
            )
            .times(1)
            .returning(|_, _| Ok(()));

        mock_git
            .expect_advertise_refs()
            .with(eq(config.cache_dir.join("example.com/a/b/c.git")))
            .times(1)
            .returning(|_| Ok(Box::new("mock git-upload-pack output".as_bytes())));

        let app = app(&config, mock_git).await.unwrap();

        let response = app
            .oneshot(
                Request::get("/example.com/a/b/c/info/refs?service=git-upload-pack")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        assert_eq!(
            Vec::from_iter(response.headers().get_all("content-type").into_iter()),
            ["application/x-git-upload-pack-advertisement"]
        );

        assert_eq!(
            Vec::from_iter(response.headers().get_all("cache-control").into_iter()),
            ["no-cache"]
        );

        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "001e# service=git-upload-pack\n0000mock git-upload-pack output"
        );

        assert_eq!(
            tokio::fs::read(config.cache_dir.join("example.com/a/b/c.git/HEAD"))
                .await
                .unwrap(),
            b"ref: refs/heads/mock"
        );
    }

    #[tokio::test]
    async fn ref_discovery_existing_repo() {
        // NOTE: Assumes that basic ref discovery of a new repo has passed its tests.

        let config = Options {
            cache_dir: tempdir().unwrap().into_path(),
            port: 0,
        };

        // TODO: check sequence of git ops?
        let mut mock_git = Git::default();

        mock_git.expect_init().times(1..=2).returning(|_| Ok(()));

        mock_git
            .expect_remote_head()
            .times(2)
            .returning(|_| Ok(String::from("ref: refs/heads/mock")));

        mock_git.expect_fetch().times(2).returning(|_, _| Ok(()));

        mock_git
            .expect_advertise_refs()
            .times(2)
            .returning(|_| Ok(Box::new("mock git-upload-pack output".as_bytes())));

        let mut app = app(&config, mock_git).await.unwrap();

        // Can't clone Request because axum::body::Body isn't Clone.

        let clone = app
            .call(
                Request::get("/example.com/a/b/c/info/refs?service=git-upload-pack")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let fetch = app
            .oneshot(
                Request::get("/example.com/a/b/c/info/refs?service=git-upload-pack")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(clone.status(), StatusCode::OK);
        assert_eq!(fetch.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn upload_pack() {
        let config = Options {
            cache_dir: tempdir().unwrap().into_path(),
            port: 0,
        };

        // TODO: check sequence of git ops?
        let mut mock_git = Git::default();

        mock_git.expect_init().times(1).returning(|_| Ok(()));

        mock_git
            .expect_upload_pack()
            .with(
                eq(config.cache_dir.join("example.com/a/b/c.git")),
                eq(Bytes::from("mock client input: 42")),
            )
            .times(1)
            .returning(|_, _| Ok(Box::new("mock git-upload-pack output".as_bytes())));

        let app = app(&config, mock_git).await.unwrap();

        let response = app
            .oneshot(
                Request::post("/example.com/a/b/c/git-upload-pack")
                    .body(Body::from("mock client input: 42"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        assert_eq!(
            Vec::from_iter(response.headers().get_all("content-type").into_iter()),
            ["application/x-git-upload-pack-result"]
        );

        assert_eq!(
            Vec::from_iter(response.headers().get_all("cache-control").into_iter()),
            ["no-cache"]
        );

        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "mock git-upload-pack output"
        );
    }

    #[tokio::test]
    async fn compressed_upload_pack_request() {
        let config = Options {
            cache_dir: tempdir().unwrap().into_path(),
            port: 0,
        };

        // TODO: check sequence of git ops?
        let mut mock_git = Git::default();

        mock_git.expect_init().times(1).returning(|_| Ok(()));

        mock_git
            .expect_upload_pack()
            .with(
                eq(config.cache_dir.join("example.com/a/b/c.git")),
                eq(Bytes::from("mock client input: 42")),
            )
            .times(1)
            .returning(|_, _| Ok(Box::new("mock git-upload-pack output".as_bytes())));

        let app = app(&config, mock_git).await.unwrap();

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"mock client input: 42").unwrap();

        let response = app
            .oneshot(
                Request::post("/example.com/a/b/c/git-upload-pack")
                    .header(header::CONTENT_ENCODING, "gzip")
                    .body(Body::from(encoder.finish().unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "mock git-upload-pack output"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn authentication() {
        todo!()
    }

    // TODO: support or at least don't break with git protocol v2
}
