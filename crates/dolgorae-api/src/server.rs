//! Exclusive daemon locking, safe Unix domain socket binding, and the 19-route
//! Axum HTTP/1.1 JSON + SSE router.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::future::Future;
use std::io::{self, Read};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bytes::Bytes;
use dolgorae_core::{
    AnswerInteractionRequest, CORE_VERSION, CoreEngine, CoreError, CreateSessionRequest, Cursor,
    InteractionStatus, MAX_PAGE_BYTES, MAX_RECEIVE_BYTES, PostMailboxRequest, ResumeSessionRequest,
    SessionFilter, SessionStatus, SubmitTurnRequest, WaitSessionsRequest,
};
use futures_util::StreamExt;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tower::ServiceExt;

use crate::API_VERSION;
use crate::auth::{ApiError, error_response, resolve_caller, verify_peer_uid};
use crate::sse::{build_session_sse, parse_after_cursor};

/// Exclusive per-home daemon lock held for the lifetime of `dolgorae serve`.
///
/// MUST be acquired BEFORE `CoreEngine::new` / `Store::open` so a duplicate
/// daemon never runs startup recovery on a live daemon's SQLite state.
pub struct DaemonLock {
    _file: File,
    uid: u32,
}

impl DaemonLock {
    /// Validates `<home_dir>` and `<home_dir>/run` ownership & `0700` permissions
    /// and acquires an exclusive non-blocking lock on `<home_dir>/run/dolgorae.lock`.
    pub fn acquire(home_dir: &Path) -> Result<Self, ApiError> {
        ensure_private_dir(home_dir)?;
        let run_dir = home_dir.join("run");
        ensure_private_dir(&run_dir)?;

        let lock_path = run_dir.join("dolgorae.lock");
        reject_symlink(&lock_path)?;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)?;
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))?;

        let uid = file.metadata()?.uid();
        verify_dir_owner_and_mode(home_dir, uid)?;
        verify_dir_owner_and_mode(&run_dir, uid)?;

        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file, uid }),
            Err(TryLockError::WouldBlock) => Err(ApiError::AlreadyRunning(lock_path)),
            Err(TryLockError::Error(err)) => Err(ApiError::Io(err)),
        }
    }

    /// Returns the safe effective UID observed from the owned lock file metadata.
    pub fn uid(&self) -> u32 {
        self.uid
    }
}

/// Configuration for the local UDS HTTP/1.1 server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub socket_path: PathBuf,
    pub heartbeat_interval: Duration,
}

impl ServerConfig {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            heartbeat_interval: Duration::from_secs(15),
        }
    }
}

#[derive(Clone)]
struct AppState {
    engine: Arc<CoreEngine>,
    heartbeat_interval: Duration,
}

/// A bound local Unix domain socket server holding its exclusive `DaemonLock`.
pub struct BoundUdsServer {
    lock: DaemonLock,
    listener: UnixListener,
    socket_path: PathBuf,
    bound_inode: u64,
    engine: Arc<CoreEngine>,
    heartbeat_interval: Duration,
}

impl BoundUdsServer {
    /// Safely binds `config.socket_path` while holding `lock`.
    ///
    /// Never unlinks non-socket paths (regular files, symlinks, dirs, fifos),
    /// foreign-owned sockets, or live sockets already accepting connections.
    pub async fn bind(
        lock: DaemonLock,
        engine: Arc<CoreEngine>,
        config: ServerConfig,
    ) -> Result<Self, ApiError> {
        let socket_path = config.socket_path;
        if let Some(parent) = socket_path.parent() {
            ensure_private_dir(parent)?;
            verify_dir_owner_and_mode(parent, lock.uid())?;
        }

        if let Ok(meta) = fs::symlink_metadata(&socket_path) {
            if !meta.file_type().is_socket() {
                return Err(ApiError::UnsafePath(format!(
                    "refusing to overwrite non-socket path '{}'",
                    socket_path.display()
                )));
            }
            if meta.uid() != lock.uid() {
                return Err(ApiError::UnsafePath(format!(
                    "refusing to overwrite socket '{}' owned by uid {}",
                    socket_path.display(),
                    meta.uid()
                )));
            }
            match UnixStream::connect(&socket_path).await {
                Ok(_live) => return Err(ApiError::AlreadyRunning(socket_path)),
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                    ) =>
                {
                    fs::remove_file(&socket_path)?;
                }
                Err(err) => return Err(ApiError::Io(err)),
            }
        }

        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;
        let bound_inode = fs::symlink_metadata(&socket_path)?.ino();

