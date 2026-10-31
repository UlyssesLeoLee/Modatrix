use axum::{
    extract::{Path, Request, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use ff3d_domain::{AssetDocument, EditOperation};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env,
    net::SocketAddr,
    sync::{Arc, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    store: Arc<RwLock<Store>>,
    api_token: Arc<String>,
}

#[derive(Default)]
struct Store {
    assets: HashMap<Uuid, AssetDocument>,
    history: HashMap<Uuid, Vec<AssetDocument>>,
    pending: HashMap<Uuid, PendingChangeSet>,
    commits: HashMap<String, CommitReceipt>,
    reviews: HashMap<Uuid, Vec<ReviewFrame>>,
}

#[derive(Clone)]
struct PendingChangeSet {
    asset_id: Uuid,
    base_version: u64,
    candidate: AssetDocument,
    idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewFrame {
    id: Uuid,
    asset_id: Uuid,
    asset_version: u64,
    created_at_unix_ms: u128,
    camera_position: [f32; 3],
    camera_target: [f32; 3],
    selected_anchor: Option<String>,
    motion_time_ms: Option<u64>,
    comment: String,
}

#[derive(Debug, Deserialize)]
struct NewReviewFrame {
    asset_version: u64,
    camera_position: [f32; 3],
    camera_target: [f32; 3],
    selected_anchor: Option<String>,
    motion_time_ms: Option<u64>,
    comment: String,
}

#[derive(Debug, Deserialize)]
struct PreviewRequest {
    base_version: u64,
    requested_by: String,
    idempotency_key: String,
    operations: Vec<EditOperation>,
}

#[derive(Debug, Serialize)]
struct PreviewResponse {
    changeset_id: Uuid,
    asset_id: Uuid,
    base_version: u64,
    operation_count: usize,
    candidate: AssetDocument,
    status: &'static str,
}

#[derive(Debug, Deserialize)]
struct CommitRequest {
    approved_by: String,
    idempotency_key: String,
}

#[derive(Debug, Clone, Serialize)]
struct CommitReceipt {
    commit_id: Uuid,
    asset_id: Uuid,
    previous_version: u64,
    new_version: u64,
    approved_by: String,
    idempotency_key: String,
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: String,
}

struct ApiError(StatusCode, &'static str, String);

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, "bad_request", message.into())
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self(StatusCode::NOT_FOUND, "not_found", message.into())
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self(StatusCode::CONFLICT, "conflict", message.into())
    }
    fn internal(message: impl Into<String>) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorBody { code: self.1, message: self.2 })).into_response()
    }
}

async fn auth_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let expected = state.api_token.as_str();
    if expected.is_empty() {
        return Ok(next.run(request).await);
    }
    let actual = request.headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if actual == format!("Bearer {expected}") {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "service": "frameforge3d-rust-api",
        "status": "ok",
        "implementation": "mvp"
    }))
}

async fn create_asset(
    State(state): State<AppState>,
    Json(mut asset): Json<AssetDocument>,
) -> Result<(StatusCode, Json<AssetDocument>), ApiError> {
    asset.version = 1;
    asset.validate().map_err(|error| ApiError::bad_request(error.to_string()))?;
    let mut store = state.store.write().map_err(|_| ApiError::internal("store lock poisoned"))?;
    if store.assets.contains_key(&asset.id) {
        return Err(ApiError::conflict("asset id already exists"));
    }
    store.history.insert(asset.id, vec![asset.clone()]);
    store.assets.insert(asset.id, asset.clone());
    Ok((StatusCode::CREATED, Json(asset)))
}

async fn get_asset(
    State(state): State<AppState>,
    Path(asset_id): Path<Uuid>,
) -> Result<Json<AssetDocument>, ApiError> {
    let store = state.store.read().map_err(|_| ApiError::internal("store lock poisoned"))?;
    let asset = store.assets.get(&asset_id).cloned()
        .ok_or_else(|| ApiError::not_found(format!("asset {asset_id}")))?;
    Ok(Json(asset))
}

async fn get_history(
    State(state): State<AppState>,
    Path(asset_id): Path<Uuid>,
) -> Result<Json<Vec<AssetDocument>>, ApiError> {
    let store = state.store.read().map_err(|_| ApiError::internal("store lock poisoned"))?;
    let history = store.history.get(&asset_id).cloned()
        .ok_or_else(|| ApiError::not_found(format!("asset {asset_id}")))?;
    Ok(Json(history))
}

async fn add_review(
    State(state): State<AppState>,
    Path(asset_id): Path<Uuid>,
    Json(request): Json<NewReviewFrame>,
) -> Result<(StatusCode, Json<ReviewFrame>), ApiError> {
    if request.comment.trim().is_empty() {
        return Err(ApiError::bad_request("review comment is empty"));
    }
    if !request.camera_position.iter().all(|value| value.is_finite())
        || !request.camera_target.iter().all(|value| value.is_finite())
    {
        return Err(ApiError::bad_request("camera values must be finite"));
    }
    let mut store = state.store.write().map_err(|_| ApiError::internal("store lock poisoned"))?;
    let asset = store.assets.get(&asset_id)
        .ok_or_else(|| ApiError::not_found(format!("asset {asset_id}")))?;
    if asset.version != request.asset_version {
        return Err(ApiError::conflict("review references a stale asset version"));
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)
        .map_err(|_| ApiError::internal("system clock is before UNIX epoch"))?
        .as_millis();
    let review = ReviewFrame {
        id: Uuid::new_v4(),
        asset_id,
        asset_version: request.asset_version,
        created_at_unix_ms: now,
        camera_position: request.camera_position,
        camera_target: request.camera_target,
        selected_anchor: request.selected_anchor,
        motion_time_ms: request.motion_time_ms,
        comment: request.comment,
    };
    store.reviews.entry(asset_id).or_default().push(review.clone());
    Ok((StatusCode::CREATED, Json(review)))
}

