//! Reusable HTTP/1.1-over-Unix-Domain-Socket client shared by `dolgorae-mcp` and `dolgorae-cli`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use dolgorae_core::{ArtifactRef, CoreError, ErrorCode, ErrorEnvelope};
use http::{HeaderValue, Method, Request, StatusCode, header};
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::Frame;
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use zeroize::Zeroizing;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(65);

/// Parsed Server-Sent Event frame returned by `SseStream::next_frame`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub id: Option<String>,
    pub event: Option<String>,
    pub data: String,
}

/// Byte-buffered UTF-8-safe Server-Sent Events parser.
///
/// Accumulates raw bytes across arbitrary chunk boundaries (including mid-UTF-8
/// codepoints) and decodes UTF-8 only once a complete `\n\n` / `\r\n\r\n` frame
/// delimiter has been received.
#[derive(Debug, Default)]
pub struct SseByteBuffer {
    buf: Vec<u8>,
}

impl SseByteBuffer {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn push_bytes(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    pub fn pop_frame(&mut self) -> Option<SseFrame> {
        loop {
            let (pos, delim_len) = find_sse_delimiter(&self.buf)?;
            let raw_block: Vec<u8> = self.buf.drain(..pos).collect();
            self.buf.drain(..delim_len);
            let block = String::from_utf8_lossy(&raw_block);
            if let Some(frame) = parse_sse_block(&block) {
                return Some(frame);
            }
        }
    }

    pub fn finish(&mut self) -> Option<SseFrame> {
        if self.buf.iter().all(|b| b.is_ascii_whitespace()) {
            self.buf.clear();
            return None;
        }
        let raw_block = std::mem::take(&mut self.buf);
        let block = String::from_utf8_lossy(&raw_block);
        parse_sse_block(&block)
    }
}

fn find_sse_delimiter(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0usize;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if i + 3 < buf.len()
            && buf[i] == b'\r'
            && buf[i + 1] == b'\n'
            && buf[i + 2] == b'\r'
            && buf[i + 3] == b'\n'
        {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

/// Streaming SSE reader backed by an open HTTP/1.1-over-UDS response body.
pub struct SseStream {
    body: hyper::body::Incoming,
    buf: SseByteBuffer,
    _conn_task: tokio::task::JoinHandle<()>,
}

impl SseStream {
    /// Reads the next complete SSE frame (`\n\n` delimited), returning `Ok(None)` on clean EOF.
    pub async fn next_frame(&mut self) -> Result<Option<SseFrame>, CoreError> {
        loop {
            if let Some(frame) = self.buf.pop_frame() {
                return Ok(Some(frame));
            }
            match self.body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        self.buf.push_bytes(&data);
                    }
                }
                Some(Err(err)) => {
                    return Err(CoreError::internal(format!("SSE body read error: {err}")));
                }
                None => return Ok(self.buf.finish()),
            }
        }
    }
}

fn parse_sse_block(block: &str) -> Option<SseFrame> {
    let mut id = None;
    let mut event = None;
    let mut data_lines: Vec<&str> = Vec::new();
    for line in block.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(v) = line.strip_prefix("id:") {
            id = Some(v.trim_start().to_owned());
        } else if let Some(v) = line.strip_prefix("event:") {
            event = Some(v.trim_start().to_owned());
        } else if let Some(v) = line.strip_prefix("data:") {
            data_lines.push(v.strip_prefix(' ').unwrap_or(v));
        }
    }
    if id.is_none() && event.is_none() && data_lines.is_empty() {
        return None;
    }
    Some(SseFrame {
        id,
        event,
        data: data_lines.join("\n"),
    })
}

/// HTTP/1.1 client communicating with a local `dolgorae serve` Unix domain socket.
#[derive(Clone)]
pub struct UdsClient {
    socket_path: PathBuf,
    bearer_token: Option<Zeroizing<String>>,
}

