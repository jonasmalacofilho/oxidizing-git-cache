use std::io;
use std::iter::once;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header;
use axum::http::{HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use clap::Parser;
use http_body_util::BodyExt;
use tokio::fs;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_util::io::ReaderStream;
use tower::ServiceBuilder;
use tower_http::decompression::RequestDecompressionLayer;
use tower_http::request_id::{MakeRequestUuid, RequestId};
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use tower_http::ServiceBuilderExt;
use tracing::Span;

use crate::error::{Error, Result};
use crate::repo::{Index, Repo};

#[cfg(not(test))]
use crate::git::Git;
#[cfg(test)]
use crate::git::MockGit as Git;
use crate::APP_NAME;

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

async fn app(options: &Options, git: Git) -> io::Result<Router> {
    // Ensure `cache_dir` exists and acquire a lock on it.
    fs::create_dir_all(&options.cache_dir).await?;
    fs::write(&options.cache_dir.join(".git-cache"), "").await?; // FIXME: lock
    tracing::info!("Cache directory is {:?}", options.cache_dir);

    let index = Index::new(options.cache_dir.clone(), git);

    // TODO: delegate more to the axum router
    Ok(Router::new()
        .route("/*req", any(router))
        .with_state(Arc::new(index))
        .layer(
            ServiceBuilder::new()
                // WARN: Will *not* overwrite `x-request-id` if already present.
                .set_x_request_id(MakeRequestUuid)
                .layer(SetSensitiveRequestHeadersLayer::new(once(
                    header::AUTHORIZATION,
                )))
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(|request: &Request<_>| {
                            let request_id = request
                                .extensions()
                                .get::<RequestId>()
                                .unwrap()
                                .header_value();
                            tracing::info_span!("request", ?request_id)
                        })
                        .on_request(|request: &Request<_>, _: &Span| {
                            tracing::info!(
                                headers = ?request.headers(),
                                "received {} {} {:?}",
                                request.method(),
                                request.uri(),
                                request.version(),
                            )
                        })
                        .on_response(|response: &Response<_>, latency: Duration, _: &Span| {
                            tracing::info!(
                                ?latency,
                                headers = ?response.headers(),
                                "done with status {}",
                                response.status(),
                            )
                        }),
                )
                .layer(RequestDecompressionLayer::new())
                .propagate_x_request_id()
                .layer(SetResponseHeaderLayer::overriding(
                    header::SERVER,
                    HeaderValue::from_static(APP_NAME),
                )),
        ))
}