        Ok(Self {
            lock,
            listener,
            socket_path,
            bound_inode,
            engine,
            heartbeat_interval: config.heartbeat_interval,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Serves HTTP/1.1 JSON + SSE connections until `shutdown` resolves, then
    /// closes all active driver sessions, unlinks the socket only if its inode
    /// still matches the bound socket, and releases the exclusive `DaemonLock`.
    pub async fn serve_until<F>(self, shutdown: F) -> Result<(), ApiError>
    where
        F: Future<Output = ()>,
    {
        let daemon_uid = self.lock.uid();
        let app = build_router(AppState {
            engine: Arc::clone(&self.engine),
            heartbeat_interval: self.heartbeat_interval,
        });

        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                accepted = self.listener.accept() => {
                    match accepted {
                        Ok((stream, _addr)) => {
                            let peer_uid = match stream.peer_cred() {
                                Ok(cred) => cred.uid(),
                                Err(err) => {
                                    tracing::warn!(%err, "rejecting UDS connection: peer_cred failed");
                                    continue;
                                }
                            };
                            if let Err(err) = verify_peer_uid(peer_uid, daemon_uid) {
                                tracing::warn!(%err, "rejecting UDS connection with mismatched peer uid");
                                continue;
                            }
                            let app_clone = app.clone();
                            tokio::spawn(async move {
                                let io = TokioIo::new(stream);
                                let svc = service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                                    let router = app_clone.clone();
                                    async move {
                                        router.oneshot(req.map(Body::new)).await
                                    }
                                });
                                if let Err(err) = http1::Builder::new().serve_connection(io, svc).await {
                                    tracing::debug!(%err, "UDS HTTP/1.1 connection closed");
                                }
                            });
                        }
                        Err(err) => {
                            tracing::warn!(%err, "UDS accept error");
                        }
                    }
                }
            }
        }

        // Graceful teardown: close all live drivers, unlink only matching inode, drop lock.
        self.engine.shutdown_all_sessions().await;
        if let Ok(meta) = fs::symlink_metadata(&self.socket_path)
            && meta.ino() == self.bound_inode
        {
            let _ = fs::remove_file(&self.socket_path);
        }
        drop(self.lock);
        Ok(())
    }
}

fn build_router(state: AppState) -> Router {
    let artifact_upload = Router::new()
        .route("/v2/artifacts", post(create_artifact))
        .layer(DefaultBodyLimit::disable());

    Router::new()
        .route("/v2/health", get(get_health))
        .route("/v2/capabilities", get(get_capabilities))
        .route("/v2/roles", get(list_roles))
        .route("/v2/roles/{name}", get(get_role))
        .route("/v2/sessions", get(list_sessions).post(create_session))
        .route("/v2/sessions/wait", post(wait_sessions))
        .route(
            "/v2/sessions/{session_id}",
            get(get_session).delete(close_session),
        )
        .route("/v2/sessions/{session_id}/resume", post(resume_session))
        .route("/v2/sessions/{session_id}/turns", post(submit_turn))
        .route(
            "/v2/sessions/{session_id}/turns/{turn_id}/interrupt",
            post(interrupt_turn),
        )
        .route("/v2/sessions/{session_id}/timeline", get(get_timeline))
        .route("/v2/sessions/{session_id}/events", get(stream_events))
        .route(
            "/v2/sessions/{session_id}/interactions",
            get(list_interactions),
        )
        .route(
            "/v2/sessions/{session_id}/interactions/{interaction_id}/answer",
            post(answer_interaction),
        )
        .route(
            "/v2/sessions/{session_id}/mailbox",
            get(list_mailbox).post(post_mailbox),
        )
        .route(
            "/v2/workspaces/lease",
            post(acquire_writer_lease).delete(release_writer_lease),
        )
        .route("/v2/workspaces/lease/transfer", post(transfer_writer_lease))
        .route("/v2/artifacts/{artifact_id}", get(get_artifact))
        .fallback(not_found_handler)
        .layer(DefaultBodyLimit::max(MAX_RECEIVE_BYTES + 1))
        .merge(artifact_upload)
        .with_state(state)
}

