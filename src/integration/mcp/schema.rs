//! MCP 工具 schema 的校验与装配。
//!
//! 外部 server 是不可信输入：`tools/list` 的名字/描述/schema 都要过筛子，
//! 调用参数也要按 schema 校验后再转发——模型幻觉出来的字段不能变成对端的
//! 解析错误或注入面。

use serde_json::{json, Value};

use crate::provider::ToolDef;

use super::jsonrpc::McpError;

/// 单台 server 暴露的工具上限（防止把上下文一次撑爆）。
pub const MAX_TOOLS: usize = 128;
/// schema 递归深度上限。
pub const MAX_SCHEMA_DEPTH: usize = 12;
/// 工具名长度上限（与 OpenAI function name 限制一致）。
pub const MAX_TOOL_NAME: usize = 64;

/// 校验后的工具声明。
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl McpTool {
    /// 对模型暴露的工具名：`mcp__<server>__<tool>`，保证不同 server 不撞名，
    /// 也让审计/审批能从名字反推来源。
    pub fn qualified_name(&self, server: &str) -> String {
        let prefix = sanitize_name(server, 24);
        let tool = sanitize_name(&self.name, MAX_TOOL_NAME - prefix.chars().count() - 6);
        format!("mcp__{prefix}__{tool}")
    }

    pub fn to_tool_def(&self, server: &str) -> ToolDef {
        ToolDef::new(
            self.qualified_name(server),
            format!("[mcp:{server}] {}", self.description),
            self.input_schema.clone(),
        )
    }
}

/// 只保留 `[A-Za-z0-9_.-]`，压缩连续下划线并截断；空名降级为 `tool`。
fn sanitize_name(raw: &str, max: usize) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_underscore = false;
    for ch in raw.chars() {
        let mapped = if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
            ch.to_ascii_lowercase()
        } else {
            '_'
        };
        if mapped == '_' {
            if last_underscore {
                continue;
            }
            last_underscore = true;
        } else {
            last_underscore = false;
        }
        out.push(mapped);
    }
    let trimmed = out.trim_matches('_').to_string();
    let capped: String = trimmed.chars().take(max.max(1)).collect();
    if capped.is_empty() {
        "tool".to_string()
    } else {
        capped
    }
}

/// 校验 `tools/list` 的 `result`，产出去重后的工具列表。
pub fn validate_tool_list(result: &Value) -> Result<Vec<McpTool>, McpError> {
    let tools = result
        .get("tools")
        .ok_or_else(|| McpError::Schema("tools/list result has no `tools`".into()))?
        .as_array()
        .ok_or_else(|| McpError::Schema("`tools` must be an array".into()))?;
    if tools.len() > MAX_TOOLS {
        return Err(McpError::Schema(format!(
            "server exposes {} tools (limit {MAX_TOOLS})",
            tools.len()
        )));
    }

    let mut out: Vec<McpTool> = Vec::with_capacity(tools.len());
    for raw in tools {
        let name = raw
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| McpError::Schema("tool without a name".into()))?;
        if name.chars().count() > MAX_TOOL_NAME {
            return Err(McpError::Schema(format!(
                "tool name `{name}` exceeds {MAX_TOOL_NAME} chars"
            )));
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        {
            return Err(McpError::Schema(format!(
                "tool name `{name}` has illegal characters"
            )));
        }
        if out.iter().any(|t| t.name == name) {
            return Err(McpError::Schema(format!("duplicate tool name `{name}`")));
        }

        let description = raw
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .take(1024)
            .collect::<String>();

        // inputSchema 可缺省（无参工具）；给了就必须是 object schema。
        let input_schema = match raw.get("inputSchema") {
            None | Some(Value::Null) => json!({"type": "object", "properties": {}}),
            Some(schema) => {
                validate_schema(schema)?;
                schema.clone()
            }
        };

        out.push(McpTool {
            name: name.to_string(),
            description,
            input_schema,
        });
    }
    Ok(out)
}

