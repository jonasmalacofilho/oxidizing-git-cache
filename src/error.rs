use axum::{
    http::{HeaderValue, StatusCode},
    response::IntoResponse,
};
use reqwest::header::WWW_AUTHENTICATE;

pub type Result<T> = std::result::Result<T, Error>;

/// Server errrors.
///
/// These errors are for our benefit only, the client will just get a StatusCode.
///
/// There are only a few types of error conditions we need to care about. The first three are
/// modelled using this `Error` type:
///
/// - the few specific cases where we want to reply with NOT_FOUND or BAD_REQUEST;
/// - (future) handling an UNAUTHORIZED response from `Git::remote_head`;
/// - internal server errors that cannot be recovered within that request (but that are presumed to
///   *not* affect all other/future requests) and can be type erased.
///
/// Additionally, server-wide non-recoverable errors are modelled with panics. And we build with
/// `panic = "abort"`.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("not found")]
    NotFound,
    #[error("not authenticated/authorized")]
    MissingAuth(HeaderValue),
    // TODO: refuse upstream not in allowlist
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        match self {
            Error::Other(err) => {
                // TODO: log the backtrace as well
                tracing::error!(error = format_args!("{:#?}", err), "internal server error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "sorry, something went terrible wrong here",
                )
                    .into_response()
            }
            Error::NotFound => StatusCode::NOT_FOUND.into_response(),
            Error::MissingAuth(authenticate) => {
                (StatusCode::UNAUTHORIZED, [(WWW_AUTHENTICATE, authenticate)]).into_response()
            }
        }
    }
}