async fn not_found_handler() -> Response {
    error_response(CoreError::not_found("route not found"))
}

async fn collect_bounded_body(body: Body) -> Result<Bytes, CoreError> {
    let limited = Limited::new(body, MAX_RECEIVE_BYTES);
    match limited.collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(err) => {
            if err.downcast_ref::<LengthLimitError>().is_some() {
                Err(CoreError::payload_too_large(format!(
                    "request body exceeds maximum receive limit of {MAX_RECEIVE_BYTES} bytes"
                )))
            } else {
                Err(CoreError::bad_request(format!(
                    "failed to read request body: {err}"
                )))
            }
        }
    }
}

async fn read_json_body<T: DeserializeOwned>(body: Body) -> Result<T, CoreError> {
    let bytes = collect_bounded_body(body).await?;
    serde_json::from_slice(&bytes)
        .map_err(|err| CoreError::bad_request(format!("invalid JSON body: {err}")))
}

async fn read_optional_json_body<T: DeserializeOwned + Default>(
    body: Body,
) -> Result<T, CoreError> {
    let bytes = collect_bounded_body(body).await?;
    if bytes.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_slice(&bytes)
        .map_err(|err| CoreError::bad_request(format!("invalid JSON body: {err}")))
}

fn parse_opt_usize(
    params: &HashMap<String, String>,
    key: &str,
) -> Result<Option<usize>, CoreError> {
    match params.get(key) {
        None => Ok(None),
        Some(raw) => raw
            .parse::<usize>()
            .map(Some)
            .map_err(|_| CoreError::bad_request(format!("invalid integer query param '{key}'"))),
    }
}

fn parse_opt_u64(params: &HashMap<String, String>, key: &str) -> Result<Option<u64>, CoreError> {
    match params.get(key) {
        None => Ok(None),
        Some(raw) => raw
            .parse::<u64>()
            .map(Some)
            .map_err(|_| CoreError::bad_request(format!("invalid integer query param '{key}'"))),
    }
}

