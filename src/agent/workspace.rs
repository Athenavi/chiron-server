//! 工具执行后端抽象。
//!
//! 生产实现把 `read/write/list` 落到用户工作区（宿主侧已挂载的卷），
//! `exec/git` 走 `SandboxManager`（`docker exec`）。测试实现用临时目录 +
//! 可编排的命令结果，不需要 Docker，也不需要真 API key。
//!
//! 契约只有一条：所有相对路径都由调用方（工具层）先做词法钳制，
//! 后端不得接受绝对路径或 `..`。

use async_trait::async_trait;
use serde::Serialize;

use super::error::AgentError;
use crate::sandbox::{ExecOptions, SandboxKey, SandboxManager};

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExecOutcome {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub timed_out: bool,
}

impl ExecOutcome {
    pub fn success(&self) -> bool {
        !self.timed_out && self.exit_code == 0
    }

    pub fn compact(&self) -> serde_json::Value {
        serde_json::json!({
            "exit_code": self.exit_code,
            "stdout": self.stdout,
            "stderr": self.stderr,
            "truncated": self.truncated,
            "timed_out": self.timed_out,
        })
    }
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn read(&self, rel: &str) -> Result<Vec<u8>, AgentError>;
    async fn write(&self, rel: &str, data: &[u8]) -> Result<(), AgentError>;
    async fn list(&self, rel: &str) -> Result<Vec<Entry>, AgentError>;
    async fn exec(
        &self,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ExecOutcome, AgentError>;
    async fn git(&self, repo: &str, sub: &str, args: &[String]) -> Result<ExecOutcome, AgentError>;
}

/// `SandboxManager` 的生产实现：文件走宿主侧卷，命令走容器。
pub struct ManagerSandbox {
    manager: std::sync::Arc<SandboxManager>,
    tenant_id: i16,
    user_id: String,
    project_id: Option<String>,
}

impl ManagerSandbox {
    pub fn new(
        manager: std::sync::Arc<SandboxManager>,
        tenant_id: i16,
        user_id: impl Into<String>,
        project_id: Option<String>,
    ) -> Self {
        Self {
            manager,
            tenant_id,
            user_id: user_id.into(),
            project_id,
        }
    }

    fn key(&self) -> SandboxKey<'_> {
        SandboxKey {
            tenant_id: self.tenant_id,
            user_id: &self.user_id,
            project_id: self.project_id.as_deref(),
        }
    }

    async fn container(&self) -> Result<String, AgentError> {
        let name = self
            .manager
            .ensure_container(self.key())
            .await
            .map_err(AgentError::from_app)?;
        self.manager.touch(&name);
        Ok(name)
    }

    /// B7 检查点：`add -A` → `write-tree` → `commit-tree`，返回 commit sha。
    ///
    /// 只读上仍是 `docker exec git`，因此工作区不会被 checkout；失败的仓库
    /// （未 init/无 git）返回 `None` 而不是让 run 失败。
    pub async fn commit_snapshot(
        &self,
        repo: &str,
        message: &str,
    ) -> Result<Option<String>, AgentError> {
        crate::sandbox::tenant_rel_path(repo).map_err(AgentError::from_app)?;
        let container = self.container().await?;
        let out = self
            .manager
            .git_commit_tree(&container, repo, message)
            .await
            .map_err(AgentError::from_app)?;
        self.manager.touch(&container);
        if !out.success() {
            tracing::info!(stderr = %out.stderr, "workspace snapshot skipped (git)");
            return Ok(None);
        }
        Ok(crate::checkpoint::workspace::parse_commit_sha(&out.stdout))
    }

    /// 把快照 sha 接到 `refs/chiron/checkpoints/...` 上（CAS 由 git 负责）。
    pub async fn pin_snapshot(
        &self,
        repo: &str,
        ref_name: &str,
        sha: &str,
    ) -> Result<bool, AgentError> {
        let container = self.container().await?;
        let out = self
            .manager
            .git(
                &container,
                repo,
                "update-ref",
                &[ref_name.to_string(), sha.to_string()],
            )
            .await
            .map_err(AgentError::from_app)?;
        self.manager.touch(&container);
        Ok(out.success())
    }

    /// 恢复工作区到某个 commit（`reset --hard`，不清理未跟踪文件）。
    pub async fn restore_snapshot(&self, repo: &str, sha: &str) -> Result<ExecOutcome, AgentError> {
        crate::sandbox::tenant_rel_path(repo).map_err(AgentError::from_app)?;
        let container = self.container().await?;
        let out = self
            .manager
            .git(
                &container,
                repo,
                "reset",
                &["--hard".to_string(), sha.to_string()],
            )
            .await
            .map_err(AgentError::from_app);
        self.manager.touch(&container);
        out.map(|o| ExecOutcome {
            exit_code: o.exit_code,
            stdout: o.stdout,
            stderr: o.stderr,
            truncated: o.truncated,
            timed_out: o.timed_out,
        })
    }
}

