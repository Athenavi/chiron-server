//! 工具注册表与执行。
//!
//! v1 工具集（PRD §7.3）：`read`、`write`、`edit`、`ls`、`glob`、`grep`、
//! `shell`、`run_code`、`git`、`web_fetch`、`todo`、`ask_user`、`subagent`。
//!
//! 策略（allow/ask/deny）与执行分离：循环先问 `Toolset::policy`，`ask` 生成审批，
//! 批准后才进入这里。文件/shell/git 一律经 [`Sandbox`]，`web_fetch` 在 Rust 侧执行，
//! `subagent` 交给 `ToolState` 注入的调用器（见 `crate::subagent`）。

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    error::AgentError,
    web::WebClient,
    workspace::{ExecOutcome, Sandbox},
};
use crate::{
    provider::ToolDef,
    sandbox::{shell_quote, tenant_rel_path},
    subagent::Invoker,
};

pub const READ: &str = "read";
pub const WRITE: &str = "write";
pub const EDIT: &str = "edit";
pub const LS: &str = "ls";
pub const GLOB: &str = "glob";
pub const GREP: &str = "grep";
pub const SHELL: &str = "shell";
pub const RUN_CODE: &str = "run_code";
pub const GIT: &str = "git";
pub const WEB_FETCH: &str = "web_fetch";
pub const TODO: &str = "todo";
pub const ASK_USER: &str = "ask_user";
/// B6：子 Agent 派生。执行入口由 `ToolState` 注入（见 `subagent::Invoker`），
/// 因此工具层本身不认识 provider/数据库，测试用 fake 调用器即可。
pub const SUBAGENT: &str = "subagent";

pub const ALL_TOOLS: &[&str] = &[
    READ, WRITE, EDIT, LS, GLOB, GREP, SHELL, RUN_CODE, GIT, WEB_FETCH, TODO, ASK_USER, SUBAGENT,
];

const READ_MAX_LINES: usize = 2000;
const WALK_MAX_ENTRIES: usize = 2000;
const WALK_MAX_DEPTH: usize = 10;
const GREP_MAX_FILE_BYTES: usize = 1024 * 1024;
const GREP_MAX_RESULTS: usize = 100;
const TODO_MAX_ITEMS: usize = 50;

/// 单个工具的策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Allow,
    Ask,
    Deny,
}

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "allow" | "auto" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// 工具策略表。未知工具一律 `Deny`：模型编造工具名不会变成执行入口。
#[derive(Clone, Debug)]
pub struct Toolset {
    policies: BTreeMap<&'static str, Policy>,
}

impl Default for Toolset {
    fn default() -> Self {
        Self::default_set()
    }
}

impl Toolset {
    /// 默认策略：只读工具直接放行，会写盘/执行命令/出网的全部走审批。
    /// 部署可用 `CHIRON_TOOL_POLICY=allow|deny` 调整默认值，
    /// 再用 `CHIRON_TOOL_ALLOW` / `CHIRON_TOOL_DENY` 逐项覆盖。
    pub fn default_set() -> Self {
        let mut policies = BTreeMap::new();
        for name in [READ, LS, GLOB, GREP, TODO, ASK_USER, SUBAGENT] {
            policies.insert(name, Policy::Allow);
        }
        for name in [WRITE, EDIT, SHELL, RUN_CODE, GIT, WEB_FETCH] {
            policies.insert(name, Policy::Ask);
        }
        Self { policies }
    }

    pub fn from_env() -> Self {
        let default_policy = std::env::var("CHIRON_TOOL_POLICY")
            .ok()
            .and_then(|raw| Policy::parse(&raw))
            .unwrap_or(Policy::Ask);
        let mut set = Self::default_set();
        for (name, policy) in set.policies.iter_mut() {
            // 只读工具不受默认策略收紧影响，仍由显式 deny 覆盖。
            if matches!(*name, READ | LS | GLOB | GREP | TODO | ASK_USER | SUBAGENT) {
                continue;
            }
            *policy = default_policy;
        }
        for name in env_list("CHIRON_TOOL_ALLOW") {
            set.set_by_name(&name, Policy::Allow);
        }
        for name in env_list("CHIRON_TOOL_DENY") {
            set.set_by_name(&name, Policy::Deny);
        }
        set
    }