async fn preview_changeset(
    State(state): State<AppState>,
    Path(asset_id): Path<Uuid>,
    Json(request): Json<PreviewRequest>,
) -> Result<(StatusCode, Json<PreviewResponse>), ApiError> {
    if request.operations.is_empty() {
        return Err(ApiError::bad_request("changeset must contain at least one operation"));
    }
    if request.requested_by.trim().is_empty() || request.idempotency_key.trim().is_empty() {
        return Err(ApiError::bad_request("requested_by and idempotency_key are required"));
    }
    let mut store = state.store.write().map_err(|_| ApiError::internal("store lock poisoned"))?;
    let current = store.assets.get(&asset_id).cloned()
        .ok_or_else(|| ApiError::not_found(format!("asset {asset_id}")))?;
    if current.version != request.base_version {
        return Err(ApiError::conflict(format!(
            "stale base version: requested {}, current {}",
            request.base_version, current.version
        )));
    }
    let candidate = current.apply_operations(&request.operations)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let changeset_id = Uuid::new_v4();
    store.pending.insert(changeset_id, PendingChangeSet {
        asset_id,
        base_version: request.base_version,
        candidate: candidate.clone(),
        idempotency_key: request.idempotency_key,
    });
    Ok((StatusCode::CREATED, Json(PreviewResponse {
        changeset_id,
        asset_id,
        base_version: request.base_version,
        operation_count: request.operations.len(),
        candidate,
        status: "preview_ready",
    })))
}

async fn commit_changeset(
    State(state): State<AppState>,
    Path((asset_id, changeset_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<CommitRequest>,
) -> Result<Json<CommitReceipt>, ApiError> {
    if request.approved_by.trim().is_empty() || request.idempotency_key.trim().is_empty() {
        return Err(ApiError::bad_request("approved_by and idempotency_key are required"));
    }
    let mut store = state.store.write().map_err(|_| ApiError::internal("store lock poisoned"))?;
    if let Some(receipt) = store.commits.get(&request.idempotency_key) {
        if receipt.asset_id == asset_id {
            return Ok(Json(receipt.clone()));
        }
        return Err(ApiError::conflict("idempotency key was already used for another asset"));
    }
    let pending = store.pending.get(&changeset_id).cloned()
        .ok_or_else(|| ApiError::not_found(format!("changeset {changeset_id}")))?;
    if pending.asset_id != asset_id {
        return Err(ApiError::bad_request("changeset does not belong to this asset"));
    }
    if pending.idempotency_key != request.idempotency_key {
        return Err(ApiError::conflict("idempotency key does not match preview"));
    }
    let current = store.assets.get(&asset_id).cloned()
        .ok_or_else(|| ApiError::not_found(format!("asset {asset_id}")))?;
    if current.version != pending.base_version {
        return Err(ApiError::conflict("asset changed after preview; create a new preview"));
    }

    let previous_version = current.version;
    let mut committed = pending.candidate;
    committed.version = previous_version + 1;
    committed.validate().map_err(|error| ApiError::bad_request(error.to_string()))?;
    store.assets.insert(asset_id, committed.clone());
    store.history.entry(asset_id).or_default().push(committed);
    let receipt = CommitReceipt {
        commit_id: Uuid::new_v4(),
        asset_id,
        previous_version,
        new_version: previous_version + 1,
        approved_by: request.approved_by,
        idempotency_key: request.idempotency_key.clone(),
        status: "committed",
    };
    store.commits.insert(request.idempotency_key, receipt.clone());
    store.pending.remove(&changeset_id);
    Ok(Json(receipt))
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/assets", post(create_asset))
        .route("/api/v1/assets/{asset_id}", get(get_asset))
        .route("/api/v1/assets/{asset_id}/history", get(get_history))
        .route("/api/v1/assets/{asset_id}/reviews", post(add_review))
        .route("/api/v1/assets/{asset_id}/changesets/preview", post(preview_changeset))
        .route(
            "/api/v1/assets/{asset_id}/changesets/{changeset_id}/commit",
            post(commit_changeset),
        )
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let host = env::var("FF3D_API_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port = env::var("FF3D_API_PORT").ok().and_then(|value| value.parse().ok()).unwrap_or(8080);
    let token = env::var("FF3D_API_TOKEN").unwrap_or_else(|_| "dev-insecure-token".into());
    let state = AppState {
        store: Arc::new(RwLock::new(Store::default())),
        api_token: Arc::new(token),
    };
    let app = build_router(state);
    let address: SocketAddr = format!("{host}:{port}").parse().expect("valid bind address");
    let listener = tokio::net::TcpListener::bind(address).await.expect("bind API listener");
    tracing::info!(%address, "FrameForge 3D Rust API listening");
    axum::serve(listener, app).await.expect("serve API");
}
