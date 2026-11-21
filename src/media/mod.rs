//! 媒体 / 文件库：租户内网盘式存储（上传 / 列表 / 下载 / 删除）。
//!
//! - 元数据进租户库 `media_assets`，文件由 [`storage::MediaStorage`] 落盘（当前 local，
//!   S3/WebDAV 预留后端）；
//! - 任何文件可存；只有魔数校验通过的位图（PNG/JPEG/WebP/GIF）允许 inline 预览，
//!   其余一律 `application/octet-stream` + attachment，避免 HTML/SVG 型 XSS；
//! - 单文件上限 50 MiB；磁盘 key 由服务端生成，文件名仅展示。

mod storage;

use std::{
    collections::HashSet,
    path::{Component, Path as FsPath, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    auth::CurrentUser,
    error::{ApiResult, AppError},
    state::AppState,
};

const MAX_UPLOAD: usize = 50 * 1024 * 1024;
const INLINE_MIME: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/gif"];

pub use storage::MediaStorage;

#[derive(Debug, FromRow, serde::Serialize)]
struct MediaRow {
    id: Uuid,
    tenant_id: i16,
    user_id: Uuid,
    filename: String,
    mime: String,
    bytes: i64,
    sha256: String,
    created_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, tenant_id, user_id, filename, mime, bytes, sha256, created_at";

pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/v1/media",
            get(list_media)
                .post(upload_media)
                .layer(DefaultBodyLimit::max(MAX_UPLOAD)),
        )
        .route(
            "/api/v1/media/{id}",
            get(media_content).delete(delete_media),
        )
        // P1-4：本地磁盘孤儿清理（默认 dry-run；webdav/s3 无法安全枚举）。
        .route("/api/v1/admin/media/gc", post(media_gc))
        .route("/api/v1/admin/media/migrate", post(media_migrate))
}

fn is_admin(actor: &CurrentUser) -> bool {
    actor.role == "owner" || actor.role == "admin"
}

fn is_inline_mime(mime: &str) -> bool {
    INLINE_MIME
        .iter()
        .any(|allowed| mime.eq_ignore_ascii_case(allowed))
}

fn normalize_claimed_mime(raw: Option<&str>) -> String {
    raw.map(|value| {
        value
            .split(';')
            .next()
            .unwrap_or(value)
            .trim()
            .to_ascii_lowercase()
    })
    .filter(|value| {
        !value.is_empty()
            && value.len() <= 120
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/.+-".contains(c))
    })
    .unwrap_or_else(|| "application/octet-stream".to_string())
}

fn magic_matches(mime: &str, body: &[u8]) -> bool {
    match mime {
        "image/png" => body.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => body.starts_with(b"\xff\xd8\xff"),
        "image/gif" => body.starts_with(b"GIF87a") || body.starts_with(b"GIF89a"),
        "image/webp" => body.len() > 12 && body.starts_with(b"RIFF") && &body[8..12] == b"WEBP",
        _ => false,
    }
}