    pub fn allow_all() -> Self {
        let mut policies = BTreeMap::new();
        for name in ALL_TOOLS {
            policies.insert(*name, Policy::Allow);
        }
        Self { policies }
    }

    pub fn deny_all() -> Self {
        let mut policies = BTreeMap::new();
        for name in ALL_TOOLS {
            policies.insert(*name, Policy::Deny);
        }
        Self { policies }
    }

    pub fn with(mut self, name: &'static str, policy: Policy) -> Self {
        self.policies.insert(name, policy);
        self
    }

    fn set_by_name(&mut self, name: &str, policy: Policy) {
        if let Some(key) = self.policies.keys().find(|k| **k == name).copied() {
            self.policies.insert(key, policy);
        }
    }

    pub fn policy(&self, name: &str) -> Policy {
        self.policies.get(name).copied().unwrap_or(Policy::Deny)
    }

    /// 暴露给 provider 的工具声明（按名字排序，测试可对比）。
    pub fn defs(&self) -> Vec<ToolDef> {
        let mut out: Vec<ToolDef> = self
            .policies
            .iter()
            .filter(|(_, policy)| **policy != Policy::Deny)
            .map(|(name, _)| definition(name))
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

fn env_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .map(|raw| {
            raw.split(',')
                .map(|item| item.trim().to_ascii_lowercase())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// 工具名 → 是否具有写/执行/出网副作用。
pub fn mutating(name: &str) -> bool {
    matches!(name, WRITE | EDIT | SHELL | RUN_CODE | GIT | WEB_FETCH)
}

fn definition(name: &str) -> ToolDef {
    let (description, parameters) = match name {
        READ => (
            "Read a text file inside the workspace. Paths are relative to /workspace.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "relative file path"},
                    "offset": {"type": "integer", "description": "1-based first line"},
                    "limit": {"type": "integer", "description": "max lines (default 2000)"}
                },
                "required": ["path"]
            }),
        ),
        WRITE => (
            "Create or overwrite a file inside the workspace.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        ),
        EDIT => (
            "Replace an exact string in a file. Fails when old_string is absent or ambiguous.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"},
                    "replace_all": {"type": "boolean"}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        ),
        LS => (
            "List one directory inside the workspace.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "default ."}}
            }),
        ),
        GLOB => (
            "Find files by glob pattern (supports *, ?, **).",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "description": "base dir, default ."}
                },
                "required": ["pattern"]
            }),
        ),
        GREP => (
            "Search file contents for a literal substring.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "description": "base dir, default ."},
                    "glob": {"type": "string", "description": "file filter, e.g. **/*.rs"},
                    "ignore_case": {"type": "boolean"}
                },
                "required": ["pattern"]
            }),
        ),
        SHELL => (
            "Run a shell command in the sandbox (/workspace). Output is truncated.",
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout_secs": {"type": "integer"}
                },
                "required": ["command"]
            }),
        ),
        RUN_CODE => (
            "Write a snippet to the workspace and execute it (python/sh/node).",
            json!({
                "type": "object",
                "properties": {
                    "language": {"type": "string", "enum": ["python", "sh", "node"]},
                    "code": {"type": "string"},
                    "timeout_secs": {"type": "integer"}
                },
                "required": ["language", "code"]
            }),
        ),
        GIT => (
            "Run a git subcommand in a workspace repository.",
            json!({
                "type": "object",
                "properties": {
                    "sub": {"type": "string", "description": "status/diff/log/add/commit..."},
                    "args": {"type": "array", "items": {"type": "string"}},
                    "path": {"type": "string", "description": "repo dir, default ."}
                },
                "required": ["sub"]
            }),
        ),
        WEB_FETCH => (
            "Fetch an http(s) URL. Private/internal addresses are blocked.",
            json!({
                "type": "object",
                "properties": {"url": {"type": "string"}},
                "required": ["url"]
            }),
        ),
        TODO => (
            "Maintain a per-run task list. Send {\"clear\":true} to reset.",
            json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "done"]}
                            },
                            "required": ["text"]
                        }
                    },
                    "clear": {"type": "boolean"}
                }
            }),
        ),
        ASK_USER => (
            "Ask the user a clarifying question and stop guessing.",
            json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string"},
                    "options": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["question"]
            }),
        ),
        SUBAGENT => (
            "Delegate a self-contained subtask to a child agent. The child runs with its own \
             budget, cannot write files by default, and answers with a summary. Prefer doing \
             small work yourself.",
            json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "self-contained instruction for the child agent"
                    },
                    "agent": {
                        "type": "string",
                        "description": "role label, default `general`"
                    },
                    "model": {
                        "type": "string",
                        "description": "optional `<provider>:<model>` override"
                    }
                },
                "required": ["task"]
            }),
        ),
        _ => ("Unknown tool.", json!({"type": "object", "properties": {}})),
    };
    ToolDef::new(name, description, parameters)
}

