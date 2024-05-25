use std::io::Result;
use std::path::PathBuf;
use std::process::Output;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;

use tokio::fs;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

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
struct Repos {
    cache_dir: PathBuf,
    git: Arc<Git>,
}

impl Repos {
    fn new(cache_dir: PathBuf, git: Arc<Git>) -> Self {
        Self { cache_dir, git }
    }

    #[instrument(level = "debug", skip_all)]
    fn open(&self, upstream: Uri) -> Repo {
        // FIXME: upstream must be sanitized
        // NOTE: beware of components that look like absolute paths when using `join`
        Repo {
            git: self.git.clone(),
            upstream: upstream.clone(),
            local: self
                .cache_dir
                .join(upstream.host().unwrap())
                .join(&upstream.path()[1..]),
        }
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
    pub async fn clone(&mut self) -> Result<Output> {
        self.git
            .clone_repo(self.upstream.clone(), self.local.clone())
            .await
    }

    #[instrument(level = "debug", skip_all)]
    pub fn advertise_refs(&mut self) -> Result<Box<dyn AsyncRead + Send + Sync + 'static>> {
        self.git.advertise_refs(self.local.clone())
    }
}

#[instrument(level = "debug", skip_all)]
async fn app(options: &Options, git: Git) -> Result<Router> {
    // Ensure `cache_dir` exists and acquire a lock on it.
    fs::create_dir_all(&options.cache_dir).await?;
    fs::write(&options.cache_dir.join(".git-cache"), "").await?; // FIXME: lock
    info!("Cache directory is {:?}", options.cache_dir);

    let repos = Repos::new(options.cache_dir.clone(), Arc::new(git));

    Ok(Router::new()
        .route("/*req", any(router))
        .with_state(Arc::new(repos)))
}

#[instrument(level = "debug", skip_all)]
async fn router(State(repos): State<Arc<Repos>>, request: Request<Body>) -> Response {
    let uri = request.uri();

    if uri.query() != Some("service=git-upload-pack") {
        return StatusCode::NOT_FOUND.into_response();
    }

    // TODO: extract authentication

    if request.method() == Method::GET {
        // "Smart" protocol client step 1: ref discovery.
        let Some(upstream) = uri.path().strip_suffix("/info/refs") else {
            todo!("handle {uri}")
        };
        let upstream: Uri = format!("https:/{}", upstream).parse().unwrap();
        let repo = repos.open(upstream);
        handle_ref_discovery(repo).await
    } else if request.method() == Method::POST && uri.path().ends_with("/git-upload-pack") {
        // "Smart" protocol client step 2: compute.
        handle_upload_pack().await
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[instrument(level = "debug", ret)]
async fn handle_ref_discovery(mut repo: Repo) -> Response {
    // TODO: authenticate
    // TODO: update local copy with git-fetch
    // TODO: use git-fetch for everything, including the initial "clone"

    // FIXME: return http error on unsuccessful git-clone
    repo.clone().await.unwrap();

    // FIXME: return http error on unsuccessful git-upload-pack
    let stdout = Box::into_pin(repo.advertise_refs().unwrap());
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
    use std::process::ExitStatus;

    use http_body_util::BodyExt;
    use mockall::predicate::eq;
    use tempfile::tempdir;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn ref_discovery_new_repo() {
        let config = Options {
            cache_dir: tempdir().unwrap().into_path(),
            port: 0,
        };

        let mut mock_git = Git::default();

        mock_git
            .expect_clone_repo()
            .with(
                eq(Uri::from_static("https://example.com/a/b/c")),
                eq(config.cache_dir.join("example.com/a/b/c")),
            )
            .times(1)
            .return_once(move |_, _| {
                Ok(Output {
                    status: ExitStatus::default(),
                    stdout: vec![],
                    stderr: vec![],
                })
            });

        mock_git
            .expect_advertise_refs()
            .with(eq(config.cache_dir.join("example.com/a/b/c")))
            .times(1)
            .returning(|_| Ok(Box::new("mock git-upload-pack output".as_bytes())));

        let app = app(&config, mock_git).await.unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/example.com/a/b/c/info/refs?service=git-upload-pack")
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
    #[ignore]
    async fn ref_discovery_existing_repo() {
        todo!()
    }

    #[test]
    #[ignore]
    fn upload_pack() {
        todo!()
    }

    #[test]
    #[ignore]
    fn authentication() {
        todo!()
    }
}
