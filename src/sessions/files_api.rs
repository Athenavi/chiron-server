//! P1-3：会话工作区文件浏览 / 下载。
//!
//! - `GET /api/v1/sessions/{id}/files?path=<rel>&limit=<n>`：列目录（默认根）；
//! - `GET /api/v1/sessions/{id}/files/download?path=<rel>`：流式下载单个常规文件。
//!
//! 归属校验复用 [`load_session`]（租户 + 所属 + 管理员），路径安全三层：
//!
//! 1. 词法闸 [`sanitize_rel`]：拒绝绝对路径、盘符、`..`、NUL；
//! 2. [`resolve_in_workspace`]：逐段解析符号链接，悬空/越界链接直接拒；
//! 3. canonicalize 后再 `starts_with` 一次根目录（[`is_within`]，防平台差异）。
//!
//! 工作区根与会话沙箱同源（[`workspace_dir`]）：租户段取请求作用域的
//! `current_tenant()`，用户/项目段取会话行，因此宿主卷 `SANDBOX_DATA_ROOT`
//! 下的 `<tenant>/<user>` 与容器内 `/workspace` 是同一份目录。宿主进程读不到
//! 该目录时一律 404（读接口不主动建目录，避免 GET 产生副作用）。
//!
//! 只读常规文件：目录与符号链接（即使指向根内）都不允许下载，避免“链到链”绕过。

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Extension, Path as UrlPath, Query, State},
    http::{header, HeaderValue},
    response::Response,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use super::{load_session, ok, SessionRow};
use crate::{
    auth::CurrentUser,
    error::{ApiResult, AppError},
    sandbox::{resolve_in_workspace, tenant_rel_path, workspace_dir, SandboxKey},
    state::AppState,
};

/// 单文件下载上限：50 MiB；更大的内容请在沙箱内命令处理。
pub const MAX_DOWNLOAD_BYTES: u64 = 50 * 1024 * 1024;
/// 默认分页条数与硬上限。
pub const DEFAULT_LIMIT: usize = 200;
pub const MAX_LIMIT: usize = 500;
/// 单次列目录的收集上限：先收集再排序，避免超大目录把内存打满。
const HARD_MAX_ENTRIES: usize = 5000;
/// 流式下载的块大小。
const STREAM_CHUNK: usize = 64 * 1024;
/// NUL 与反斜杠（用码点表示，避免转义歧义）。
const NUL: char = char::from_u32(0).unwrap();
const BACKSLASH: char = char::from_u32(0x5C).unwrap();