/// 单次 run 的工具状态（todo 清单 + web 客户端 + 子 Agent 调用器）。
pub struct ToolState {
    todo: Mutex<Vec<Value>>,
    pub web: Arc<WebClient>,
    /// B6：由 `subagent::runner` 注入；为 `None` 时 `subagent` 工具明确失败。
    subagent: Option<Arc<dyn Invoker>>,
}

impl Default for ToolState {
    fn default() -> Self {
        Self::new(Arc::new(WebClient::default()))
    }
}

impl ToolState {
    pub fn new(web: Arc<WebClient>) -> Self {
        Self {
            todo: Mutex::new(Vec::new()),
            web,
            subagent: None,
        }
    }

    /// 挂上子 Agent 调用器（父 run 在 `persist::execute` 里装配）。
    pub fn with_subagent(mut self, invoker: Arc<dyn Invoker>) -> Self {
        self.subagent = Some(invoker);
        self
    }

    /// 子 Agent 自己也要有"再派生"的入口；`None` 表示到达深度上限。
    pub fn with_subagent_opt(mut self, invoker: Option<Arc<dyn Invoker>>) -> Self {
        self.subagent = invoker;
        self
    }

    pub fn subagent_invoker(&self) -> Option<Arc<dyn Invoker>> {
        self.subagent.clone()
    }

    pub fn todo_items(&self) -> Vec<Value> {
        self.todo.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, AgentError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| AgentError::Tool(format!("missing argument `{key}`")))
}

fn arg_opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(Value::as_u64)
}

fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// 工具入口。`name` 未知时直接失败——注册表与执行器保持一致。
pub async fn execute(
    state: &ToolState,
    sandbox: &dyn Sandbox,
    name: &str,
    args: &Value,
) -> Result<Value, AgentError> {
    let args = if args.is_object() {
        args.clone()
    } else {
        json!({})
    };
    match name {
        READ => read_file(sandbox, &args).await,
        WRITE => write_file(sandbox, &args).await,
        EDIT => edit_file(sandbox, &args).await,
        LS => list_dir(sandbox, &args).await,
        GLOB => glob(sandbox, &args).await,
        GREP => grep(sandbox, &args).await,
        SHELL => shell(sandbox, &args).await,
        RUN_CODE => run_code(sandbox, &args).await,
        GIT => git(sandbox, &args).await,
        WEB_FETCH => web_fetch(state, &args).await,
        TODO => todo(state, &args),
        ASK_USER => ask_user(&args),
        SUBAGENT => subagent(state, &args).await,
        other => Err(AgentError::Tool(format!("unknown tool `{other}`"))),
    }
}

async fn read_file(sandbox: &dyn Sandbox, args: &Value) -> Result<Value, AgentError> {
    let path = arg_str(args, "path")?;
    let bytes = sandbox.read(path).await?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let offset = arg_u64(args, "offset").unwrap_or(1).max(1) as usize;
    let limit = arg_u64(args, "limit").unwrap_or(READ_MAX_LINES as u64) as usize;
    let limit = limit.clamp(1, READ_MAX_LINES);
    let start = (offset - 1).min(total);
    let end = (start + limit).min(total);
    let body = lines[start..end].join("\n");
    Ok(json!({
        "path": path,
        "content": body,
        "total_lines": total,
        "start_line": start + 1,
        "end_line": end,
        "truncated": end < total,
    }))
}