fn parse_opt_bool(params: &HashMap<String, String>, key: &str) -> Result<Option<bool>, CoreError> {
    match params.get(key).map(String::as_str) {
        None => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(CoreError::bad_request(format!(
            "invalid boolean query param '{key}'"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Route handlers (all 19 OpenAPI v2 routes)
// ---------------------------------------------------------------------------

async fn get_health() -> Response {
    Json(json!({
        "status": "ok",
        "version": CORE_VERSION,
        "api_version": API_VERSION,
        "pid": std::process::id(),
    }))
    .into_response()
}

async fn get_capabilities(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(err) = resolve_caller(&state.engine, &headers).await {
        return error_response(err);
    }
    match state.engine.capabilities().await {
        Ok(caps) => Json(caps).into_response(),
        Err(err) => error_response(err),
    }
}

#[derive(Deserialize)]
struct WorkspaceQuery {
    workspace: Option<PathBuf>,
}

async fn list_roles(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    if let Err(err) = resolve_caller(&state.engine, &headers).await {
        return error_response(err);
    }
    match state.engine.list_roles(q.workspace.as_deref()) {
        Ok(roles) => Json(json!({ "roles": roles })).into_response(),
        Err(err) => error_response(err),
    }
}

async fn get_role(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<WorkspaceQuery>,
) -> Response {
    if let Err(err) = resolve_caller(&state.engine, &headers).await {
        return error_response(err);
    }
    match state.engine.get_role(&name, q.workspace.as_deref()) {
        Ok(role) => Json(role).into_response(),
        Err(err) => error_response(err),
    }
}

#[derive(Deserialize)]
struct ListSessionsQuery {
    parent_session_id: Option<String>,
    root_session_id: Option<String>,
    status: Option<String>,
}

async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListSessionsQuery>,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let status = match q.status.as_deref() {
        Some(raw) => match SessionStatus::parse_str(raw) {
            Ok(s) => Some(s),
            Err(_) => {
                return error_response(CoreError::bad_request(format!(
                    "invalid session status '{raw}'"
                )));
            }
        },
        None => None,
    };
    let filter = SessionFilter {
        parent_session_id: q.parent_session_id,
        root_session_id: q.root_session_id,
        status,
    };
    match state.engine.list_sessions(&caller, filter).await {
        Ok(sessions) => Json(json!({ "sessions": sessions })).into_response(),
        Err(err) => error_response(err),
    }
}

async fn create_session(State(state): State<AppState>, headers: HeaderMap, body: Body) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let req: CreateSessionRequest = match read_json_body(body).await {
        Ok(r) => r,
        Err(err) => return error_response(err),
    };
    match state.engine.create_session(&caller, req).await {
        Ok(snap) => (StatusCode::CREATED, Json(snap)).into_response(),
        Err(err) => error_response(err),
    }
}

async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    match state.engine.get_session(&caller, &session_id).await {
        Ok(snap) => Json(snap).into_response(),
        Err(err) => error_response(err),
    }
}

async fn close_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    match state.engine.close_session(&caller, &session_id).await {
        Ok(snap) => Json(snap).into_response(),
        Err(err) => error_response(err),
    }
}

async fn resume_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    body: Body,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let req: ResumeSessionRequest = match read_optional_json_body(body).await {
        Ok(r) => r,
        Err(err) => return error_response(err),
    };
    match state.engine.resume_session(&caller, &session_id, req).await {
        Ok(snap) => Json(snap).into_response(),
        Err(err) => error_response(err),
    }
}

async fn submit_turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    body: Body,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let req: SubmitTurnRequest = match read_json_body(body).await {
        Ok(r) => r,
        Err(err) => return error_response(err),
    };
    match state.engine.submit_turn(&caller, &session_id, req).await {
        Ok(accepted) => (StatusCode::ACCEPTED, Json(accepted)).into_response(),
        Err(err) => error_response(err),
    }
}

async fn interrupt_turn(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((session_id, turn_id)): AxumPath<(String, String)>,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    match state
        .engine
        .interrupt_turn(&caller, &session_id, &turn_id)
        .await
    {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "session_id": session_id,
                "turn_id": turn_id,
                "status": "interrupt_requested",
            })),
        )
            .into_response(),
        Err(err) => error_response(err),
    }
}

async fn get_timeline(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let limit = match parse_opt_usize(&q, "limit") {
        Ok(v) => v.unwrap_or(100),
        Err(err) => return error_response(err),
    };
    let page_token = q.get("page_token").cloned();
    match state
        .engine
        .get_timeline(&caller, &session_id, page_token, limit)
        .await
    {
        Ok(page) => Json(page).into_response(),
        Err(err) => error_response(err),
    }
}

async fn stream_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let after = match parse_after_cursor(&headers, q.get("after").map(String::as_str)) {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    match build_session_sse(
        Arc::clone(&state.engine),
        caller,
        session_id,
        after,
        state.heartbeat_interval,
    )
    .await
    {
        Ok(sse) => sse.into_response(),
        Err(err) => error_response(err),
    }
}

async fn list_interactions(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let pending_only = match parse_opt_bool(&q, "pending_only") {
        Ok(v) => v.unwrap_or(true),
        Err(err) => return error_response(err),
    };
    let status = if pending_only {
        Some(InteractionStatus::Pending)
    } else {
        None
    };
    match state
        .engine
        .list_interactions(&caller, &session_id, status)
        .await
    {
        Ok(list) => Json(list).into_response(),
        Err(err) => error_response(err),
    }
}