#[derive(Debug, Default, Deserialize)]
pub struct ListFilesQuery {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
pub struct DownloadQuery {
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    pub name: String,
    /// `file` / `dir` / `symlink`。
    pub kind: &'static str,
    /// 常规文件取字节数；目录与符号链接为 0。
    pub size: u64,
    pub modified_at: Option<DateTime<Utc>>,
    /// 相对工作区根的路径（`/` 分隔，便于前端直接回传）。
    pub path: String,
}

// ---------------------------------------------------------------- 路径安全

/// 词法闸：把查询串归一成“工作区内的相对路径”（根 = 空串）。
///
/// 绝对路径、盘符/UNC、`..`、NUL 一律 400；`.` 与重复分隔符被吃掉。
/// 纯函数，不碰文件系统（符号链接由 [`resolve_in_workspace`] 负责）。
pub(crate) fn sanitize_rel(raw: Option<&str>) -> Result<String, AppError> {
    let raw = raw.map(str::trim).unwrap_or("");
    if raw.contains(NUL) {
        return Err(AppError::bad_request("path must not contain NUL bytes"));
    }
    // 复用沙箱的词法钳制；对外统一 400（沙箱内部用的是 403）。
    let rel = tenant_rel_path(raw)
        .map_err(|_| AppError::bad_request("path must stay inside the workspace"))?;
    Ok(to_slash(&rel))
}

/// `Path` → `/` 分隔的相对串；根（`.`）映射为空串。
fn to_slash(rel: &Path) -> String {
    rel.components()
        .filter_map(|comp| match comp {
            Component::Normal(seg) => Some(seg.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// 组件级前缀比较：`/ws-evil` 不是 `/ws` 的子路径（纯函数，便于测试）。
pub(crate) fn is_within(base: &Path, target: &Path) -> bool {
    target.starts_with(base)
}

fn entry_kind(file_type: &std::fs::FileType) -> &'static str {
    if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_dir() {
        "dir"
    } else {
        "file"
    }
}

pub(crate) fn clamp_limit(limit: Option<i64>) -> usize {
    match limit {
        Some(v) => v.clamp(1, MAX_LIMIT as i64) as usize,
        None => DEFAULT_LIMIT,
    }
}

/// `attachment` 的文件名：只取最后一段，去掉控制字符、引号与反斜杠。
fn attachment_filename(rel: &str) -> String {
    let base = rel.rsplit('/').next().unwrap_or(rel);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != BACKSLASH)
        .take(160)
        .collect();
    if cleaned.is_empty() {
        "download".to_string()
    } else {
        cleaned
    }
}

// ------------------------------------------------------------ 工作区定位

/// 会话工作区根：与会话沙箱用同一函数推导（含 tenant / user / project 段）。
///
/// 只读接口不创建目录：从未进入沙箱的会话返回 404，而不是凭空建一棵空树。
async fn session_workspace(state: &AppState, session: &SessionRow) -> Result<PathBuf, AppError> {
    let user_id = session.user_id.to_string();
    let project_id = session.project_id.clone();
    let key = SandboxKey {
        tenant_id: session.tenant_id,
        user_id: &user_id,
        project_id: project_id.as_deref(),
    };
    let root = workspace_dir(state.sandbox.cfg(), key);
    match tokio::fs::metadata(&root).await {
        Ok(md) if md.is_dir() => Ok(root),
        Ok(_) => Err(AppError::not_found("workspace not found")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(AppError::not_found("workspace not found"))
        }
        Err(e) => {
            tracing::error!(error = %e, "stat agent workspace failed");
            Err(AppError::internal("agent workspace unavailable"))
        }
    }
}

/// 符号链接逐段校验 + canonicalize 二次确认（词法闸由调用方先做）。
async fn resolve_target(workspace: &Path, rel: &str) -> Result<PathBuf, AppError> {
    let target = resolve_in_workspace(workspace, rel).await?;
    let base = tokio::fs::canonicalize(workspace).await.map_err(|e| {
        tracing::error!(error = %e, "canonicalize agent workspace failed");
        AppError::internal("agent workspace unavailable")
    })?;
    // 目标存在时再 canonicalize 一次：任何平台差异下都必须落在根内。
    if let Ok(real) = tokio::fs::canonicalize(&target).await {
        if !is_within(&base, &real) {
            return Err(AppError::forbidden("path escapes the workspace"));
        }
    }
    Ok(target)
}

fn map_dir_error(e: std::io::Error) -> AppError {
    if e.kind() == std::io::ErrorKind::NotFound {
        AppError::not_found("directory not found")
    } else {
        tracing::error!(error = %e, "read agent workspace dir failed");
        AppError::internal("cannot list agent workspace")
    }
}

fn map_file_error(e: std::io::Error) -> AppError {
    if e.kind() == std::io::ErrorKind::NotFound {
        AppError::not_found("file not found")
    } else {
        tracing::error!(error = %e, "read agent workspace file failed");
        AppError::internal("cannot read agent workspace")
    }
}

/// 按块转发，不把整个文件读进内存。
fn stream_file(
    file: tokio::fs::File,
) -> impl futures_util::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send + 'static {
    async_stream::stream! {
        let mut reader = tokio::io::BufReader::with_capacity(STREAM_CHUNK, file);
        let mut buf = vec![0u8; STREAM_CHUNK];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => yield Ok(bytes::Bytes::copy_from_slice(&buf[..n])),
                Err(e) => {
                    yield Err(e);
                    break;
                }
            }
        }
    }
}

// ------------------------------------------------------------------ 路由