async fn write_file(sandbox: &dyn Sandbox, args: &Value) -> Result<Value, AgentError> {
    let path = arg_str(args, "path")?;
    let content = args
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::Tool("missing argument `content`".into()))?;
    sandbox.write(path, content.as_bytes()).await?;
    Ok(json!({ "path": path, "bytes": content.len() }))
}

async fn edit_file(sandbox: &dyn Sandbox, args: &Value) -> Result<Value, AgentError> {
    let path = arg_str(args, "path")?;
    let old = arg_str(args, "old_string")?;
    let new = args
        .get("new_string")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::Tool("missing argument `new_string`".into()))?;
    let replace_all = arg_bool(args, "replace_all");
    let bytes = sandbox.read(path).await?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let occurrences = text.matches(old).count();
    if occurrences == 0 {
        return Err(AgentError::Tool(format!("old_string not found in {path}")));
    }
    if occurrences > 1 && !replace_all {
        return Err(AgentError::Tool(format!(
            "old_string appears {occurrences} times in {path}; pass replace_all=true or add context"
        )));
    }
    let updated = if replace_all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    sandbox.write(path, updated.as_bytes()).await?;
    Ok(json!({
        "path": path,
        "replacements": if replace_all { occurrences } else { 1 },
    }))
}

async fn list_dir(sandbox: &dyn Sandbox, args: &Value) -> Result<Value, AgentError> {
    let path = arg_opt_str(args, "path").unwrap_or(".");
    let entries = sandbox.list(path).await?;
    Ok(json!({ "path": path, "entries": entries }))
}

/// 从 base 起递归收集（相对路径, 是否目录）。深度与条数都有上限。
async fn walk(sandbox: &dyn Sandbox, base: &str) -> Result<Vec<(String, bool)>, AgentError> {
    let mut out = Vec::new();
    let mut queue: Vec<(String, usize)> = vec![(base.to_string(), 0)];
    while let Some((dir, depth)) = queue.pop() {
        if depth > WALK_MAX_DEPTH || out.len() >= WALK_MAX_ENTRIES {
            break;
        }
        let entries = match sandbox.list(&dir).await {
            Ok(entries) => entries,
            Err(_) => continue, // 不可读目录：跳过而不是整个工具失败
        };
        for entry in entries {
            if out.len() >= WALK_MAX_ENTRIES {
                break;
            }
            if entry.name == ".git" {
                continue;
            }
            let child = join_rel(&dir, &entry.name);
            out.push((child.clone(), entry.is_dir));
            if entry.is_dir {
                queue.push((child, depth + 1));
            }
        }
    }
    Ok(out)
}

fn join_rel(base: &str, name: &str) -> String {
    if base.is_empty() || base == "." {
        name.to_string()
    } else {
        format!("{}/{}", base.trim_end_matches('/'), name)
    }
}

async fn glob(sandbox: &dyn Sandbox, args: &Value) -> Result<Value, AgentError> {
    let pattern = arg_str(args, "pattern")?;
    let base = arg_opt_str(args, "path").unwrap_or(".");
    let entries = walk(sandbox, base).await?;
    let matches: Vec<&String> = entries
        .iter()
        .filter(|(path, _)| glob_match(pattern, path))
        .map(|(path, _)| path)
        .take(WALK_MAX_ENTRIES)
        .collect();
    Ok(json!({
        "pattern": pattern,
        "base": base,
        "matches": matches,
        "count": matches.len(),
    }))
}