/// 只有"声明是图片且魔数匹配"才 inline；否则按二进制下载。
fn served_mime(claimed: &str, body: &[u8]) -> String {
    if is_inline_mime(claimed) && magic_matches(claimed, body) {
        claimed.to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

fn display_name(raw: Option<&str>, mime: &str) -> String {
    let fallback = if mime.starts_with("image/") {
        "upload.png"
    } else {
        "upload.bin"
    };
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            value
                .chars()
                .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
                .take(160)
                .collect::<String>()
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn storage_key(actor: &CurrentUser, id: Uuid) -> String {
    let tenant = crate::tenant::current_tenant()
        .map(|value| value.to_string())
        .unwrap_or_else(|| actor.tenant_id.to_string());
    format!("{tenant}/{}/{id}/blob", actor.id)
}

fn public(row: &MediaRow) -> serde_json::Value {
    json!({
        "id": row.id,
        "filename": row.filename,
        "mime": row.mime,
        "kind": if row.mime.starts_with("image/") { "image" } else { "file" },
        "bytes": row.bytes,
        "sha256": row.sha256,
        "created_at": row.created_at,
        "url": format!("/api/v1/media/{}", row.id),
    })
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListQuery {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

pub async fn list_media(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Response> {
    let limit = q.limit.unwrap_or(50).clamp(1, 100);
    let offset = q
        .cursor
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let admin = is_admin(&actor);
    let rows = sqlx::query_as::<_, MediaRow>(&format!(
        "SELECT {COLUMNS} FROM media_assets
          WHERE tenant_id = $1 AND ($2::bool OR user_id = $3)
          ORDER BY created_at DESC, id DESC
          LIMIT $4 OFFSET $5"
    ))
    .bind(actor.tenant_id)
    .bind(admin)
    .bind(actor.id)
    .bind(limit + 1)
    .bind(offset)
    .fetch_all(&state.db())
    .await?;
    let has_more = rows.len() as i64 > limit;
    let page: Vec<_> = rows.into_iter().take(limit as usize).collect();
    Ok(Json(json!({
        "data": {
            "media": page.iter().map(public).collect::<Vec<_>>(),
            "next_cursor": if has_more { Some((offset + limit).to_string()) } else { None },
        }
    }))
    .into_response())
}

#[derive(Debug, Deserialize)]
pub(crate) struct UploadQuery {
    #[serde(default)]
    filename: Option<String>,
}

pub async fn upload_media(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    headers: HeaderMap,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<Response> {
    if body.is_empty() {
        return Err(AppError::bad_request("empty upload"));
    }
    if body.len() > MAX_UPLOAD {
        return Err(AppError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "file exceeds 50 MiB",
        ));
    }
    crate::tenant::quota::ensure_storage_budget_with(&state, actor.plan.as_ref(), body.len() as u64)
        .await?;
    let claimed = normalize_claimed_mime(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
    );
    let mime = served_mime(&claimed, &body);
    let filename = display_name(q.filename.as_deref(), &mime);
    let id = Uuid::new_v4();
    let key = storage_key(&actor, id);
    let storage = storage::from_config(&state.cfg)
        .map_err(|msg| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "media_unavailable", msg))?;
    storage.put(&key, &body).await.map_err(|e| {
        tracing::error!(error = %e, "media put failed");
        AppError::internal("cannot store media")
    })?;

    let digest = Sha256::digest(&body);
    let sha256: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let row = sqlx::query_as::<_, MediaRow>(&format!(
        "INSERT INTO media_assets (id, tenant_id, user_id, filename, mime, bytes, sha256)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING {COLUMNS}"
    ))
    .bind(id)
    .bind(actor.tenant_id)
    .bind(actor.id)
    .bind(&filename)
    .bind(&mime)
    .bind(body.len() as i64)
    .bind(&sha256)
    .fetch_one(&state.db())
    .await?;

    audit(
        &state,
        &actor,
        headers_ip(&headers),
        "media.upload",
        id,
        &format!("{mime} {} bytes", body.len()),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "data": { "media": public(&row) } })),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
pub(crate) struct ContentQuery {
    #[serde(default)]
    download: Option<String>,
}

pub async fn media_content(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    Query(q): Query<ContentQuery>,
) -> ApiResult<Response> {
    let row = load_media(&state, &actor, id).await?;
    let storage = storage::from_config(&state.cfg)
        .map_err(|msg| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "media_unavailable", msg))?;
    let key = storage_key(&actor, row.id);
    let bytes = tokio::time::timeout(Duration::from_secs(10), storage.get(&key))
        .await
        .map_err(|_| AppError::internal("read media timeout"))?
        .map_err(|e| {
            tracing::error!(%id, error = %e, "read media blob failed");
            AppError::not_found("media file missing")
        })?;
    let download = q.download.as_deref() == Some("1") || !row.mime.starts_with("image/");
    let disposition = format!(
        "{}; filename=\"{}\"",
        if download { "attachment" } else { "inline" },
        row.filename.replace(['"', '\\'], "_")
    );
    Ok((
        [
            (header::CONTENT_TYPE, row.mime.clone()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, "private, max-age=3600".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        bytes,
    )
        .into_response())
}

pub async fn delete_media(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let row = load_media(&state, &actor, id).await?;
    sqlx::query("DELETE FROM media_assets WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(actor.tenant_id)
        .execute(&state.db())
        .await?;
    if let Ok(storage) = storage::from_config(&state.cfg) {
        let _ = storage.delete(&storage_key(&actor, id)).await;
    }
    audit(
        &state,
        &actor,
        headers_ip(&headers),
        "media.delete",
        id,
        &row.filename,
    )
    .await;
    Ok(Json(json!({ "data": { "id": id, "deleted": true } })).into_response())
}

#[derive(Debug, Default, Deserialize)]
pub struct MigrateQuery {
    /// 默认 dry-run；`0/false/no/off` 才执行复制。
    #[serde(default)]
    dry_run: Option<String>,
    /// 复制成功后删除本地源（默认 false）。
    #[serde(default)]
    delete_source: Option<String>,
}

fn toggle(value: Option<&str>, default: bool) -> Result<bool, AppError> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(default),
        Some(v) if matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on") => Ok(true),
        Some(v) if matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off") => {
            Ok(false)
        }
        Some(_) => Err(AppError::bad_request("expected boolean flag")),
    }
}

fn migrate_key(tenant: Uuid, user: Uuid, asset: Uuid) -> String {
    format!("{tenant}/{user}/{asset}/blob")
}

/// 把当前租户的 local 媒体对象迁移到当前配置的 WebDAV/S3 后端。
/// 仅 local→非 local；dry-run 时不读文件、不写目标。
pub async fn media_migrate(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Query(q): Query<MigrateQuery>,
) -> ApiResult<Response> {
    crate::admin::ensure_admin(&actor)?;
    let dry_run = toggle(q.dry_run.as_deref(), true)?;
    let delete_source = toggle(q.delete_source.as_deref(), false)?;
    let backend = state.cfg.media_backend.trim().to_ascii_lowercase();
    if backend.is_empty() || backend == "local" {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "media_migrate_unsupported",
            "set CHIRON_MEDIA_BACKEND to webdav or s3 before migrating",
        ));
    }
    let tenant = crate::tenant::current_tenant()
        .ok_or_else(|| AppError::bad_request("media migrate requires a tenant context"))?;
    let target = storage::from_config(&state.cfg)
        .map_err(|msg| AppError::new(StatusCode::SERVICE_UNAVAILABLE, "media_unavailable", msg))?;
    let source = storage::LocalStorage::new(state.cfg.media_root_path());
    let rows: Vec<(Uuid, Uuid, i64, String)> = sqlx::query_as(
        "SELECT id, user_id, bytes, sha256 FROM media_assets \
          WHERE tenant_id = $1 ORDER BY created_at ASC",
    )
    .bind(actor.tenant_id)
    .fetch_all(&state.db())
    .await?;

    let (mut copied, mut missing, mut failed, mut bytes) = (0usize, 0usize, 0usize, 0i64);
    if !dry_run {
        for (id, user, size, expected) in &rows {
            let key = migrate_key(tenant, *user, *id);
            let data = match source.get(&key).await {
                Ok(data) => data,
                Err(_) => {
                    missing += 1;
                    continue;
                }
            };
            let digest: String = Sha256::digest(&data)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            if digest != *expected || data.len() as i64 != *size {
                failed += 1;
                tracing::warn!(%id, "media migrate source checksum mismatch");
                continue;
            }
            match target.put(&key, &data).await {
                Ok(()) => {
                    copied += 1;
                    bytes += *size;
                    if delete_source {
                        // 删除源前回读目标并校验；校验失败宁可留源，不能让对象只剩未知副本。
                        match target.get(&key).await {
                            Ok(back) => {
                                let back_digest: String = Sha256::digest(&back)
                                    .iter()
                                    .map(|b| format!("{b:02x}"))
                                    .collect();
                                if back_digest == *expected && back.len() as i64 == *size {
                                    let _ = source.delete(&key).await;
                                } else {
                                    tracing::warn!(%id, "media migrate target checksum mismatch; source kept");
                                }
                            }
                            Err(e) => {
                                tracing::warn!(%id, error = %e, "media migrate target verify failed; source kept")
                            }
                        }
                    }
                }
                Err(e) => {
                    failed += 1;
                    tracing::warn!(%id, error = %e, "media migrate put failed");
                }
            }
        }
    }
    let total_bytes: i64 = rows.iter().map(|row| row.2).sum();
    crate::integration::audit(
        &state,
        &actor,
        "media.migrate",
        &actor.tenant_id.to_string(),
        &format!(
            "backend={backend} dry_run={dry_run} scanned={} copied={copied} missing={missing} failed={failed}",
            rows.len()
        ),
    )
    .await;
    Ok(Json(json!({ "data": {
        "dry_run": dry_run,
        "backend": backend,
        "scanned": rows.len(),
        "copied": copied,
        "missing": missing,
        "failed": failed,
        "bytes": if dry_run { total_bytes } else { bytes },
    } }))
    .into_response())
}

async fn load_media(state: &AppState, actor: &CurrentUser, id: Uuid) -> Result<MediaRow, AppError> {
    let row = sqlx::query_as::<_, MediaRow>(&format!(
        "SELECT {COLUMNS} FROM media_assets WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(actor.tenant_id)
    .fetch_optional(&state.db())
    .await?
    .ok_or_else(|| AppError::not_found("media not found"))?;
    if row.user_id != actor.id && !is_admin(actor) {
        return Err(AppError::not_found("media not found"));
    }
    Ok(row)
}

// ---------------------------------------------------------------------------
// P1-4：本地媒体孤儿清理
// ---------------------------------------------------------------------------

/// 查询参数：`dry_run` 缺省为 true，只有显式 `0/false/no/off` 才真正删除。
#[derive(Debug, Deserialize)]
pub(crate) struct GcQuery {
    #[serde(default)]
    dry_run: Option<String>,
}

/// 扫描到的一个 `<user-uuid>/<asset-uuid>/blob` 文件。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScannedAsset {
    user: Uuid,
    asset: Uuid,
    bytes: u64,
}

fn is_local_backend(backend: &str) -> bool {
    matches!(backend.trim().to_ascii_lowercase().as_str(), "" | "local")
}

/// dry-run 解析：未传 = true；非法值报 400，避免拼写错误变成真实删除。
fn parse_dry_run(raw: Option<&str>) -> Result<bool, AppError> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(true);
    };
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(AppError::bad_request(format!(
            "invalid dry_run `{other}`; use 1/0 or true/false"
        ))),
    }
}

