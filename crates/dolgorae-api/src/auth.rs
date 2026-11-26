//! Peer credential and per-request bearer authentication for the local UDS API.

use std::path::PathBuf;

use axum::Json;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use dolgorae_core::{CallerContext, CoreEngine, CoreError};
use thiserror::Error;

/// Daemon startup and UDS binding errors.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("another daemon already holds exclusive lock at {}", .0.display())]
    AlreadyRunning(PathBuf),
    #[error("unsafe filesystem path: {0}")]
    UnsafePath(String),
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Pure same-UID peer credential check enforced on every UDS connection.
pub fn verify_peer_uid(peer_uid: u32, daemon_uid: u32) -> Result<(), CoreError> {
    if peer_uid == daemon_uid {
        Ok(())
    } else {
        Err(CoreError::unauthorized(format!(
            "peer uid {peer_uid} does not match daemon uid {daemon_uid}"
        )))
    }
}

/// Resolves per-request `CallerContext` from the optional `Authorization: Bearer` header.
///
/// - Absent `Authorization` header on a same-UID UDS connection -> `ExternalHost`.
/// - Present `Authorization` header -> must be `Bearer <token>` verified by `CoreEngine`.
pub async fn resolve_caller(
    engine: &CoreEngine,
    headers: &HeaderMap,
) -> Result<CallerContext, CoreError> {
    let Some(raw) = headers.get(header::AUTHORIZATION) else {
        return Ok(CallerContext::ExternalHost);
    };
    let text = raw
        .to_str()
        .map_err(|_| CoreError::unauthorized("malformed Authorization header"))?;
    let Some(token) = text.strip_prefix("Bearer ") else {
        return Err(CoreError::unauthorized(
            "Authorization header must use Bearer scheme",
        ));
    };
    let token = token.trim();
    if token.is_empty() {
        return Err(CoreError::unauthorized("empty bearer token"));
    }
    let scope = engine.authenticate_actor_token(token).await?;
    Ok(CallerContext::ScopedActor(scope))
}

/// Converts a `CoreError` into an HTTP JSON `ErrorEnvelope` response with its canonical status code.
pub fn error_response(err: CoreError) -> Response {
    let status =
        StatusCode::from_u16(err.code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(err.to_envelope())).into_response()
}