/// 校验一份 JSON Schema 的"可安全透传"子集：必须是 object 类型、深度受限。
pub fn validate_schema(schema: &Value) -> Result<(), McpError> {
    check_schema_node(schema, 0)
}

fn check_schema_node(node: &Value, depth: usize) -> Result<(), McpError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(McpError::Schema("schema nesting too deep".into()));
    }
    let obj = node
        .as_object()
        .ok_or_else(|| McpError::Schema("schema node must be an object".into()))?;
    if depth == 0 {
        match obj.get("type").and_then(Value::as_str) {
            Some("object") | None => {}
            Some(other) => {
                return Err(McpError::Schema(format!(
                    "inputSchema type must be object, got `{other}`"
                )))
            }
        }
    }
    if let Some(properties) = obj.get("properties") {
        let map = properties
            .as_object()
            .ok_or_else(|| McpError::Schema("`properties` must be an object".into()))?;
        for (key, child) in map {
            if key.len() > 128 {
                return Err(McpError::Schema("property name too long".into()));
            }
            check_schema_node(child, depth + 1)?;
        }
    }
    if let Some(child) = obj.get("items") {
        check_schema_node(child, depth + 1)?;
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(items) = obj.get(key) {
            let list = items
                .as_array()
                .ok_or_else(|| McpError::Schema(format!("`{key}` must be an array")))?;
            if list.len() > 16 {
                return Err(McpError::Schema(format!("`{key}` has too many entries")));
            }
            for child in list {
                check_schema_node(child, depth + 1)?;
            }
        }
    }
    Ok(())
}

/// 按 schema 校验调用参数（只校验能判定的部分，未知关键字忽略）。
pub fn validate_arguments(schema: &Value, args: &Value) -> Result<(), McpError> {
    if !args.is_object() {
        return Err(McpError::Schema("tool arguments must be an object".into()));
    }
    let mut path = String::from("$");
    check_value(schema, args, 0, &mut path)
}

