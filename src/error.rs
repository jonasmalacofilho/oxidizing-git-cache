use axum::{http::StatusCode, response::IntoResponse};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    StatusCode { status: StatusCode },
    Io { source: std::io::Error },
    Git { description: &'static str },
    Other,
}

impl IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        // TODO: more useful responses
        match self {
            Error::StatusCode { status } => status.into_response(),
            _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }
}

impl From<StatusCode> for Error {
    fn from(status: StatusCode) -> Self {
        Error::StatusCode { status }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io { source }
    }
}

impl From<axum::Error> for Error {
    fn from(_: axum::Error) -> Self {
        Error::Other
    }
}

impl From<axum::http::uri::InvalidUri> for Error {
    fn from(_: axum::http::uri::InvalidUri) -> Self {
        Error::Other
    }
}