async fn answer_interaction(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((session_id, interaction_id)): AxumPath<(String, String)>,
    body: Body,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let req: AnswerInteractionRequest = match read_json_body(body).await {
        Ok(r) => r,
        Err(err) => return error_response(err),
    };
    match state
        .engine
        .answer_interaction(&caller, &session_id, &interaction_id, req)
        .await
    {
        Ok(resp) => Json(resp).into_response(),
        Err(err) => error_response(err),
    }
}

async fn list_mailbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let limit = match parse_opt_usize(&q, "limit") {
        Ok(v) => v.unwrap_or(100),
        Err(err) => return error_response(err),
    };
    let page_token = if let Some(raw_after) = q.get("after") {
        match Cursor::parse(raw_after) {
            Ok(c) => Some(format!("tok:{}", c.value())),
            Err(err) => return error_response(err),
        }
    } else {
        q.get("page_token").cloned()
    };
    match state
        .engine
        .list_mailbox_messages(&caller, &session_id, page_token, limit)
        .await
    {
        Ok(page) => Json(page).into_response(),
        Err(err) => error_response(err),
    }
}

async fn post_mailbox(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(session_id): AxumPath<String>,
    body: Body,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let req: PostMailboxRequest = match read_json_body(body).await {
        Ok(r) => r,
        Err(err) => return error_response(err),
    };
    match state
        .engine
        .post_mailbox_message(&caller, &session_id, req)
        .await
    {
        Ok(msg) => (StatusCode::CREATED, Json(msg)).into_response(),
        Err(err) => error_response(err),
    }
}

async fn wait_sessions(State(state): State<AppState>, headers: HeaderMap, body: Body) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let req: WaitSessionsRequest = match read_json_body(body).await {
        Ok(r) => r,
        Err(err) => return error_response(err),
    };
    match state.engine.wait_sessions(&caller, req).await {
        Ok(resp) => Json(resp).into_response(),
        Err(err) => error_response(err),
    }
}

#[derive(Deserialize)]
struct AcquireLeaseBody {
    workspace: PathBuf,
    session_id: String,
}

async fn acquire_writer_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let req: AcquireLeaseBody = match read_json_body(body).await {
        Ok(r) => r,
        Err(err) => return error_response(err),
    };
    match state
        .engine
        .acquire_writer_lease(&caller, &req.workspace, &req.session_id)
        .await
    {
        Ok(lease) => Json(lease).into_response(),
        Err(err) => error_response(err),
    }
}

async fn release_writer_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let Some(workspace) = q.get("workspace").filter(|s| !s.is_empty()) else {
        return error_response(CoreError::bad_request(
            "missing required query param 'workspace'",
        ));
    };
    let Some(session_id) = q.get("session_id").filter(|s| !s.is_empty()) else {
        return error_response(CoreError::bad_request(
            "missing required query param 'session_id'",
        ));
    };
    match state
        .engine
        .release_writer_lease(&caller, Path::new(workspace), session_id)
        .await
    {
        Ok(released) => Json(json!({ "released": released })).into_response(),
        Err(err) => error_response(err),
    }
}

#[derive(Deserialize)]
struct TransferLeaseBody {
    workspace: PathBuf,
    from_session_id: String,
    to_session_id: String,
}

async fn transfer_writer_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let req: TransferLeaseBody = match read_json_body(body).await {
        Ok(r) => r,
        Err(err) => return error_response(err),
    };
    match state
        .engine
        .transfer_writer_lease(
            &caller,
            &req.workspace,
            &req.from_session_id,
            &req.to_session_id,
        )
        .await
    {
        Ok(lease) => Json(lease).into_response(),
        Err(err) => error_response(err),
    }
}

#[derive(Deserialize)]
struct CreateArtifactQuery {
    media_type: Option<String>,
}