/// 列目录：`{ path, entries, truncated }`；缺失目录 404，越界/非法 400。
pub async fn list_files(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    UrlPath(session_id): UrlPath<Uuid>,
    Query(q): Query<ListFilesQuery>,
) -> ApiResult<Response> {
    let session = load_session(&state, &actor, session_id).await?;
    let rel = sanitize_rel(q.path.as_deref())?;
    let limit = clamp_limit(q.limit);
    let workspace = session_workspace(&state, &session).await?;
    let target = resolve_target(&workspace, &rel).await?;

    // 最后一段若是根内符号链接也允许列（已过 resolve_target）。
    let md = tokio::fs::metadata(&target).await.map_err(map_dir_error)?;
    if !md.is_dir() {
        return Err(AppError::bad_request("not a directory"));
    }

    let mut rd = tokio::fs::read_dir(&target).await.map_err(map_dir_error)?;
    let mut entries: Vec<FileEntry> = Vec::new();
    let mut truncated = false;
    while let Some(entry) = rd.next_entry().await.map_err(map_dir_error)? {
        if entries.len() >= HARD_MAX_ENTRIES {
            truncated = true;
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = match entry.file_type().await {
            Ok(file_type) => file_type,
            // 单项 stat 失败（并发删除等）跳过，不让整个目录 500。
            Err(_) => continue,
        };
        let kind = entry_kind(&file_type);
        // tokio 的 DirEntry::metadata 不跟随符号链接：size/mtime 取链接自身。
        let md = entry.metadata().await.ok();
        let size = if kind == "file" {
            md.as_ref().map(|m| m.len()).unwrap_or(0)
        } else {
            0
        };
        let modified_at = md
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(DateTime::<Utc>::from);
        entries.push(FileEntry {
            name: name.clone(),
            kind,
            size,
            modified_at,
            path: join_rel(&rel, &name),
        });
    }

    // 目录优先，再按名字升序：与沙箱 list_dir 的展示顺序一致。
    entries.sort_by(|a, b| {
        let a_dir = a.kind == "dir";
        let b_dir = b.kind == "dir";
        (!a_dir, &a.name).cmp(&(!b_dir, &b.name))
    });
    if entries.len() > limit {
        truncated = true;
        entries.truncate(limit);
    }

    tracing::debug!(session = %session_id, path = %rel, count = entries.len(), "list workspace files");
    Ok(ok(json!({
        "path": rel,
        "entries": entries,
        "truncated": truncated,
    })))
}

/// 下载单个常规文件：`Content-Disposition: attachment` + 分块流式响应。
pub async fn download_file(
    State(state): State<Arc<AppState>>,
    Extension(actor): Extension<CurrentUser>,
    UrlPath(session_id): UrlPath<Uuid>,
    Query(q): Query<DownloadQuery>,
) -> ApiResult<Response> {
    let session = load_session(&state, &actor, session_id).await?;
    let rel = sanitize_rel(q.path.as_deref())?;
    if rel.is_empty() {
        return Err(AppError::bad_request("path is required"));
    }
    let workspace = session_workspace(&state, &session).await?;
    let target = resolve_target(&workspace, &rel).await?;

    // 不跟随最后一段：symlink_metadata 下链接既不是目录也不是常规文件。
    let md = tokio::fs::symlink_metadata(&target)
        .await
        .map_err(map_file_error)?;
    let file_type = md.file_type();
    if file_type.is_symlink() {
        return Err(AppError::forbidden("symlinks cannot be downloaded"));
    }
    if file_type.is_dir() {
        return Err(AppError::bad_request("path is a directory"));
    }
    if !file_type.is_file() {
        return Err(AppError::bad_request("not a regular file"));
    }
    if md.len() > MAX_DOWNLOAD_BYTES {
        return Err(AppError::bad_request("file too large (max 50 MiB)"));
    }

    let file = tokio::fs::File::open(&target).await.map_err(map_file_error)?;
    let mut resp = Response::new(Body::from_stream(stream_file(file)));
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    if let Ok(value) = HeaderValue::from_str(&md.len().to_string()) {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    let disposition = format!("attachment; filename=\"{}\"", attachment_filename(&rel));
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }

    tracing::debug!(session = %session_id, path = %rel, bytes = md.len(), "download workspace file");
    Ok(resp)
}

// ------------------------------------------------------------------ 测试

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_lexically_clamped() {
        assert_eq!(sanitize_rel(None).unwrap(), "");
        assert_eq!(sanitize_rel(Some("")).unwrap(), "");
        assert_eq!(sanitize_rel(Some("  .  ")).unwrap(), "");
        assert_eq!(sanitize_rel(Some("./src/./main.rs")).unwrap(), "src/main.rs");
        assert_eq!(sanitize_rel(Some("a//b/")).unwrap(), "a/b");
        assert_eq!(sanitize_rel(Some("  a/b.rs  ")).unwrap(), "a/b.rs");
    }

    #[test]
    fn absolute_paths_and_drive_prefixes_are_rejected() {
        let sep = BACKSLASH;
        let cases = vec![
            "/etc/passwd".to_string(),
            format!("{sep}windows{sep}win.ini"),
            "C:".to_string() + "x",
            "c:/x".to_string(),
            "//host/share".to_string(),
        ];
        for bad in cases {
            let err = sanitize_rel(Some(&bad)).unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(err.code, "bad_request", "{bad}");
        }
    }

    #[test]
    fn traversal_is_rejected() {
        for bad in ["..", "../x", "a/../../b", "a/..", "./../etc"] {
            let err = sanitize_rel(Some(bad)).unwrap_err();
            assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST, "{bad}");
            assert_eq!(err.code, "bad_request", "{bad}");
        }
    }

    #[test]
    fn nul_bytes_are_rejected() {
        let bad = format!("a{NUL}b");
        let err = sanitize_rel(Some(&bad)).unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains("NUL"), "{}", err.message);
    }

    #[test]
    fn prefix_similar_siblings_are_not_within_root() {
        let base = Path::new("/var/agent-data/t/u");
        assert!(is_within(base, Path::new("/var/agent-data/t/u/a.txt")));
        assert!(is_within(base, base));
        // 前缀字符串相同但组件不同：必须判为越界。
        assert!(!is_within(base, Path::new("/var/agent-data/t/u-evil/a.txt")));
        assert!(!is_within(base, Path::new("/var/agent-data/t")));
        assert!(!is_within(base, Path::new("/etc/passwd")));
    }

    #[test]
    fn limits_are_clamped() {
        assert_eq!(clamp_limit(None), DEFAULT_LIMIT);
        assert_eq!(clamp_limit(Some(0)), 1);
        assert_eq!(clamp_limit(Some(-5)), 1);
        assert_eq!(clamp_limit(Some(1)), 1);
        assert_eq!(clamp_limit(Some(500)), 500);
        assert_eq!(clamp_limit(Some(10_000)), MAX_LIMIT);
    }

    #[test]
    fn attachment_filename_strips_quotes_and_controls() {
        assert_eq!(attachment_filename("a/b/c.txt"), "c.txt");
        let quoted = format!("evil{}name{}x.rs", char::from(0x22), BACKSLASH);
        assert_eq!(attachment_filename(&quoted), "evilnamex.rs");
        assert_eq!(attachment_filename(&NUL.to_string()), "download");
    }

    fn try_symlink_dir(src: &Path, dst: &Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(src, dst).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(src, dst).is_ok()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (src, dst);
            false
        }
    }

    #[tokio::test]
    async fn on_disk_traversal_and_absolute_paths_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        tokio::fs::write(root.path().join("ok.txt"), b"hello")
            .await
            .unwrap();
        assert!(resolve_target(root.path(), "ok.txt").await.is_ok());

        let abs = root.path().join("ok.txt");
        for bad in ["../ok.txt", "/etc/passwd", "sub/../../ok.txt"] {
            let err = resolve_target(root.path(), bad).await.unwrap_err();
            assert!(
                err.code == "bad_request" || err.code == "forbidden",
                "{bad} -> {}",
                err.code
            );
        }
        // 绝对路径必须被词法闸挡住（不会真的去 canonicalize 它）。
        let abs_str = abs.to_string_lossy().to_string();
        assert!(resolve_target(root.path(), &abs_str).await.is_err());
    }

    /// 根内符号链接指向根外：必须被 `resolve_in_workspace` 拒绝。
    #[tokio::test]
    async fn symlink_escape_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        tokio::fs::write(outside.path().join("secret.txt"), b"top secret")
            .await
            .unwrap();
        let link = root.path().join("escape");
        if !try_symlink_dir(outside.path(), &link) {
            eprintln!("skip: cannot create symlinks in this environment");
            return;
        }
        let err = resolve_target(root.path(), "escape/secret.txt")
            .await
            .unwrap_err();
        assert_eq!(
            err.status,
            axum::http::StatusCode::FORBIDDEN,
            "{}",
            err.message
        );
        assert_eq!(err.code, "forbidden");
    }

    /// 根内符号链接指向根内：允许访问（只拦越界）。
    #[tokio::test]
    async fn symlink_inside_root_is_allowed() {
        let root = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(root.path().join("real")).await.unwrap();
        tokio::fs::write(root.path().join("real/a.txt"), b"x")
            .await
            .unwrap();
        let link = root.path().join("alias");
        let real = root.path().join("real");
        if !try_symlink_dir(&real, &link) {
            eprintln!("skip: cannot create symlinks in this environment");
            return;
        }
        let resolved = resolve_target(root.path(), "alias/a.txt").await.unwrap();
        assert!(resolved.ends_with("a.txt"));
    }
}