/// 目录段只允许 uuid / 数字租户 id（ASCII 字母数字与 `-`），杜绝 `..`、分隔符与控制字符。
fn safe_segment(raw: &str) -> Result<&str, AppError> {
    let ok = !raw.is_empty()
        && raw.len() <= 64
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if ok {
        Ok(raw)
    } else {
        Err(AppError::bad_request("invalid tenant directory"))
    }
}

/// `media_root/<tenant>`：任何扫描/删除都从这里出发，不接受调用方拼路径。
fn tenant_root(media_root: &FsPath, tenant: &str) -> Result<PathBuf, AppError> {
    Ok(media_root.join(safe_segment(tenant)?))
}

/// 当前租户的目录段：控制库 uuid 优先，其次沿用 `storage_key` 的 `actor.tenant_id` 段。
fn tenant_segment(tenant_id: i16) -> Result<String, AppError> {
    if let Some(id) = crate::tenant::current_tenant() {
        return Ok(id.to_string());
    }
    if tenant_id > 0 {
        return Ok(tenant_id.to_string());
    }
    Err(AppError::bad_request("no tenant context for media gc"))
}

/// 只认 `<user-uuid>/<asset-uuid>/blob` 三段；其余深度/名字一律忽略（不删）。
fn parse_asset_blob(relative: &FsPath) -> Option<(Uuid, Uuid)> {
    let mut parts = relative.components();
    let (
        Some(Component::Normal(user)),
        Some(Component::Normal(asset)),
        Some(Component::Normal(name)),
    ) = (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if name != "blob" || parts.next().is_some() {
        return None;
    }
    Some((
        Uuid::parse_str(user.to_str()?).ok()?,
        Uuid::parse_str(asset.to_str()?).ok()?,
    ))
}

/// 列出一层的真实子目录：符号链接、普通文件一律跳过，绝不越出当前根。
async fn subdirs(path: &FsPath) -> Vec<tokio::fs::DirEntry> {
    let mut out = Vec::new();
    let mut dir = match tokio::fs::read_dir(path).await {
        Ok(dir) => dir,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return out,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "media gc: read dir failed");
            return out;
        }
    };
    loop {
        match dir.next_entry().await {
            Ok(Some(entry)) => match tokio::fs::symlink_metadata(entry.path()).await {
                Ok(meta) if meta.is_dir() && !meta.is_symlink() => out.push(entry),
                _ => {}
            },
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "media gc: read dir entry failed");
                break;
            }
        }
    }
    out
}