async fn router(State(repos): State<Arc<Index>>, request: Request<Body>) -> Result<Response> {
    if request.method() == Method::GET {
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
        handle_ref_discovery(repo, request).await
    } else if request.method() == Method::POST {
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

// "Smart" protocol client step 1: ref discovery.
async fn handle_ref_discovery(repo: Arc<Mutex<Repo>>, request: Request) -> Result<Response> {
    // FIXME: should only drop this guard after child git-upload-pack exits.
    let mut repo = repo.lock().await;

    // Authenticate and fetch the remote head (if available).
    let auth = request.headers().get(header::AUTHORIZATION).cloned();
    let remote_head = repo.authenticate_with_head(auth.clone()).await?;

    // Clone or update local copy from upstream.
    repo.fetch(remote_head, auth).await?;

    // Advertise refs to client.
    //
    // According to the specs (see `gitprotocol-http(5)`), if the request includes the
    // `Git-Protocol: version=1` header an extra PKT_LINE `000dversion 1` shoule be inserted before
    // the first ref. However, GitHub doesn't implement that, and neither do we: it should just
    // look like we only support version 1, which is true.
    let stdout = repo.advertise_refs()?;
    let output = b"001e# service=git-upload-pack\n0000".chain(stdout);
    let output = ReaderStream::new(output);
    Ok((
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/x-git-upload-pack-advertisement",
            ),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from_stream(output),
    )
        .into_response())
}

// "Smart" protocol client step 2: compute.
async fn handle_upload_pack(repo: Arc<Mutex<Repo>>, request: Request) -> Result<Response> {
    // FIXME: should only drop this guard after child git-upload-pack exits.
    let repo = repo.lock().await;

    // Authenticate (discard the remote head).
    let auth = request.headers().get(header::AUTHORIZATION).cloned();
    let _ = repo.authenticate_with_head(auth).await?;

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
            (header::CONTENT_TYPE, "application/x-git-upload-pack-result"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        Body::from_stream(output),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use axum::body::Bytes;
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

        let mut mock_git = Git::default();

        mock_git
            .expect_init()
            .with(eq(config.cache_dir.join("example.com/a/b/c.git")))
            .times(1)
            .returning(|_| Ok(()));

        mock_git
            .expect_authenticate_with_head()
            .with(eq(Uri::from_static("https://example.com/a/b/c")), eq(None))
            .times(1)
            .returning(|_, _| Ok(Some(String::from("refs/heads/mock"))));

        mock_git
            .expect_fetch()
            .with(
                eq(Uri::from_static("https://example.com/a/b/c")),
                eq(config.cache_dir.join("example.com/a/b/c.git")),
                eq(None),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

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
            Vec::from_iter(response.headers().get_all(header::CONTENT_TYPE).into_iter()),
            ["application/x-git-upload-pack-advertisement"]
        );

        assert_eq!(
            Vec::from_iter(
                response
                    .headers()
                    .get_all(header::CACHE_CONTROL)
                    .into_iter()
            ),
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

        let mut mock_git = Git::default();

        mock_git.expect_init().times(1).returning(|_| Ok(()));

        mock_git
            .expect_authenticate_with_head()
            .times(2)
            .returning(|_, _| Ok(Some(String::from("refs/heads/mock"))));

        mock_git.expect_fetch().times(2).returning(|_, _, _| Ok(()));

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

        let mut mock_git = Git::default();

        mock_git.expect_init().times(1).returning(|_| Ok(()));

        mock_git
            .expect_authenticate_with_head()
            .with(eq(Uri::from_static("https://example.com/a/b/c")), eq(None))
            .times(1)
            .returning(|_, _| Ok(None));

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
            Vec::from_iter(response.headers().get_all(header::CONTENT_TYPE).into_iter()),
            ["application/x-git-upload-pack-result"]
        );

        assert_eq!(
            Vec::from_iter(
                response
                    .headers()
                    .get_all(header::CACHE_CONTROL)
                    .into_iter()
            ),
            ["no-cache"]
        );

        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "mock git-upload-pack output"
        );
    }

    #[tokio::test]
    async fn compressed_upload_pack_request() {
        // NOTE: Assumes that basic uplaod_pack without compressed requests has passed.

        let config = Options {
            cache_dir: tempdir().unwrap().into_path(),
            port: 0,
        };

        let mut mock_git = Git::default();

        mock_git.expect_init().times(1).returning(|_| Ok(()));

        mock_git
            .expect_authenticate_with_head()
            .times(1)
            .returning(|_, _| Ok(None));

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
    async fn authentication() {
        let config = Options {
            cache_dir: tempdir().unwrap().into_path(),
            port: 0,
        };

        let mut mock_git = Git::default();

        mock_git.expect_init().times(1).returning(|_| Ok(()));

        mock_git
            .expect_authenticate_with_head()
            .times(2)
            .with(
                eq(Uri::from_static("https://example.com/a/b/c")),
                eq(Some(HeaderValue::from_static("mock auth"))),
            )
            .returning(|_, _| Ok(Some(String::from("refs/heads/mock"))));

        mock_git
            .expect_fetch()
            .with(
                eq(Uri::from_static("https://example.com/a/b/c")),
                eq(config.cache_dir.join("example.com/a/b/c.git")),
                eq(Some(HeaderValue::from_static("mock auth"))),
            )
            .times(1)
            .returning(|_, _, _| Ok(()));

        mock_git
            .expect_advertise_refs()
            .times(1)
            .returning(|_| Ok(Box::new([].as_slice())));

        mock_git
            .expect_upload_pack()
            .times(1)
            .returning(|_, _| Ok(Box::new([].as_slice())));

        let mut app = app(&config, mock_git).await.unwrap();

        let refs = app
            .call(
                Request::get("/example.com/a/b/c/info/refs?service=git-upload-pack")
                    .header(header::AUTHORIZATION, "mock auth")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let upload_pack = app
            .oneshot(
                Request::post("/example.com/a/b/c/git-upload-pack")
                    .header(header::AUTHORIZATION, "mock auth")
                    .body(Body::from("mock client input: 42"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(refs.status(), StatusCode::OK);
        assert_eq!(upload_pack.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn non_existent_repository() {
        let config = Options {
            cache_dir: tempdir().unwrap().into_path(),
            port: 0,
        };

        let mut mock_git = Git::default();

        // TODO: don't initialize a local repo for non-existent upstreams
        mock_git.expect_init().times(0..).returning(|_| Ok(()));

        mock_git
            .expect_authenticate_with_head()
            .returning(|_, _| Err(Error::NotFound));

        let mut app = app(&config, mock_git).await.unwrap();

        let refs = app
            .call(
                Request::get("/example.com/a/b/c/info/refs?service=git-upload-pack")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let upload_pack = app
            .oneshot(
                Request::post("/example.com/a/b/c/git-upload-pack")
                    .body(Body::from("mock client input: 42"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(refs.status(), StatusCode::NOT_FOUND);
        assert_eq!(upload_pack.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn requires_authentication() {
        let config = Options {
            cache_dir: tempdir().unwrap().into_path(),
            port: 0,
        };

        let mut mock_git = Git::default();

        // TODO: don't initialize a local repo before upstream authorizes the client
        mock_git.expect_init().times(0..).returning(|_| Ok(()));

        mock_git.expect_authenticate_with_head().returning(|_, _| {
            Err(Error::MissingAuth(HeaderValue::from_static(
                "mock authenticate",
            )))
        });

        let mut app = app(&config, mock_git).await.unwrap();

        let refs = app
            .call(
                Request::get("/example.com/a/b/c/info/refs?service=git-upload-pack")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let upload_pack = app
            .oneshot(
                Request::post("/example.com/a/b/c/git-upload-pack")
                    .body(Body::from("mock client input: 42"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(refs.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            Vec::from_iter(refs.headers().get_all(header::WWW_AUTHENTICATE).into_iter()),
            ["mock authenticate"]
        );

        assert_eq!(upload_pack.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            Vec::from_iter(
                upload_pack
                    .headers()
                    .get_all(header::WWW_AUTHENTICATE)
                    .into_iter()
            ),
            ["mock authenticate"]
        );
    }

    // TODO: support or at least don't break with git protocol v2
}