async fn grep(sandbox: &dyn Sandbox, args: &Value) -> Result<Value, AgentError> {
    let needle = arg_str(args, "pattern")?;
    let base = arg_opt_str(args, "path").unwrap_or(".");
    let filter = arg_opt_str(args, "glob");
    let ignore_case = arg_bool(args, "ignore_case");
    let needle_cmp = if ignore_case {
        needle.to_lowercase()
    } else {
        needle.to_string()
    };

    let entries = walk(sandbox, base).await?;
    let mut hits = Vec::new();
    for (path, is_dir) in entries {
        if is_dir || hits.len() >= GREP_MAX_RESULTS {
            continue;
        }
        if let Some(filter) = filter {
            if !glob_match(filter, &path) {
                continue;
            }
        }
        let bytes = match sandbox.read(&path).await {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        if bytes.len() > GREP_MAX_FILE_BYTES {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (idx, line) in text.lines().enumerate() {
            let haystack = if ignore_case {
                line.to_lowercase()
            } else {
                line.to_string()
            };
            if haystack.contains(&needle_cmp) {
                hits.push(json!({
                    "path": path,
                    "line": idx + 1,
                    "text": line.chars().take(400).collect::<String>(),
                }));
                if hits.len() >= GREP_MAX_RESULTS {
                    break;
                }
            }
        }
    }
    Ok(json!({ "pattern": needle, "matches": hits, "count": hits.len() }))
}

async fn shell(sandbox: &dyn Sandbox, args: &Value) -> Result<Value, AgentError> {
    let command = arg_str(args, "command")?;
    let timeout = arg_u64(args, "timeout_secs").filter(|t| *t > 0);
    let out = sandbox.exec(command, timeout).await?;
    Ok(out.compact())
}

async fn run_code(sandbox: &dyn Sandbox, args: &Value) -> Result<Value, AgentError> {
    let language = arg_str(args, "language")?.to_ascii_lowercase();
    let code = args
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::Tool("missing argument `code`".into()))?;
    let (interpreter, ext) = match language.as_str() {
        "python" | "python3" | "py" => ("python3", "py"),
        "sh" | "bash" | "shell" => ("sh", "sh"),
        "node" | "javascript" | "js" => ("node", "js"),
        other => {
            return Err(AgentError::Tool(format!(
                "unsupported language `{other}` (python/sh/node)"
            )))
        }
    };
    let path = format!(".chiron/run_{}.{}", Uuid::new_v4().simple(), ext);
    sandbox.write(&path, code.as_bytes()).await?;
    let command = format!("{} {}", interpreter, shell_quote(&path));
    let timeout = arg_u64(args, "timeout_secs").filter(|t| *t > 0);
    let mut value = sandbox.exec(&command, timeout).await?.compact();
    if let Some(map) = value.as_object_mut() {
        map.insert("path".into(), json!(path));
    }
    Ok(value)
}

async fn git(sandbox: &dyn Sandbox, args: &Value) -> Result<Value, AgentError> {
    let sub = arg_str(args, "sub")?;
    let repo = arg_opt_str(args, "path").unwrap_or(".");
    // 先做路径钳制，避免 `path` 被当成容器内任意目录。
    tenant_rel_path(repo)
        .map_err(|e| AgentError::Tool(format!("invalid repo path: {}", e.message)))?;
    let extra: Vec<String> = args
        .get("args")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let out: ExecOutcome = sandbox.git(repo, sub, &extra).await?;
    Ok(out.compact())
}

async fn web_fetch(state: &ToolState, args: &Value) -> Result<Value, AgentError> {
    let url = arg_str(args, "url")?;
    let page = state.web.fetch(url).await?;
    Ok(json!(page))
}

fn todo(state: &ToolState, args: &Value) -> Result<Value, AgentError> {
    let mut guard = state
        .todo
        .lock()
        .map_err(|_| AgentError::Internal("todo lock poisoned".into()))?;
    if arg_bool(args, "clear") {
        guard.clear();
    }
    if let Some(items) = args.get("items").and_then(Value::as_array) {
        for item in items.iter().take(TODO_MAX_ITEMS) {
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty());
            let Some(text) = text else { continue };
            let status = item
                .get("status")
                .and_then(Value::as_str)
                .filter(|s| matches!(*s, "pending" | "in_progress" | "done"))
                .unwrap_or("pending");
            guard.push(json!({ "text": text, "status": status }));
        }
        if guard.len() > TODO_MAX_ITEMS {
            let overflow = guard.len() - TODO_MAX_ITEMS;
            guard.drain(0..overflow);
        }
    }
    Ok(json!({ "items": guard.clone(), "count": guard.len() }))
}