/// 扫描租户根下的 `<user>/<asset>/blob`；逐项错误只记日志，不中断整体扫描。
///
/// 只有能解析成两个 UUID 的三段路径才计入（畸形路径直接忽略，不会被删）。
async fn scan_tenant_root(root: &FsPath) -> Vec<ScannedAsset> {
    let mut found = Vec::new();
    for user_dir in subdirs(root).await {
        let user_name = user_dir.file_name();
        for asset_dir in subdirs(&user_dir.path()).await {
            let blob = asset_dir.path().join("blob");
            let bytes = match tokio::fs::symlink_metadata(&blob).await {
                // 只统计真实文件；目录 / 符号链接 / 缺失的 blob 都不参与。
                Ok(meta) if meta.is_file() && !meta.is_symlink() => meta.len(),
                _ => continue,
            };
            let relative = FsPath::new(&user_name)
                .join(asset_dir.file_name())
                .join("blob");
            if let Some((user, asset)) = parse_asset_blob(&relative) {
                found.push(ScannedAsset { user, asset, bytes });
            }
        }
    }
    found
}

/// 孤儿 = 磁盘上存在但 `media_assets` 里没有的 asset uuid；返回 (孤儿列表, 字节合计)。
fn select_orphans(
    candidates: Vec<ScannedAsset>,
    known: &HashSet<Uuid>,
) -> (Vec<ScannedAsset>, u64) {
    let mut orphans = Vec::with_capacity(candidates.len());
    let mut bytes = 0u64;
    for candidate in candidates {
        if !known.contains(&candidate.asset) {
            bytes += candidate.bytes;
            orphans.push(candidate);
        }
    }
    (orphans, bytes)
}

