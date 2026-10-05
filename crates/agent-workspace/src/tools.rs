//! Workspace tools. The agent id is bound on the runtime. A tool argument cannot select another
//! agent's directory.

use async_trait::async_trait;
use liberado_provider::{ToolDef, ToolInvocation};
use liberado_tool_runtime::ToolRuntime;
use serde_json::{Value, json};

use crate::error::WorkspaceError;
use crate::workspace::{AgentWorkspace, DirEntry, EntryKind};

pub const TOOL_LIST: &str = "workspace_list";
pub const TOOL_READ: &str = "workspace_read";
pub const TOOL_WRITE: &str = "workspace_write";
pub const TOOL_DELETE: &str = "workspace_delete";
pub const TOOL_DOWNLOAD: &str = "workspace_download";

pub const WORKSPACE_TOOL_NAMES: &[&str] =
    &[TOOL_LIST, TOOL_READ, TOOL_WRITE, TOOL_DELETE, TOOL_DOWNLOAD];

const PRIVATE: &str = "Private to this agent. This is not a shared scratch directory. Other \
agents cannot see these files. Share through a channel or a local git repository.";

pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            TOOL_LIST,
            format!("List one directory in this agent's private workspace. {PRIVATE}"),
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory relative to the workspace root. Omit or pass \".\" for the root."
                    }
                }
            }),
        ),
        ToolDef::new(
            TOOL_READ,
            format!("Read a UTF-8 text file from this agent's private workspace. {PRIVATE}"),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File relative to the workspace root." }
                },
                "required": ["path"]
            }),
        ),
        ToolDef::new(
            TOOL_WRITE,
            format!(
                "Create or replace a UTF-8 text file in this agent's private workspace. The byte cap applies. {PRIVATE}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File relative to the workspace root." },
                    "content": { "type": "string", "description": "Full new contents of the file." }
                },
                "required": ["path", "content"]
            }),
        ),
        ToolDef::new(
            TOOL_DELETE,
            format!("Delete a file or directory in this agent's private workspace. {PRIVATE}"),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the workspace root. The root itself cannot be deleted." }
                },
                "required": ["path"]
            }),
        ),
        ToolDef::new(
            TOOL_DOWNLOAD,
            format!(
                "Download an http or https URL into a file in this agent's private workspace. The byte cap applies. {PRIVATE}"
            ),
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "http or https URL." },
                    "path": { "type": "string", "description": "Destination file relative to the workspace root." }
                },
                "required": ["url", "path"]
            }),
        ),
    ]
}

pub fn is_workspace_tool(name: &str) -> bool {
    WORKSPACE_TOOL_NAMES.contains(&name)
}

pub fn apply(workspace: &AgentWorkspace, name: &str, args: &Value) -> Result<String, String> {
    let result = match name {
        TOOL_LIST => list_tool(workspace, args),
        TOOL_READ => read_tool(workspace, args),
        TOOL_WRITE => write_tool(workspace, args),
        TOOL_DELETE => delete_tool(workspace, args),
        TOOL_DOWNLOAD => download_tool(workspace, args),
        _ => return Err(format!("unknown workspace tool {name}")),
    };
    result.map_err(|err| err.to_string())
}

fn list_tool(workspace: &AgentWorkspace, args: &Value) -> Result<String, WorkspaceError> {
    let path = optional_path(args);
    let entries = workspace.list(&path)?;
    let used = workspace.usage_bytes()?;
    Ok(format_list(&entries, used, workspace.max_bytes()))
}

fn read_tool(workspace: &AgentWorkspace, args: &Value) -> Result<String, WorkspaceError> {
    workspace.read_text(&required_path(args)?)
}

fn write_tool(workspace: &AgentWorkspace, args: &Value) -> Result<String, WorkspaceError> {
    let path = required_path(args)?;
    let content = required_content(args)?;
    let bytes = workspace.write_text(&path, &content)?;
    let used = workspace.usage_bytes()?;
    Ok(format!(
        "Wrote {bytes} bytes to {path}. Using {used} of {} bytes.",
        workspace.max_bytes()
    ))
}

