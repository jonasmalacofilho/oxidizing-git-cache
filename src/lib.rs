use std::io::Result;
use std::path::PathBuf;
use std::sync::Arc;

use http_body_util::BodyExt;
use tokio::fs;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;

use tracing::{info, instrument};

use clap::Parser;

mod git;

#[cfg(not(test))]
use crate::git::Git;
#[cfg(test)]
use crate::git::MockGit as Git;

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
    let app = app(options, Git::default()).await?;

    let listener = TcpListener::bind(("0.0.0.0", options.port)).await?;
    info!("Listening on {}", listener.local_addr()?);

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

    #[instrument(level = "debug", skip_all)]
    async fn open(&self, upstream: Uri) -> Result<Repo> {
        // FIXME: upstream must be sanitized:
        // - upstreams that escape out of the cache-dir
        // - upstreams that turn into subpaths of existing cached repos
        // - upstreams that result in absolute-looking paths being feed into Path::join

        let local = self
            .cache_dir
            .join(upstream.host().unwrap())
            .join(&upstream.path()[1..])
            .with_extension("git");

        fs::create_dir_all(&local).await?;
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
    #[instrument(level = "debug", skip_all)]
    pub async fn fetch(&mut self) -> Result<()> {
        // Assume we (the server) has a modern git that supports symrefs.
        let remote_head = self.git.remote_head(self.upstream.clone()).await?;
        tokio::fs::write(self.local.join("HEAD"), remote_head).await?;

        self.git
            .fetch(self.upstream.clone(), self.local.clone())
            .await
            .map(|_| ())
    }

    #[instrument(level = "debug", skip_all)]
    pub fn advertise_refs(&mut self) -> Result<Box<dyn AsyncRead + Send + Sync + 'static>> {
        self.git.advertise_refs(self.local.clone())
    }

    #[instrument(level = "debug", skip_all)]
    pub async fn upload_pack(
        &mut self,
        input: Bytes,
    ) -> Result<Box<dyn AsyncRead + Send + Sync + 'static>> {
        self.git.upload_pack(self.local.clone(), input).await
    }
}

#[instrument(level = "debug", skip_all)]
async fn app(options: &Options, git: Git) -> Result<Router> {
    // Ensure `cache_dir` exists and acquire a lock on it.
    fs::create_dir_all(&options.cache_dir).await?;
    fs::write(&options.cache_dir.join(".git-cache"), "").await?; // FIXME: lock
    info!("Cache directory is {:?}", options.cache_dir);

    let repos = Index::new(options.cache_dir.clone(), Arc::new(git));

    // TODO: delegate more to the axum router
    Ok(Router::new()
        .route("/*req", any(router))
        .with_state(Arc::new(repos)))
}

#[instrument(level = "debug", skip_all)]
async fn router(State(repos): State<Arc<Index>>, request: Request<Body>) -> Response {
    // TODO: extract credentials

    if request.method() == Method::GET {
        // "Smart" protocol client step 1: ref discovery.

        if request.uri().query() != Some("service=git-upload-pack") {
            return StatusCode::NOT_FOUND.into_response();
        }

        let Some(upstream) = request.uri().path().strip_suffix("/info/refs") else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let upstream: Uri = format!("https:/{}", upstream).parse().unwrap();

        let repo = repos.open(upstream).await.unwrap();
        handle_ref_discovery(repo).await
    } else if request.method() == Method::POST {
        // "Smart" protocol client step 2: compute.

        let Some(upstream) = request.uri().path().strip_suffix("/git-upload-pack") else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let upstream: Uri = format!("https:/{}", upstream).parse().unwrap();

        let repo = repos.open(upstream).await.unwrap();
        handle_upload_pack(repo, request).await
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[instrument(level = "debug", ret)]
async fn handle_ref_discovery(mut repo: Repo) -> Response {
    // TODO: validate & authenticate on upstream, and appropriately reply to client

    repo.fetch().await.unwrap();

    let stdout = Box::into_pin(repo.advertise_refs().unwrap());
    let output = b"001e# service=git-upload-pack\n0000".chain(stdout);
    let output = ReaderStream::new(output);

    Response::builder()
        .status(StatusCode::OK)
        .header(
            "Content-Type",
            "application/x-git-upload-pack-advertisement",
        )
        .header("Cache-Control", "no-cache")
        .body(Body::from_stream(output))
        .unwrap()
}

#[instrument(level = "debug")]
async fn handle_upload_pack(mut repo: Repo, request: Request) -> Response {
    // TODO: validate & authenticate on upstream, and appropriately reply to client

    // FIXME: pipe client body into git-upload-pack stdin
    // let input = request
    //     .into_body()
    //     .into_data_stream()
    //     .map_err(|err| Error::new(ErrorKind::Other, err));
    // let stdin = StreamReader::new(input);

    // FIXME: missing any type of safety limit on the body size
    let input = request.into_body().collect().await.unwrap().to_bytes();

    // FIXME: can't deal with large wants, which are gzip compressed

    let output = Box::into_pin(repo.upload_pack(input).await.unwrap());
    let output = ReaderStream::new(output);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/x-git-upload-pack-result")
        .header("Cache-Control", "no-cache")
        .body(Body::from_stream(output))
        .unwrap()
}

#[cfg(test)]
mod unit_tests {
    use std::process::{ExitStatus, Output};

    use http_body_util::BodyExt;
    use mockall::predicate::eq;
    use tempfile::tempdir;

    use tower::{Service, ServiceExt};

    use super::*;

    fn default_output() -> Result<Output> {
        Ok(Output {
            status: ExitStatus::default(),
            stdout: vec![],
            stderr: vec![],
        })
    }

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
            .returning(|_| default_output());

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
            .returning(|_, _| default_output());

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

        mock_git
            .expect_init()
            .times(1..=2)
            .returning(|_| default_output());

        mock_git
            .expect_fetch()
            .times(2)
            .returning(|_, _| default_output());

        mock_git
            .expect_remote_head()
            .times(2)
            .returning(|_| Ok(String::from("ref: refs/heads/mock")));

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

        mock_git
            .expect_init()
            .times(1)
            .returning(|_| default_output());

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
    #[ignore]
    async fn authentication() {
        todo!()
    }

    // TODO: support or at least don't break with git protocol v2
}