struct ChannelReader {
    rx: mpsc::Receiver<io::Result<Bytes>>,
    buf: Bytes,
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.buf.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(chunk)) => self.buf = chunk,
                Some(Err(err)) => return Err(err),
                None => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len());
        out[..n].copy_from_slice(&self.buf[..n]);
        let _ = self.buf.split_to(n);
        Ok(n)
    }
}

async fn create_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CreateArtifactQuery>,
    body: Body,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let media_type = q
        .media_type
        .unwrap_or_else(|| "application/octet-stream".to_owned());

    let (tx, rx) = mpsc::channel::<io::Result<Bytes>>(8);
    let mut stream = body.into_data_stream();
    let feeder = tokio::spawn(async move {
        while let Some(chunk) = stream.next().await {
            let item = chunk.map_err(|e| io::Error::other(e.to_string()));
            if tx.send(item).await.is_err() {
                break;
            }
        }
    });

    let reader = ChannelReader {
        rx,
        buf: Bytes::new(),
    };
    let result = state
        .engine
        .create_artifact(&caller, media_type, reader)
        .await;
    feeder.abort();

    match result {
        Ok(art) => (StatusCode::CREATED, Json(art)).into_response(),
        Err(err) => error_response(err),
    }
}

async fn get_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(artifact_id): AxumPath<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let caller = match resolve_caller(&state.engine, &headers).await {
        Ok(c) => c,
        Err(err) => return error_response(err),
    };
    let offset = match parse_opt_u64(&q, "offset") {
        Ok(v) => v.unwrap_or(0),
        Err(err) => return error_response(err),
    };
    let max_bytes = match parse_opt_usize(&q, "max_bytes") {
        Ok(v) => v.unwrap_or(262_144),
        Err(err) => return error_response(err),
    };
    if max_bytes == 0 || max_bytes > MAX_PAGE_BYTES {
        return error_response(CoreError::bad_request(format!(
            "max_bytes must be between 1 and {MAX_PAGE_BYTES}"
        )));
    }
    match state
        .engine
        .read_artifact(&caller, &artifact_id, offset, max_bytes)
        .await
    {
        Ok((art, bytes)) => {
            let mut resp = (StatusCode::OK, bytes).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            if let Ok(val) = HeaderValue::from_str(&art.sha256) {
                resp.headers_mut().insert("x-dolgorae-sha256", val);
            }
            if let Ok(val) = HeaderValue::from_str(&art.media_type) {
                resp.headers_mut().insert("x-dolgorae-media-type", val);
            }
            resp
        }
        Err(err) => error_response(err),
    }
}

// ---------------------------------------------------------------------------
// Safe filesystem path checks (zero `unsafe`)
// ---------------------------------------------------------------------------

fn reject_symlink(path: &Path) -> Result<(), ApiError> {
    if let Ok(meta) = fs::symlink_metadata(path)
        && meta.file_type().is_symlink()
    {
        return Err(ApiError::UnsafePath(format!(
            "refusing symlink path '{}'",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_private_dir(dir: &Path) -> Result<(), ApiError> {
    reject_symlink(dir)?;
    if !dir.exists() {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        return Ok(());
    }
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(ApiError::UnsafePath(format!(
            "'{}' is not a directory",
            dir.display()
        )));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(ApiError::UnsafePath(format!(
            "directory '{}' has overly permissive mode {:04o} (must be 0700 or stricter)",
            dir.display(),
            mode
        )));
    }
    Ok(())
}

fn verify_dir_owner_and_mode(dir: &Path, expected_uid: u32) -> Result<(), ApiError> {
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(ApiError::UnsafePath(format!(
            "'{}' must be a regular directory",
            dir.display()
        )));
    }
    if meta.uid() != expected_uid {
        return Err(ApiError::UnsafePath(format!(
            "directory '{}' owned by uid {}, expected {}",
            dir.display(),
            meta.uid(),
            expected_uid
        )));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(ApiError::UnsafePath(format!(
            "directory '{}' has mode {:04o}, expected 0700 or stricter",
            dir.display(),
            mode
        )));
    }
    Ok(())
}