/// 删除孤儿 asset 目录：路径由已解析的 uuid 重新拼接，绝不使用扫描到的原始路径。
async fn delete_orphans(root: &FsPath, orphans: &[ScannedAsset]) -> u64 {
    let mut deleted = 0u64;
    for orphan in orphans {
        let Ok(user_root) = tenant_root(root, &orphan.user.to_string()) else {
            continue;
        };
        let dir = user_root.join(orphan.asset.to_string());
        match tokio::fs::symlink_metadata(&dir).await {
            Ok(meta) if meta.is_dir() && !meta.is_symlink() => {}
            _ => {
                tracing::warn!(path = %dir.display(), "media gc: skip non-directory orphan");
                continue;
            }
        }
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => deleted += 1,
            Err(e) => {
                tracing::warn!(path = %dir.display(), error = %e, "media gc: delete orphan failed")
            }
        }
    }
    deleted
}

/// `POST /api/v1/admin/media/gc[?dry_run=1]`：清理当前租户本地媒体孤儿目录。
pub async fn media_gc(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    Query(q): Query<GcQuery>,
) -> ApiResult<Response> {
    crate::admin::ensure_admin(&actor)?;
    let dry_run = parse_dry_run(q.dry_run.as_deref())?;
    if !is_local_backend(&state.cfg.media_backend) {
        return Err(AppError::new(
            StatusCode::CONFLICT,
            "media_gc_unsupported",
            "media garbage collection requires CHIRON_MEDIA_BACKEND=local",
        ));
    }
    let tenant = tenant_segment(actor.tenant_id)?;
    let root = tenant_root(&state.cfg.media_root_path(), &tenant)?;

    let known: HashSet<Uuid> =
        sqlx::query_scalar("SELECT id FROM media_assets WHERE tenant_id = $1")
            .bind(actor.tenant_id)
            .fetch_all(&state.db())
            .await?
            .into_iter()
            .collect();

    let candidates = scan_tenant_root(&root).await;
    let scanned = candidates.len();
    let (orphans, bytes) = select_orphans(candidates, &known);
    let orphan_count = orphans.len();
    let deleted = if dry_run {
        0
    } else {
        delete_orphans(&root, &orphans).await
    };

    crate::integration::audit(
        &state,
        &actor,
        "media_gc",
        &tenant,
        &format!(
            "dry_run={dry_run} scanned={scanned} orphans={orphan_count} bytes={bytes} deleted={deleted}"
        ),
    )
    .await;

    Ok(Json(json!({
        "data": {
            "dry_run": dry_run,
            "scanned": scanned,
            "orphans": orphan_count,
            "bytes": bytes,
            "deleted": deleted,
        }
    }))
    .into_response())
}

fn headers_ip(headers: &HeaderMap) -> &str {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("")
}