fn ask_user(args: &Value) -> Result<Value, AgentError> {
    let question = arg_str(args, "question")?;
    let options: Vec<String> = args
        .get("options")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({
        "status": "awaiting_user",
        "question": question,
        "options": options,
        "note": "The question is shown to the user; continue after their next message.",
    }))
}

/// 子 Agent 派生：真正的执行在 [`crate::subagent::Invoker`] 后面（准入/预算/事件都在那边）。
async fn subagent(state: &ToolState, args: &Value) -> Result<Value, AgentError> {
    let invoker = state
        .subagent_invoker()
        .ok_or_else(|| AgentError::Tool("subagent tool is not available for this run".into()))?;
    invoker.invoke(args).await
}

/// `*` 匹配除 `/` 外任意串，`**` 匹配任意层目录，`?` 匹配单个字符。
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&pat, &parts)
}

fn match_segments(pat: &[&str], parts: &[&str]) -> bool {
    if pat.is_empty() {
        return parts.is_empty();
    }
    if pat[0] == "**" {
        for skip in 0..=parts.len() {
            if match_segments(&pat[1..], &parts[skip..]) {
                return true;
            }
        }
        return false;
    }
    if parts.is_empty() {
        return false;
    }
    seg_match(pat[0], parts[0]) && match_segments(&pat[1..], &parts[1..])
}