fn check_value(
    schema: &Value,
    value: &Value,
    depth: usize,
    path: &mut String,
) -> Result<(), McpError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(McpError::Schema("arguments nesting too deep".into()));
    }
    let Some(obj) = schema.as_object() else {
        return Ok(());
    };

    if let Some(expected) = obj.get("type") {
        if !type_matches(expected, value) {
            return Err(McpError::Schema(format!(
                "{path} must be {}, got {}",
                type_label(expected),
                value_label(value)
            )));
        }
    }

    if let Some(allowed) = obj.get("enum").and_then(Value::as_array) {
        if !allowed.iter().any(|candidate| candidate == value) {
            return Err(McpError::Schema(format!("{path} must be one of the enum")));
        }
    }

    match value {
        Value::Object(map) => {
            if let Some(required) = obj.get("required").and_then(Value::as_array) {
                for key in required.iter().filter_map(Value::as_str) {
                    if !map.contains_key(key) {
                        return Err(McpError::Schema(format!("{path} is missing `{key}`")));
                    }
                }
            }
            let properties = obj.get("properties").and_then(Value::as_object);
            if let Some(properties) = properties {
                for (key, child) in map {
                    if let Some(child_schema) = properties.get(key) {
                        let saved = path.clone();
                        path.push('.');
                        path.push_str(key);
                        check_value(child_schema, child, depth + 1, path)?;
                        *path = saved;
                    } else if obj.get("additionalProperties") == Some(&Value::Bool(false)) {
                        return Err(McpError::Schema(format!("{path} does not accept `{key}`")));
                    }
                }
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = obj.get("items") {
                if items.len() > 1000 {
                    return Err(McpError::Schema(format!("{path} has too many items")));
                }
                for (idx, item) in items.iter().enumerate() {
                    let saved = path.clone();
                    path.push_str(&format!("[{idx}]"));
                    check_value(item_schema, item, depth + 1, path)?;
                    *path = saved;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn type_matches(expected: &Value, value: &Value) -> bool {
    match expected {
        Value::String(kind) => single_type_matches(kind, value),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .any(|kind| single_type_matches(kind, value)),
        _ => true,
    }
}

fn single_type_matches(kind: &str, value: &Value) -> bool {
    match kind {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        // 未识别的类型词（如 `any`）不放行也不误杀：交给对端判定。
        _ => true,
    }
}

fn type_label(expected: &Value) -> String {
    match expected {
        Value::String(kind) => kind.clone(),
        Value::Array(kinds) => kinds
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("|"),
        _ => "any".to_string(),
    }
}

fn value_label(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str) -> Value {
        json!({
            "name": name,
            "description": "d",
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }
        })
    }

    #[test]
    fn discovers_valid_tools_and_qualifies_names() {
        let result = json!({"tools": [tool("read_file"), tool("search")]});
        let tools = validate_tool_list(&result).unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].qualified_name("fs"), "mcp__fs__read_file");
        let def = tools[0].to_tool_def("fs");
        assert_eq!(def.name, "mcp__fs__read_file");
        assert!(def.description.starts_with("[mcp:fs]"));
        assert_eq!(def.parameters["type"], "object");
    }

    #[test]
    fn rejects_bad_tool_shapes() {
        // 没有 tools
        assert!(validate_tool_list(&json!({})).is_err());
        // tools 不是数组
        assert!(validate_tool_list(&json!({"tools": {}})).is_err());
        // 名字非法字符
        assert!(validate_tool_list(&json!({"tools": [{"name": "a b"}]})).is_err());
        // 名字为空
        assert!(validate_tool_list(&json!({"tools": [{"name": "  "}]})).is_err());
        // 重名
        assert!(validate_tool_list(&json!({"tools": [tool("a"), tool("a")]})).is_err());
        // inputSchema 不是 object schema
        let err = validate_tool_list(&json!({
            "tools": [{"name": "a", "inputSchema": {"type": "string"}}]
        }))
        .unwrap_err();
        assert!(err.to_string().contains("must be object"));
        // 超量
        let many: Vec<Value> = (0..MAX_TOOLS + 1).map(|i| tool(&format!("t{i}"))).collect();
        assert!(validate_tool_list(&json!({"tools": many})).is_err());
    }

    #[test]
    fn missing_input_schema_defaults_to_object() {
        let tools = validate_tool_list(&json!({"tools": [{"name": "ping"}]})).unwrap();
        assert_eq!(
            tools[0].input_schema,
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(tools[0].description, "");
    }

    #[test]
    fn sanitizes_qualified_names() {
        let t = McpTool {
            name: "weird name!".into(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
        };
        let q = t.qualified_name("My Server/1");
        assert!(q.starts_with("mcp__my_server_1__"));
        assert!(q
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')));
        assert!(q.chars().count() <= MAX_TOOL_NAME);
    }

    #[test]
    fn argument_validation_covers_required_types_and_enum() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "limit": {"type": "integer"},
                "mode": {"type": "string", "enum": ["a", "b"]},
                "tags": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["path"],
            "additionalProperties": false
        });

        assert!(validate_arguments(&schema, &json!({"path": "x"})).is_ok());
        assert!(validate_arguments(&schema, &json!({})).is_err());
        assert!(validate_arguments(&schema, &json!({"path": 1})).is_err());
        assert!(validate_arguments(&schema, &json!({"path": "x", "limit": 1.5})).is_err());
        assert!(validate_arguments(&schema, &json!({"path": "x", "mode": "c"})).is_err());
        assert!(validate_arguments(&schema, &json!({"path": "x", "tags": ["a", 1]})).is_err());
        assert!(validate_arguments(&schema, &json!({"path": "x", "extra": 1})).is_err());
        assert!(validate_arguments(&schema, &json!([1, 2])).is_err());
    }

    #[test]
    fn schema_depth_is_bounded() {
        let mut node = json!({"type": "string"});
        for _ in 0..MAX_SCHEMA_DEPTH + 2 {
            node = json!({"type": "object", "properties": {"n": node}});
        }
        let err = validate_schema(&node).unwrap_err();
        assert!(err.to_string().contains("too deep"));
    }
}