fn delete_tool(workspace: &AgentWorkspace, args: &Value) -> Result<String, WorkspaceError> {
    let path = required_path(args)?;
    workspace.delete(&path)?;
    let used = workspace.usage_bytes()?;
    Ok(format!(
        "Deleted {path}. Using {used} of {} bytes.",
        workspace.max_bytes()
    ))
}

fn download_tool(workspace: &AgentWorkspace, args: &Value) -> Result<String, WorkspaceError> {
    let path = required_path(args)?;
    let url = required_str(args, "url")?;
    let bytes = workspace.download_url(&path, &url)?;
    let used = workspace.usage_bytes()?;
    Ok(format!(
        "Downloaded {bytes} bytes to {path}. Using {used} of {} bytes.",
        workspace.max_bytes()
    ))
}

fn format_list(entries: &[DirEntry], used: u64, cap: u64) -> String {
    let mut out =
        format!("Using {used} of {cap} bytes. This directory is private to this agent.\n");
    if entries.is_empty() {
        out.push_str("(empty)");
        return out;
    }
    for entry in entries {
        match entry.kind {
            EntryKind::Dir => out.push_str(&format!("{}/\tdir\n", entry.name)),
            EntryKind::File => out.push_str(&format!("{}\tfile\t{}\n", entry.name, entry.bytes)),
            EntryKind::Symlink => out.push_str(&format!("{}\tsymlink\n", entry.name)),
        }
    }
    out
}

fn optional_path(args: &Value) -> String {
    args.get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .unwrap_or(".")
        .to_string()
}

fn required_path(args: &Value) -> Result<String, WorkspaceError> {
    required_str(args, "path")
}

fn required_content(args: &Value) -> Result<String, WorkspaceError> {
    args.get("content")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| WorkspaceError::BadPath("`content` must be a string".into()))
}

fn required_str(args: &Value, key: &str) -> Result<String, WorkspaceError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| WorkspaceError::BadPath(format!("`{key}` must be a non-empty string")))
}

/// Tools for one agent, in front of another [`ToolRuntime`].
pub struct WorkspaceRuntime {
    workspace: AgentWorkspace,
    inner: Box<dyn ToolRuntime>,
}

impl WorkspaceRuntime {
    pub fn new(workspace: AgentWorkspace, inner: Box<dyn ToolRuntime>) -> Self {
        Self { workspace, inner }
    }
}

#[async_trait]
impl ToolRuntime for WorkspaceRuntime {
    fn catalog(&self) -> Vec<ToolDef> {
        let mut tools = tool_defs();
        tools.extend(self.inner.catalog());
        tools
    }

    async fn invoke(&self, call: &ToolInvocation) -> Result<String, String> {
        if !is_workspace_tool(&call.name) {
            return self.inner.invoke(call).await;
        }
        let workspace = self.workspace.clone();
        let name = call.name.clone();
        let args = call.arguments.clone();
        // File and network IO stays off the async runtime. List uses this same
        // path so there is one implementation for every workspace tool.
        tokio::task::spawn_blocking(move || apply(&workspace, &name, &args))
            .await
            .map_err(|err| format!("workspace tool failed: {err}"))?
    }

    fn is_read_only(&self, tool_name: &str) -> bool {
        match tool_name {
            TOOL_LIST | TOOL_READ => true,
            TOOL_WRITE | TOOL_DELETE | TOOL_DOWNLOAD => false,
            _ => self.inner.is_read_only(tool_name),
        }
    }

    fn parks_for_human(&self, tool_name: &str) -> bool {
        if is_workspace_tool(tool_name) {
            false
        } else {
            self.inner.parks_for_human(tool_name)
        }
    }
}