fn seg_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::workspace::FakeSandbox;

    fn sandbox() -> (tempfile::TempDir, FakeSandbox) {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = FakeSandbox::new(dir.path().to_path_buf());
        (dir, sandbox)
    }

    fn state() -> ToolState {
        ToolState::new(Arc::new(WebClient::new(true)))
    }

    #[test]
    fn glob_matching_handles_star_star_and_question() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("**/*.rs", "src/main.rs"));
        assert!(glob_match("src/*.rs", "src/lib.rs"));
        assert!(!glob_match("src/*.rs", "src/deep/lib.rs"));
        assert!(glob_match("a?c.txt", "abc.txt"));
        assert!(!glob_match("a?c.txt", "abbc.txt"));
        assert!(glob_match("**/test_*.py", "pkg/sub/test_x.py"));
        assert!(glob_match("**", "anything/at/all.txt"));
    }

    #[test]
    fn policies_default_to_ask_for_mutating_tools() {
        let set = Toolset::default_set();
        assert_eq!(set.policy(READ), Policy::Allow);
        assert_eq!(set.policy(LS), Policy::Allow);
        assert_eq!(set.policy(SHELL), Policy::Ask);
        assert_eq!(set.policy(WEB_FETCH), Policy::Ask);
        assert_eq!(set.policy("bogus"), Policy::Deny);
        for name in ALL_TOOLS {
            assert!(mutating(name) || set.policy(name) == Policy::Allow);
        }
    }

    #[test]
    fn defs_skip_denied_tools_and_are_sorted() {
        let set = Toolset::allow_all().with(SHELL, Policy::Deny);
        let defs = set.defs();
        assert!(defs.iter().all(|d| d.name != SHELL));
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        assert_eq!(names.len(), ALL_TOOLS.len() - 1);
        assert!(Toolset::deny_all().defs().is_empty());
    }

    #[test]
    fn policy_parsing_and_labels() {
        assert_eq!(Policy::parse("ALLOW"), Some(Policy::Allow));
        assert_eq!(Policy::parse("auto"), Some(Policy::Allow));
        assert_eq!(Policy::parse("ask"), Some(Policy::Ask));
        assert_eq!(Policy::parse("deny"), Some(Policy::Deny));
        assert_eq!(Policy::parse("maybe"), None);
        assert_eq!(Policy::Ask.as_str(), "ask");
    }

    #[tokio::test]
    async fn write_then_read_roundtrip_with_line_window() {
        let (_dir, sandbox) = sandbox();
        let state = state();
        let out = execute(
            &state,
            &sandbox,
            WRITE,
            &json!({"path": "src/a.rs", "content": "l1\nl2\nl3"}),
        )
        .await
        .unwrap();
        assert_eq!(out["bytes"], 8);

        let out = execute(
            &state,
            &sandbox,
            READ,
            &json!({"path": "src/a.rs", "offset": 2, "limit": 1}),
        )
        .await
        .unwrap();
        assert_eq!(out["content"], "l2");
        assert_eq!(out["total_lines"], 3);
        assert_eq!(out["truncated"], true);
    }

    #[tokio::test]
    async fn edit_requires_unique_match_unless_replace_all() {
        let (_dir, sandbox) = sandbox();
        let state = state();
        execute(
            &state,
            &sandbox,
            WRITE,
            &json!({"path": "f.txt", "content": "a a b"}),
        )
        .await
        .unwrap();

        let err = execute(
            &state,
            &sandbox,
            EDIT,
            &json!({"path": "f.txt", "old_string": "a", "new_string": "z"}),
        )
        .await
        .expect_err("ambiguous");
        assert!(matches!(err, AgentError::Tool(_)));

        let out = execute(
            &state,
            &sandbox,
            EDIT,
            &json!({"path": "f.txt", "old_string": "a", "new_string": "z", "replace_all": true}),
        )
        .await
        .unwrap();
        assert_eq!(out["replacements"], 2);
        let content = execute(&state, &sandbox, READ, &json!({"path": "f.txt"}))
            .await
            .unwrap();
        assert_eq!(content["content"], "z z b");
    }

    #[tokio::test]
    async fn edit_reports_missing_old_string() {
        let (_dir, sandbox) = sandbox();
        let state = state();
        execute(
            &state,
            &sandbox,
            WRITE,
            &json!({"path": "f.txt", "content": "x"}),
        )
        .await
        .unwrap();
        let err = execute(
            &state,
            &sandbox,
            EDIT,
            &json!({"path": "f.txt", "old_string": "nope", "new_string": "y"}),
        )
        .await
        .expect_err("missing");
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn ls_glob_and_grep_walk_the_workspace() {
        let (_dir, sandbox) = sandbox();
        let state = state();
        for (path, content) in [
            ("src/main.rs", "fn main() { hello }"),
            ("src/lib.rs", "pub fn lib() {}"),
            ("docs/readme.md", "hello world"),
        ] {
            execute(
                &state,
                &sandbox,
                WRITE,
                &json!({"path": path, "content": content}),
            )
            .await
            .unwrap();
        }

        let out = execute(&state, &sandbox, LS, &json!({"path": "src"}))
            .await
            .unwrap();
        assert_eq!(out["entries"].as_array().unwrap().len(), 2);

        let out = execute(&state, &sandbox, GLOB, &json!({"pattern": "**/*.rs"}))
            .await
            .unwrap();
        let mut matches: Vec<&str> = out["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        matches.sort_unstable();
        assert_eq!(matches, vec!["src/lib.rs", "src/main.rs"]);

        let out = execute(&state, &sandbox, GREP, &json!({"pattern": "hello"}))
            .await
            .unwrap();
        assert_eq!(out["count"], 2);
        let out = execute(
            &state,
            &sandbox,
            GREP,
            &json!({"pattern": "HELLO", "ignore_case": true, "glob": "**/*.md"}),
        )
        .await
        .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["matches"][0]["path"], "docs/readme.md");
    }

    #[tokio::test]
    async fn shell_and_run_code_go_through_the_sandbox() {
        let (_dir, sandbox) = sandbox();
        let state = state();
        let out = execute(&state, &sandbox, SHELL, &json!({"command": "echo hi"}))
            .await
            .unwrap();
        assert_eq!(out["exit_code"], 0);

        let out = execute(
            &state,
            &sandbox,
            RUN_CODE,
            &json!({"language": "python", "code": "print(1)"}),
        )
        .await
        .unwrap();
        let path = out["path"].as_str().unwrap();
        assert!(path.starts_with(".chiron/run_") && path.ends_with(".py"));

        let err = execute(
            &state,
            &sandbox,
            RUN_CODE,
            &json!({"language": "ruby", "code": "puts 1"}),
        )
        .await
        .expect_err("unsupported language");
        assert!(err.to_string().contains("unsupported language"));
    }

    #[tokio::test]
    async fn git_is_quoted_and_path_clamped() {
        let (_dir, sandbox) = sandbox();
        let state = state();
        let out = execute(
            &state,
            &sandbox,
            GIT,
            &json!({"sub": "status", "args": ["--porcelain"], "path": "repo"}),
        )
        .await
        .unwrap();
        assert_eq!(out["exit_code"], 0);
        let calls = sandbox.calls();
        assert!(calls[0].contains("git -C /workspace/repo --no-pager status"));
        assert!(calls[0].contains("--porcelain"));

        let err = execute(
            &state,
            &sandbox,
            GIT,
            &json!({"sub": "status", "path": "../etc"}),
        )
        .await
        .expect_err("escape");
        assert!(err.to_string().contains("invalid repo path"));
    }

    #[tokio::test]
    async fn todo_accumulates_clears_and_caps() {
        let state = state();
        let out = execute(
            &state,
            &FakeSandbox::new(std::env::temp_dir()),
            TODO,
            &json!({"items": [{"text": "a"}, {"text": " b ", "status": "done"}]}),
        )
        .await
        .unwrap();
        assert_eq!(out["count"], 2);
        assert_eq!(out["items"][1]["status"], "done");

        let out = execute(
            &state,
            &FakeSandbox::new(std::env::temp_dir()),
            TODO,
            &json!({"clear": true}),
        )
        .await
        .unwrap();
        assert_eq!(out["count"], 0);
    }

    #[tokio::test]
    async fn ask_user_echoes_question_and_options() {
        let state = state();
        let out = execute(
            &state,
            &FakeSandbox::new(std::env::temp_dir()),
            ASK_USER,
            &json!({"question": "which db?", "options": ["pg", "sqlite"]}),
        )
        .await
        .unwrap();
        assert_eq!(out["status"], "awaiting_user");
        assert_eq!(out["options"][0], "pg");
    }

    #[tokio::test]
    async fn unknown_tool_and_missing_args_fail_cleanly() {
        let state = state();
        let sandbox = FakeSandbox::new(std::env::temp_dir());
        let err = execute(&state, &sandbox, "nope", &json!({}))
            .await
            .expect_err("unknown");
        assert!(err.to_string().contains("unknown tool"));
        let err = execute(&state, &sandbox, READ, &json!({}))
            .await
            .expect_err("missing path");
        assert!(err.to_string().contains("missing argument"));
    }

    /// B6：`subagent` 工具只依赖注入的调用器，因此这里不需要数据库/真 provider。
    struct FakeInvoker {
        calls: Mutex<Vec<Value>>,
    }

    #[async_trait::async_trait]
    impl Invoker for FakeInvoker {
        async fn invoke(&self, args: &Value) -> Result<Value, AgentError> {
            self.calls.lock().unwrap().push(args.clone());
            Ok(json!({
                "subagent_id": "11111111-1111-1111-1111-111111111111",
                "status": "completed",
                "summary": "[completed] done",
            }))
        }
    }

    #[tokio::test]
    async fn subagent_tool_requires_an_invoker_and_delegates() {
        let sandbox = FakeSandbox::new(std::env::temp_dir());
        let args = json!({"task": "summarize the repo"});

        let err = execute(&state(), &sandbox, SUBAGENT, &args)
            .await
            .expect_err("no invoker");
        assert!(err.to_string().contains("not available"), "{err}");
        assert_eq!(Toolset::allow_all().defs().len(), ALL_TOOLS.len());
        assert_eq!(Toolset::default_set().policy(SUBAGENT), Policy::Allow);

        let invoker = Arc::new(FakeInvoker {
            calls: Mutex::new(Vec::new()),
        });
        let state = ToolState::new(Arc::new(WebClient::new(true))).with_subagent(invoker.clone());
        let out = execute(&state, &sandbox, SUBAGENT, &args)
            .await
            .expect("delegated");
        assert_eq!(out["status"], "completed");
        assert!(out["summary"].as_str().unwrap().starts_with("[completed]"));
        assert_eq!(invoker.calls.lock().unwrap().len(), 1);
        assert_eq!(
            invoker.calls.lock().unwrap()[0]["task"],
            "summarize the repo"
        );
    }
}