async fn audit(
    state: &AppState,
    actor: &CurrentUser,
    ip: &str,
    action: &str,
    id: Uuid,
    detail: &str,
) {
    let _ = sqlx::query(
        "INSERT INTO audit_logs (tenant_id, user_id, action, target, detail, ip)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(actor.tenant_id)
    .bind(actor.id)
    .bind(action)
    .bind(id.to_string())
    .bind(detail)
    .bind(ip)
    .execute(&state.db())
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_files_are_downloads_not_inline() {
        assert_eq!(
            served_mime("text/html", b"<script>"),
            "application/octet-stream"
        );
        assert_eq!(
            served_mime("image/svg+xml", b"<svg/>"),
            "application/octet-stream"
        );
        assert_eq!(
            served_mime("image/png", b"<html>"),
            "application/octet-stream"
        );
        assert_eq!(
            served_mime("image/png", b"\x89PNG\r\n\x1a\nrest"),
            "image/png"
        );
    }

    #[test]
    fn claimed_mime_is_normalized() {
        assert_eq!(
            normalize_claimed_mime(Some("Text/Plain; charset=utf-8")),
            "text/plain"
        );
        assert_eq!(
            normalize_claimed_mime(Some("bad mime!!")),
            "application/octet-stream"
        );
        assert_eq!(normalize_claimed_mime(None), "application/octet-stream");
    }

    #[test]
    fn display_name_strips_paths_and_controls() {
        assert_eq!(
            display_name(Some("a/b\\c.pdf"), "application/pdf"),
            "abc.pdf"
        );
        assert_eq!(display_name(None, "application/pdf"), "upload.bin");
    }

    // ---- P1-4：本地媒体孤儿清理 -------------------------------------------

    #[test]
    fn dry_run_defaults_to_true_and_rejects_garbage() {
        assert!(parse_dry_run(None).unwrap());
        assert!(parse_dry_run(Some("  ")).unwrap());
        for yes in ["1", "true", "TRUE", "yes", "on"] {
            assert!(parse_dry_run(Some(yes)).unwrap(), "`{yes}` must be dry");
        }
        for no in ["0", "false", "False", "no", "off"] {
            assert!(!parse_dry_run(Some(no)).unwrap(), "`{no}` must apply");
        }
        // 拼写错误不能静默变成真实删除。
        let err = parse_dry_run(Some("nope")).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn local_backend_gate_is_strict() {
        assert!(is_local_backend(""));
        assert!(is_local_backend("local"));
        assert!(is_local_backend(" LOCAL "));
        assert!(!is_local_backend("webdav"));
        assert!(!is_local_backend("s3"));
    }

    #[test]
    fn tenant_root_rejects_unsafe_segments() {
        let media_root = FsPath::new("/srv/media");
        for bad in ["", "..", ".", "a/b", "../etc", "x\\y", "a b"] {
            let err = tenant_root(media_root, bad).unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST, "segment `{bad}`");
        }
        let uuid = Uuid::from_u128(0x2b6f);
        let root = tenant_root(media_root, &uuid.to_string()).unwrap();
        assert_eq!(
            root.file_name().unwrap(),
            uuid.to_string().as_str()
        );
        // 单库形态的目录段是数字租户 id。
        assert!(tenant_root(media_root, "1").is_ok());
    }

    #[test]
    fn asset_paths_require_two_uuid_segments_and_blob_name() {
        let user = Uuid::from_u128(1);
        let asset = Uuid::from_u128(2);
        let ok = PathBuf::from(format!("{user}/{asset}/blob"));
        assert_eq!(parse_asset_blob(&ok), Some((user, asset)));

        for bad in [
            String::new(),
            "blob".to_string(),
            format!("{user}/blob"),
            format!("{user}/{asset}"),
            format!("{user}/{asset}/blob/extra"),
            format!("{user}/{asset}/other"),
            format!("not-a-uuid/{asset}/blob"),
            format!("{user}/not-a-uuid/blob"),
            "../{asset}/blob".to_string(),
        ] {
            assert!(
                parse_asset_blob(FsPath::new(&bad)).is_none(),
                "`{bad}` must be ignored"
            );
        }
    }

    #[test]
    fn orphan_selection_keeps_known_rows_and_sums_bytes() {
        let user = Uuid::from_u128(1);
        let known_asset = Uuid::from_u128(2);
        let orphan = Uuid::from_u128(3);
        let candidates = vec![
            ScannedAsset { user, asset: known_asset, bytes: 10 },
            ScannedAsset { user, asset: orphan, bytes: 7 },
        ];
        let known: HashSet<Uuid> = [known_asset].into_iter().collect();
        let (orphans, bytes) = select_orphans(candidates, &known);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].asset, orphan);
        assert_eq!(bytes, 7);
        // 全部命中时不删任何东西。
        let (none, zero) = select_orphans(
            vec![ScannedAsset { user, asset: known_asset, bytes: 10 }],
            &known,
        );
        assert!(none.is_empty());
        assert_eq!(zero, 0);
    }

    /// 只在临时目录里造树：验证深度/名字过滤，以及"只删孤儿、不碰其他文件"。
    #[tokio::test]
    async fn gc_scans_and_deletes_only_orphan_dirs_in_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let boxed: PathBuf = dir.path().to_path_buf();
        let root = tenant_root(&boxed, &Uuid::from_u128(9).to_string()).unwrap();

        let user = Uuid::from_u128(1);
        let kept = Uuid::from_u128(2);
        let orphan = Uuid::from_u128(3);
        let kept_blob = root.join(user.to_string()).join(kept.to_string()).join("blob");
        let orphan_dir = root.join(user.to_string()).join(orphan.to_string());
        let orphan_blob = orphan_dir.join("blob");
        tokio::fs::create_dir_all(kept_blob.parent().unwrap()).await.unwrap();
        tokio::fs::create_dir_all(&orphan_dir).await.unwrap();
        tokio::fs::write(&kept_blob, b"keep").await.unwrap();
        tokio::fs::write(&orphan_blob, b"orphan!").await.unwrap();

        // 畸形路径：深度、名字、非 UUID 目录都不参与扫描。
        let shallow = root.join(user.to_string()).join("blob");
        tokio::fs::write(&shallow, b"x").await.unwrap();
        let wrong_name = root.join(user.to_string()).join(kept.to_string()).join("notblob");
        tokio::fs::write(&wrong_name, b"x").await.unwrap();
        let stray_dir = root.join(user.to_string()).join("not-a-uuid");
        tokio::fs::create_dir_all(&stray_dir).await.unwrap();
        tokio::fs::write(stray_dir.join("blob"), b"x").await.unwrap();
        let nested = root
            .join(user.to_string())
            .join(orphan.to_string())
            .join("extra")
            .join("blob");
        tokio::fs::create_dir_all(nested.parent().unwrap()).await.unwrap();
        tokio::fs::write(&nested, b"x").await.unwrap();
        // 没有 blob 的 asset 目录不统计。
        let empty_asset = root.join(user.to_string()).join(Uuid::from_u128(4).to_string());
        tokio::fs::create_dir_all(&empty_asset).await.unwrap();
        // 租户根之外的兄弟目录：绝不能被删除。
        let outside = dir.path().join("outside");
        tokio::fs::create_dir_all(&outside).await.unwrap();
        tokio::fs::write(outside.join("blob"), b"o").await.unwrap();

        let candidates = scan_tenant_root(&root).await;
        assert_eq!(candidates.len(), 2, "candidates: {candidates:?}");

        let known: HashSet<Uuid> = [kept].into_iter().collect();
        let (orphans, bytes) = select_orphans(candidates, &known);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].asset, orphan);
        assert_eq!(bytes, b"orphan!".len() as u64);

        assert_eq!(delete_orphans(&root, &orphans).await, 1);
        assert!(!orphan_dir.exists(), "orphan dir must be removed");
        assert!(kept_blob.exists(), "known blob must survive");
        assert!(outside.exists(), "sibling dir must survive");
        assert!(stray_dir.exists(), "malformed dirs must survive");

        // 幂等：处理完再扫一遍就没有孤儿了。
        let again = scan_tenant_root(&root).await;
        assert_eq!(select_orphans(again, &known).0.len(), 0);
    }

    #[tokio::test]
    async fn gc_scan_of_missing_root_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        assert!(scan_tenant_root(&missing).await.is_empty());
    }

    #[test]
    fn migrate_flags_default_to_dry_run_and_key_is_server_built() {
        assert!(toggle(None, true).unwrap());
        assert!(!toggle(Some("false"), true).unwrap());
        assert!(toggle(Some("1"), false).unwrap());
        assert!(toggle(Some("maybe"), true).is_err());
        let tenant = Uuid::from_u128(1);
        let user = Uuid::from_u128(2);
        let asset = Uuid::from_u128(3);
        assert_eq!(migrate_key(tenant, user, asset), format!("{tenant}/{user}/{asset}/blob"));
    }
}