impl UdsClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            bearer_token: None,
        }
    }

    pub fn with_bearer_token(mut self, token: Zeroizing<String>) -> Self {
        self.bearer_token = Some(token);
        self
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path_and_query: &str,
    ) -> Result<T, CoreError> {
        let (status, bytes) = self
            .send_full(Method::GET, path_and_query, None, Bytes::new())
            .await?;
        decode_json_response(status, &bytes)
    }

    pub async fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        path_and_query: &str,
        body: &B,
    ) -> Result<T, CoreError> {
        let payload = serde_json::to_vec(body)
            .map_err(|e| CoreError::internal(format!("serialize request failed: {e}")))?;
        let (status, bytes) = self
            .send_full(
                Method::POST,
                path_and_query,
                Some("application/json"),
                Bytes::from(payload),
            )
            .await?;
        decode_json_response(status, &bytes)
    }

    pub async fn post_empty<T: DeserializeOwned>(
        &self,
        path_and_query: &str,
    ) -> Result<T, CoreError> {
        let (status, bytes) = self
            .send_full(Method::POST, path_and_query, None, Bytes::new())
            .await?;
        decode_json_response(status, &bytes)
    }

    pub async fn delete_json<T: DeserializeOwned>(
        &self,
        path_and_query: &str,
    ) -> Result<T, CoreError> {
        let (status, bytes) = self
            .send_full(Method::DELETE, path_and_query, None, Bytes::new())
            .await?;
        decode_json_response(status, &bytes)
    }

    /// Uploads raw bytes to `POST /v2/artifacts`.
    pub async fn post_artifact_bytes(
        &self,
        media_type: &str,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, CoreError> {
        let path = format!("/v2/artifacts?media_type={}", url_encode(media_type));
        let (status, resp) = self
            .send_full(
                Method::POST,
                &path,
                Some("application/octet-stream"),
                Bytes::from(bytes),
            )
            .await?;
        decode_json_response(status, &resp)
    }

    /// Streams chunks to `POST /v2/artifacts` without buffering the full payload in memory.
    pub async fn post_artifact_stream(
        &self,
        media_type: &str,
        rx: mpsc::Receiver<Result<Frame<Bytes>, std::io::Error>>,
    ) -> Result<ArtifactRef, CoreError> {
        let path = format!("/v2/artifacts?media_type={}", url_encode(media_type));
        let stream = connect_uds_timeout(&self.socket_path).await?;
        let (mut sender, conn) =
            tokio::time::timeout(CONNECT_TIMEOUT, http1::handshake(TokioIo::new(stream)))
                .await
                .map_err(|_| CoreError::internal("HTTP/1.1 handshake timed out"))?
                .map_err(|e| CoreError::internal(format!("HTTP/1.1 handshake failed: {e}")))?;
        let conn_task = tokio::spawn(async move {
            let _ = conn.await;
        });

        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(&path)
            .header(header::HOST, "localhost")
            .header(header::CONTENT_TYPE, "application/octet-stream");
        if let Some(ref tok) = self.bearer_token {
            builder = builder.header(header::AUTHORIZATION, build_bearer_header(tok)?);
        }
        let body = StreamBody::new(ReceiverStream::new(rx));
        let req = builder
            .body(body)
            .map_err(|e| CoreError::internal(format!("failed to build request: {e}")))?;

        let resp = tokio::time::timeout(REQUEST_TIMEOUT, sender.send_request(req))
            .await
            .map_err(|_| CoreError::internal("UDS streaming request timed out"))?
            .map_err(|e| CoreError::internal(format!("UDS streaming request failed: {e}")))?;
        let status = resp.status();
        let collected = tokio::time::timeout(REQUEST_TIMEOUT, resp.into_body().collect())
            .await
            .map_err(|_| CoreError::internal("UDS streaming response timed out"))?
            .map_err(|e| CoreError::internal(format!("read response failed: {e}")))?
            .to_bytes();
        conn_task.abort();
        decode_json_response(status, &collected)
    }

    /// Downloads a bounded artifact slice from `GET /v2/artifacts/{id}`.
    pub async fn get_artifact_slice(
        &self,
        artifact_id: &str,
        offset: u64,
        max_bytes: usize,
    ) -> Result<Vec<u8>, CoreError> {
        let path = format!(
            "/v2/artifacts/{}?offset={offset}&max_bytes={max_bytes}",
            url_encode(artifact_id)
        );
        let (status, bytes) = self
            .send_full(Method::GET, &path, None, Bytes::new())
            .await?;
        if status.is_success() {
            Ok(bytes.to_vec())
        } else {
            Err(decode_error_response(status, &bytes))
        }
    }

    /// Opens an SSE event stream (`GET /v2/sessions/{id}/events`).
    pub async fn open_sse(
        &self,
        path_and_query: &str,
        last_event_id: Option<&str>,
    ) -> Result<SseStream, CoreError> {
        let stream = connect_uds_timeout(&self.socket_path).await?;
        let (mut sender, conn) =
            tokio::time::timeout(CONNECT_TIMEOUT, http1::handshake(TokioIo::new(stream)))
                .await
                .map_err(|_| CoreError::internal("SSE HTTP/1.1 handshake timed out"))?
                .map_err(|e| CoreError::internal(format!("HTTP/1.1 handshake failed: {e}")))?;
        let conn_task = tokio::spawn(async move {
            let _ = conn.await;
        });

        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(path_and_query)
            .header(header::HOST, "localhost")
            .header(header::ACCEPT, "text/event-stream");
        if let Some(ref tok) = self.bearer_token {
            builder = builder.header(header::AUTHORIZATION, build_bearer_header(tok)?);
        }
        if let Some(id) = last_event_id {
            builder = builder.header("last-event-id", id);
        }
        let req = builder
            .body(Full::new(Bytes::new()))
            .map_err(|e| CoreError::internal(format!("failed to build SSE request: {e}")))?;

        let resp = tokio::time::timeout(CONNECT_TIMEOUT, sender.send_request(req))
            .await
            .map_err(|_| CoreError::internal("SSE open timed out"))?
            .map_err(|e| CoreError::internal(format!("SSE request failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let bytes = tokio::time::timeout(CONNECT_TIMEOUT, resp.into_body().collect())
                .await
                .map_err(|_| CoreError::internal("SSE error body timed out"))?
                .map_err(|e| CoreError::internal(format!("read error body failed: {e}")))?
                .to_bytes();
            conn_task.abort();
            return Err(decode_error_response(status, &bytes));
        }

        Ok(SseStream {
            body: resp.into_body(),
            buf: SseByteBuffer::new(),
            _conn_task: conn_task,
        })
    }

    async fn send_full(
        &self,
        method: Method,
        path_and_query: &str,
        content_type: Option<&str>,
        payload: Bytes,
    ) -> Result<(StatusCode, Bytes), CoreError> {
        let stream = connect_uds_timeout(&self.socket_path).await?;
        let (mut sender, conn) =
            tokio::time::timeout(CONNECT_TIMEOUT, http1::handshake(TokioIo::new(stream)))
                .await
                .map_err(|_| CoreError::internal("HTTP/1.1 handshake timed out"))?
                .map_err(|e| CoreError::internal(format!("HTTP/1.1 handshake failed: {e}")))?;
        let conn_task = tokio::spawn(async move {
            let _ = conn.await;
        });

        let mut builder = Request::builder()
            .method(method)
            .uri(path_and_query)
            .header(header::HOST, "localhost");
        if let Some(ct) = content_type {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        if let Some(ref tok) = self.bearer_token {
            builder = builder.header(header::AUTHORIZATION, build_bearer_header(tok)?);
        }
        let req = builder
            .body(Full::new(payload))
            .map_err(|e| CoreError::internal(format!("failed to build request: {e}")))?;

        let resp = tokio::time::timeout(REQUEST_TIMEOUT, sender.send_request(req))
            .await
            .map_err(|_| CoreError::internal("UDS request timed out"))?
            .map_err(|e| CoreError::internal(format!("UDS request failed: {e}")))?;
        let status = resp.status();
        let bytes = tokio::time::timeout(REQUEST_TIMEOUT, resp.into_body().collect())
            .await
            .map_err(|_| CoreError::internal("UDS response read timed out"))?
            .map_err(|e| CoreError::internal(format!("read response failed: {e}")))?
            .to_bytes();
        conn_task.abort();
        Ok((status, bytes))
    }
}

async fn connect_uds_timeout(socket_path: &Path) -> Result<UnixStream, CoreError> {
    tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(socket_path))
        .await
        .map_err(|_| {
            CoreError::internal(format!(
                "timed out connecting to UDS '{}'",
                socket_path.display()
            ))
        })?
        .map_err(|e| {
            CoreError::internal(format!(
                "failed to connect to UDS '{}': {e}",
                socket_path.display()
            ))
        })
}

fn build_bearer_header(token: &Zeroizing<String>) -> Result<HeaderValue, CoreError> {
    let formatted = Zeroizing::new(format!("Bearer {}", token.as_str()));
    let mut val = HeaderValue::from_str(&formatted)
        .map_err(|_| CoreError::unauthorized("invalid characters in bearer token"))?;
    val.set_sensitive(true);
    Ok(val)
}

fn decode_json_response<T: DeserializeOwned>(
    status: StatusCode,
    bytes: &[u8],
) -> Result<T, CoreError> {
    if status.is_success() {
        serde_json::from_slice(bytes)
            .map_err(|e| CoreError::internal(format!("invalid JSON response: {e}")))
    } else {
        Err(decode_error_response(status, bytes))
    }
}

fn decode_error_response(status: StatusCode, bytes: &[u8]) -> CoreError {
    if let Ok(env) = serde_json::from_slice::<ErrorEnvelope>(bytes) {
        let mut err = CoreError::new(env.error.code, env.error.message);
        if let Some(details) = env.error.details {
            err = err.with_details(details);
        }
        return err;
    }
    let code = match status.as_u16() {
        400 => ErrorCode::BadRequest,
        401 => ErrorCode::Unauthorized,
        403 => ErrorCode::ForbiddenScope,
        404 => ErrorCode::NotFound,
        409 => ErrorCode::SnapshotConflict,
        413 => ErrorCode::PayloadTooLarge,
        503 => ErrorCode::DriverUnavailable,
        _ => ErrorCode::InternalError,
    };
    let text = String::from_utf8_lossy(bytes).trim().to_owned();
    CoreError::new(
        code,
        if text.is_empty() {
            format!("HTTP {status}")
        } else {
            text
        },
    )
}

/// Minimal percent-encoder for query parameters and single path segments.
pub fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            use std::fmt::Write;
            let _ = write!(&mut out, "%{b:02X}");
        }
    }
    out
}