#[async_trait]
impl Sandbox for ManagerSandbox {
    async fn read(&self, rel: &str) -> Result<Vec<u8>, AgentError> {
        self.manager
            .read_file(self.key(), rel)
            .await
            .map_err(AgentError::from_app)
    }

    async fn write(&self, rel: &str, data: &[u8]) -> Result<(), AgentError> {
        self.manager
            .write_file(self.key(), rel, data)
            .await
            .map_err(AgentError::from_app)
    }

    async fn list(&self, rel: &str) -> Result<Vec<Entry>, AgentError> {
        let entries = self
            .manager
            .list_dir(self.key(), rel)
            .await
            .map_err(AgentError::from_app)?;
        Ok(entries
            .into_iter()
            .map(|e| Entry {
                name: e.name,
                is_dir: e.is_dir,
                size: e.size,
            })
            .collect())
    }

    async fn exec(
        &self,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ExecOutcome, AgentError> {
        let container = self.container().await?;
        let opts = ExecOptions {
            timeout: timeout_secs
                .map(|secs| std::time::Duration::from_secs(secs.clamp(1, 600)))
                .unwrap_or_else(|| self.manager.cfg().exec_timeout()),
            max_output_bytes: self.manager.cfg().exec_max_output_bytes,
            workdir: None,
        };
        let out = self
            .manager
            .exec(&container, command, &opts)
            .await
            .map_err(AgentError::from_app);
        self.manager.touch(&container);
        out.map(|o| ExecOutcome {
            exit_code: o.exit_code,
            stdout: o.stdout,
            stderr: o.stderr,
            truncated: o.truncated,
            timed_out: o.timed_out,
        })
    }

    async fn git(&self, repo: &str, sub: &str, args: &[String]) -> Result<ExecOutcome, AgentError> {
        let container = self.container().await?;
        let out = self
            .manager
            .git(&container, repo, sub, args)
            .await
            .map_err(AgentError::from_app);
        self.manager.touch(&container);
        out.map(|o| ExecOutcome {
            exit_code: o.exit_code,
            stdout: o.stdout,
            stderr: o.stderr,
            truncated: o.truncated,
            timed_out: o.timed_out,
        })
    }
}

/// 测试后端：临时目录 + 可编排命令输出。
#[cfg(test)]
pub struct FakeSandbox {
    root: std::path::PathBuf,
    exec_result: ExecOutcome,
    pub exec_calls: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl FakeSandbox {
    pub fn new(root: std::path::PathBuf) -> Self {
        Self {
            root,
            exec_result: ExecOutcome {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
                truncated: false,
                timed_out: false,
            },
            exec_calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn with_exec_output(mut self, stdout: impl Into<String>, exit_code: i32) -> Self {
        self.exec_result.stdout = stdout.into();
        self.exec_result.exit_code = exit_code;
        self
    }

    fn resolve(&self, rel: &str) -> Result<std::path::PathBuf, AgentError> {
        let rel_path = crate::sandbox::tenant_rel_path(rel)
            .map_err(|e| AgentError::Tool(format!("invalid path: {}", e.message)))?;
        Ok(self.root.join(rel_path))
    }

    pub fn calls(&self) -> Vec<String> {
        self.exec_calls
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[async_trait]
impl Sandbox for FakeSandbox {
    async fn read(&self, rel: &str) -> Result<Vec<u8>, AgentError> {
        let path = self.resolve(rel)?;
        tokio::fs::read(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AgentError::Tool("file not found".into())
            } else {
                AgentError::Sandbox(e.to_string())
            }
        })
    }

    async fn write(&self, rel: &str, data: &[u8]) -> Result<(), AgentError> {
        let path = self.resolve(rel)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AgentError::Sandbox(e.to_string()))?;
        }
        tokio::fs::write(&path, data)
            .await
            .map_err(|e| AgentError::Sandbox(e.to_string()))
    }

    async fn list(&self, rel: &str) -> Result<Vec<Entry>, AgentError> {
        let path = self.resolve(rel)?;
        let mut rd = tokio::fs::read_dir(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AgentError::Tool("directory not found".into())
            } else {
                AgentError::Sandbox(e.to_string())
            }
        })?;
        let mut out = Vec::new();
        while let Ok(Some(entry)) = rd.next_entry().await {
            let metadata = entry.metadata().await.ok();
            out.push(Entry {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir: metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false),
                size: metadata.as_ref().map(|m| m.len()).unwrap_or(0),
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn exec(
        &self,
        command: &str,
        _timeout_secs: Option<u64>,
    ) -> Result<ExecOutcome, AgentError> {
        if let Ok(mut calls) = self.exec_calls.lock() {
            calls.push(command.to_string());
        }
        Ok(self.exec_result.clone())
    }

    async fn git(&self, repo: &str, sub: &str, args: &[String]) -> Result<ExecOutcome, AgentError> {
        let command =
            crate::sandbox::build_git_command(repo, sub, args).map_err(AgentError::from_app)?;
        self.exec(&command, None).await
    }
}
