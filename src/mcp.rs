//! MCP configuration adapters. Clients own the actual transports and processes.
//! No command from a registry entry is executed by the manager.
use crate::{backup, config::ClientPaths, gui_settings, gui_worker::ClientTarget, i18n};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, Table};
use zeroize::Zeroizing;

fn tr(zh: &'static str, en: &'static str) -> &'static str {
    i18n::tr(zh, en)
}

#[cfg(test)]
#[path = "mcp/live_tests.rs"]
mod live_tests;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
enum Format {
    Codex,
    Json,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Binding {
    pub target: ClientTarget,
    path: PathBuf,
    format: Format,
    /// Object containing mcpServers, empty for a file's root object.
    scope: Vec<String>,
    key: String,
    native: Value,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub description: String,
    pub config: Value,
    pub bindings: Vec<Binding>,
    pub registry_name: Option<String>,
}

impl Server {
    pub fn is_codex_builtin(&self) -> bool {
        self.name == "node_repl"
            && self.bindings.iter().any(|binding| {
                matches!(
                    binding.target,
                    ClientTarget::CodexCli | ClientTarget::CodexDesktop
                ) && binding.key == "node_repl"
            })
            && self
                .config
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(is_codex_node_repl_command)
    }

    pub fn display_description(&self) -> &str {
        if self.is_codex_builtin() {
            tr(
                "Codex 内置服务，用于执行 JavaScript，并支持浏览器和电脑操作。由 Codex 管理，只读显示。",
                "Built-in Codex service for running JavaScript and supporting browser and computer use. Managed by Codex; read-only.",
            )
        } else {
            &self.description
        }
    }

    pub fn targets(&self) -> Vec<ClientTarget> {
        ClientTarget::ALL
            .into_iter()
            .filter(|t| self.bindings.iter().any(|b| b.target == *t))
            .collect()
    }
    pub fn enabled(&self) -> bool {
        self.bindings.iter().any(|b| b.enabled)
    }
    pub fn transport(&self) -> &str {
        if self.config.get("command").is_some() {
            "stdio"
        } else if self.config["type"] == "sse" {
            "SSE"
        } else {
            "HTTP"
        }
    }
}

fn is_codex_node_repl_command(command: &str) -> bool {
    let normalized = command.replace('\\', "/").to_ascii_lowercase();
    let absolute = normalized.starts_with('/')
        || (normalized
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
            && normalized.as_bytes().get(1..3) == Some(b":/"));
    if !absolute
        || normalized
            .split('/')
            .any(|part| part == "." || part == "..")
    {
        return false;
    }
    [
        (
            "/appdata/local/openai/codex/runtimes/cua_node/",
            "node_repl.exe",
        ),
        (
            "/library/application support/codex/runtimes/cua_node/",
            "node_repl",
        ),
        (
            "/library/application support/com.openai.codex/runtimes/cua_node/",
            "node_repl",
        ),
    ]
    .iter()
    .any(|(prefix, executable)| {
        normalized.rsplit_once(prefix).is_some_and(|(_, suffix)| {
            let parts: Vec<_> = suffix.split('/').collect();
            parts.len() == 3 && !parts[0].is_empty() && parts[1] == "bin" && parts[2] == *executable
        })
    })
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Store {
    records: Vec<Server>,
}

#[derive(Clone)]
struct Source {
    target: ClientTarget,
    path: PathBuf,
    format: Format,
    scope: Vec<String>,
}

pub struct Repository {
    store_path: PathBuf,
    sources: Vec<Source>,
    pub records: Vec<Server>,
    pub warnings: Vec<String>,
}

impl Repository {
    pub fn discover() -> Result<Self> {
        let desktop = ClientPaths::discover(None, None)?;
        let cli = ClientPaths::launcher_codex_cli_profile()?;
        let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .map(|p| p.join(".claude.json"))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| desktop.claude_state.clone());
        let mut sources = vec![
            Source {
                target: ClientTarget::CodexCli,
                path: cli.codex_config,
                format: Format::Codex,
                scope: vec![],
            },
            Source {
                target: ClientTarget::CodexDesktop,
                path: desktop.codex_config,
                format: Format::Codex,
                scope: vec![],
            },
            Source {
                target: ClientTarget::ClaudeCode,
                path: claude.clone(),
                format: Format::Json,
                scope: vec![],
            },
        ];
        if let Some(home) = std::env::var_os("CODEX_HOME") {
            let path = PathBuf::from(home).join("config.toml");
            if path.is_file() && !sources.iter().any(|s| s.path == path) {
                sources.push(Source {
                    target: ClientTarget::CodexDesktop,
                    path,
                    format: Format::Codex,
                    scope: vec![],
                });
            }
        }
        #[cfg(windows)]
        let claude_desktop =
            PathBuf::from(std::env::var_os("APPDATA").context("APPDATA unavailable")?)
                .join("Claude/claude_desktop_config.json");
        #[cfg(not(windows))]
        let claude_desktop = desktop
            .user_home
            .join("Library/Application Support/Claude/claude_desktop_config.json");
        sources.push(Source {
            target: ClientTarget::ClaudeDesktop,
            path: claude_desktop,
            format: Format::Json,
            scope: vec![],
        });
        #[cfg(windows)]
        let third_party =
            PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA unavailable")?)
                .join("Claude-3p/claude_desktop_config.json");
        #[cfg(not(windows))]
        let third_party = desktop
            .user_home
            .join("Library/Application Support/Claude-3p/claude_desktop_config.json");
        sources.push(Source {
            target: ClientTarget::ClaudeDesktop,
            path: third_party,
            format: Format::Json,
            scope: vec![],
        });
        #[cfg(windows)]
        if let Some(base) = std::env::var_os("LOCALAPPDATA") {
            for entry in fs::read_dir(PathBuf::from(base).join("Packages"))
                .into_iter()
                .flatten()
                .flatten()
            {
                if crate::platform::is_claude_store_package_family(
                    &entry.file_name().to_string_lossy(),
                ) {
                    let path = entry
                        .path()
                        .join("LocalCache/Roaming/Claude/claude_desktop_config.json");
                    if path.is_file() {
                        sources.push(Source {
                            target: ClientTarget::ClaudeDesktop,
                            path,
                            format: Format::Json,
                            scope: vec![],
                        });
                    }
                }
            }
        }
        // Claude Code indexes private project-scoped entries in the user file.
        if let Ok(bytes) = fs::read(&claude)
            && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
            && let Some(projects) = value.get("projects").and_then(Value::as_object)
        {
            for (directory, project) in projects {
                if project.get("mcpServers").is_some() {
                    sources.push(Source {
                        target: ClientTarget::ClaudeCode,
                        path: claude.clone(),
                        format: Format::Json,
                        scope: vec!["projects".into(), directory.clone()],
                    });
                }
                let path = PathBuf::from(directory).join(".mcp.json");
                if path.is_file() {
                    sources.push(Source {
                        target: ClientTarget::ClaudeCode,
                        path,
                        format: Format::Json,
                        scope: vec![],
                    });
                }
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            let path = cwd.join(".mcp.json");
            if path.is_file() && !sources.iter().any(|s| s.path == path) {
                sources.push(Source {
                    target: ClientTarget::ClaudeCode,
                    path,
                    format: Format::Json,
                    scope: vec![],
                });
            }
        }
        Self::open(
            crate::platform_paths::data_root()?.join("gui/mcp-servers.dpapi"),
            sources,
        )
    }

    fn open(store_path: PathBuf, sources: Vec<Source>) -> Result<Self> {
        let store: Store = match fs::read(&store_path) {
            Ok(bytes) => {
                let plain = Zeroizing::new(gui_settings::protect_or_unprotect(&bytes, false)?);
                serde_json::from_slice(&plain)
                    .context(tr("MCP 本地记录格式无效", "Invalid local MCP records"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Store::default(),
            Err(e) => return Err(e.into()),
        };
        let mut result = Self {
            store_path,
            sources,
            records: store.records,
            warnings: vec![],
        };
        result.refresh()?;
        let plain = Zeroizing::new(serde_json::to_vec(&Store {
            records: result.records.clone(),
        })?);
        backup::atomic_write(&result.store_path, &gui_settings::protect(&plain)?)?;
        Ok(result)
    }

    fn refresh(&mut self) -> Result<()> {
        let old = self.records.clone();
        let order: BTreeMap<_, _> = old
            .iter()
            .enumerate()
            .map(|(index, record)| (record.id.clone(), index))
            .collect();
        let mut records: Vec<Server> = vec![];
        for source in &self.sources {
            let doc = match ConfigDocument::read(&source.path, &source.format) {
                Ok(doc) => doc,
                Err(error) => {
                    self.warnings
                        .push(format!("{}: {error:#}", source.path.display()));
                    for previous in &old {
                        let mut retained = previous.clone();
                        retained.bindings.retain(|b| {
                            b.path == source.path
                                && b.target == source.target
                                && b.scope == source.scope
                        });
                        if !retained.bindings.is_empty() {
                            records.push(retained);
                        }
                    }
                    continue;
                }
            };
            for (key, native) in doc.entries(&source.scope)? {
                let previous = old.iter().find(|r| {
                    r.bindings.iter().any(|b| {
                        b.path == source.path
                            && b.scope == source.scope
                            && b.key == key
                            && b.target == source.target
                    })
                });
                let config = if let Some(previous) = previous.filter(|r| {
                    r.bindings.iter().any(|b| {
                        canonical(&b.native, &b.format) == canonical(&native, &source.format)
                    })
                }) {
                    previous.config.clone()
                } else {
                    canonical(&native, &source.format)
                };
                let binding = Binding {
                    target: source.target,
                    path: source.path.clone(),
                    format: source.format.clone(),
                    scope: source.scope.clone(),
                    key: key.clone(),
                    enabled: source.format != Format::Codex
                        || native
                            .get("enabled")
                            .and_then(Value::as_bool)
                            .unwrap_or(true),
                    native,
                };
                if let Some(record) = records
                    .iter_mut()
                    .find(|r| r.name == key && r.config == config)
                {
                    record.bindings.push(binding);
                } else {
                    records.push(Server {
                        id: previous
                            .map(|r| r.id.clone())
                            .filter(|id| !records.iter().any(|r| &r.id == id))
                            .unwrap_or_else(new_id),
                        name: previous.map(|r| r.name.clone()).unwrap_or(key),
                        description: previous.map(|r| r.description.clone()).unwrap_or_default(),
                        registry_name: previous.and_then(|r| r.registry_name.clone()),
                        config,
                        bindings: vec![binding],
                    });
                }
            }
        }
        // Disabled JSON entries are retained encrypted, rather than adding an
        // unsupported enabled flag to client configuration files.
        for previous in old {
            for binding in &previous.bindings {
                if !binding.enabled
                    && !records
                        .iter()
                        .any(|r| r.bindings.iter().any(|b| same_binding(b, binding)))
                {
                    if let Some(record) = records.iter_mut().find(|r| r.id == previous.id) {
                        record.bindings.push(binding.clone());
                    } else {
                        let mut r = previous.clone();
                        r.bindings = vec![binding.clone()];
                        records.push(r);
                    }
                }
            }
        }
        // Client files may enumerate keys in a different order, and disabled
        // JSON entries are absent from them. Neither should move existing cards.
        records.sort_by_key(|record| order.get(&record.id).copied().unwrap_or(usize::MAX));
        self.records = records;
        Ok(())
    }

    pub fn save(
        &mut self,
        id: Option<&str>,
        name: &str,
        description: &str,
        config: Value,
        targets: &[ClientTarget],
        registry_name: Option<String>,
    ) -> Result<()> {
        ensure!(
            !name.trim().is_empty() && !name.contains(['\n', '\r', '\0']),
            "{}",
            tr("请填写有效的名称/ID", "Enter a valid name/ID")
        );
        ensure!(
            !targets.is_empty(),
            "{}",
            tr(
                "请选择至少一个目标客户端",
                "Select at least one target client"
            )
        );
        let config = validate_config(config)?;
        let previous = id
            .and_then(|id| self.records.iter().find(|r| r.id == id))
            .cloned();
        let mut record = Server {
            id: previous
                .as_ref()
                .map(|r| r.id.clone())
                .unwrap_or_else(new_id),
            name: name.trim().into(),
            description: description.trim().into(),
            config,
            bindings: vec![],
            registry_name: registry_name
                .or_else(|| previous.as_ref().and_then(|r| r.registry_name.clone())),
        };
        for &target in targets {
            let bindings: Vec<_> = previous
                .as_ref()
                .map(|r| {
                    r.bindings
                        .iter()
                        .filter(|b| b.target == target)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if bindings.is_empty() {
                let sources: Vec<_> = self
                    .sources
                    .iter()
                    .filter(|s| s.target == target && s.scope.is_empty())
                    .collect();
                ensure!(
                    !sources.is_empty(),
                    "MCP client configuration path unavailable"
                );
                // Claude Desktop has separate account and gateway profiles.
                // Configure both; otherwise an install disappears after switching modes.
                for source in sources
                    .into_iter()
                    .take(if target == ClientTarget::ClaudeDesktop {
                        usize::MAX
                    } else {
                        1
                    })
                {
                    record.bindings.push(Binding {
                        target,
                        path: source.path.clone(),
                        format: source.format.clone(),
                        scope: vec![],
                        key: record.name.clone(),
                        native: native_config(&record.config, target)?,
                        enabled: previous.as_ref().is_none_or(Server::enabled),
                    });
                }
            } else {
                for mut binding in bindings {
                    binding.key = record.name.clone();
                    binding.native = native_config(&record.config, target)?;
                    record.bindings.push(binding);
                }
            }
        }
        self.apply(previous.as_ref(), Some(record))
    }

    pub fn toggle(&mut self, id: &str) -> Result<()> {
        let old = self
            .records
            .iter()
            .find(|r| r.id == id)
            .context("MCP record no longer exists")?
            .clone();
        let mut next = old.clone();
        let enabled = !old.enabled();
        for b in &mut next.bindings {
            b.enabled = enabled;
        }
        self.apply(Some(&old), Some(next))
    }

    pub fn ensure_unchanged(&self, expected: &Server) -> Result<()> {
        let normalized = |record: &Server| {
            let mut record = record.clone();
            for b in &mut record.bindings {
                if b.format == Format::Codex
                    && let Some(o) = b.native.as_object_mut()
                {
                    o.remove("enabled");
                }
            }
            record.bindings.sort_by_key(|b| {
                (
                    b.target.id(),
                    b.path.clone(),
                    b.scope.clone(),
                    b.key.clone(),
                )
            });
            record
        };
        ensure!(
            self.records
                .iter()
                .find(|r| r.id == expected.id)
                .map(normalized)
                == Some(normalized(expected)),
            "{}",
            tr(
                "客户端 MCP 配置已变化，请刷新后重试",
                "Client MCP configuration changed; refresh and try again"
            )
        );
        Ok(())
    }

    pub fn delete(&mut self, id: &str) -> Result<()> {
        let old = self
            .records
            .iter()
            .find(|r| r.id == id)
            .context("MCP record no longer exists")?
            .clone();
        self.apply(Some(&old), None)
    }

    fn apply(&mut self, old: Option<&Server>, next: Option<Server>) -> Result<()> {
        ensure!(
            old.is_none_or(|record| !record.is_codex_builtin()),
            "{}",
            tr(
                "此 MCP Server 由 Codex 管理，只读显示，不能编辑、停用或删除。",
                "This MCP Server is managed by Codex and is read-only; it cannot be edited, disabled, or removed.",
            )
        );
        let mut docs: BTreeMap<PathBuf, ConfigDocument> = BTreeMap::new();
        for binding in old
            .into_iter()
            .flat_map(|r| &r.bindings)
            .chain(next.iter().flat_map(|r| &r.bindings))
        {
            if !docs.contains_key(&binding.path) {
                docs.insert(
                    binding.path.clone(),
                    ConfigDocument::read(&binding.path, &binding.format)?,
                );
            }
        }
        if let Some(old) = old {
            for binding in &old.bindings {
                let doc = docs.get_mut(&binding.path).unwrap();
                let current = doc.entries(&binding.scope)?.remove(&binding.key);
                let expected = stored_native(binding);
                ensure!(
                    current == expected,
                    "{}: {} • {}",
                    tr(
                        "客户端 MCP 配置已变化，请刷新后重试",
                        "Client MCP configuration changed; refresh and try again"
                    ),
                    binding.path.display(),
                    binding.key
                );
                doc.set(&binding.scope, &binding.key, None)?;
            }
        }
        if let Some(next) = &next {
            for binding in &next.bindings {
                let doc = docs.get_mut(&binding.path).unwrap();
                ensure!(
                    !doc.entries(&binding.scope)?.contains_key(&binding.key),
                    "{}: {} • {}",
                    tr(
                        "同名 MCP 已存在，未覆盖",
                        "An MCP with this name already exists; nothing was overwritten"
                    ),
                    binding.path.display(),
                    binding.key
                );
                doc.set(&binding.scope, &binding.key, stored_native(binding))?;
                if binding.target == ClientTarget::ClaudeCode && binding.enabled {
                    doc.enable_claude_code(&binding.scope, &binding.key);
                }
            }
        }
        let mut records = self.records.clone();
        if let Some(old) = old {
            if let Some(index) = records.iter().position(|r| r.id == old.id) {
                if let Some(next) = next {
                    records[index] = next;
                } else {
                    records.remove(index);
                }
            }
        } else if let Some(next) = next {
            records.push(next);
        }
        let encrypted = gui_settings::protect(&Zeroizing::new(serde_json::to_vec(&Store {
            records: records.clone(),
        })?))?;
        let mut changes = Vec::new();
        for (path, doc) in docs {
            changes.push(Change {
                path,
                before: doc.original.clone(),
                after: doc.bytes()?,
            });
        }
        changes.push(Change {
            path: self.store_path.clone(),
            before: optional_bytes(&self.store_path)?,
            after: encrypted,
        });
        commit_changes(&self.store_path, &changes)?;
        self.records = records;
        Ok(())
    }
}

fn new_id() -> String {
    format!("mcp-{:016x}", rand::random::<u64>())
}
fn same_binding(a: &Binding, b: &Binding) -> bool {
    a.path == b.path && a.scope == b.scope && a.key == b.key && a.target == b.target
}
fn stored_native(b: &Binding) -> Option<Value> {
    if b.format == Format::Codex {
        let mut v = b.native.clone();
        if v.get("enabled").is_some() || !b.enabled {
            v["enabled"] = Value::Bool(b.enabled);
        }
        Some(v)
    } else if b.enabled {
        Some(b.native.clone())
    } else {
        None
    }
}

fn canonical(native: &Value, format: &Format) -> Value {
    let mut v = native.clone();
    if let Some(object) = v.as_object_mut() {
        object.remove("enabled");
        if *format == Format::Codex {
            if let Some(headers) = object.remove("http_headers") {
                object.insert("headers".into(), headers);
            }
            if object.contains_key("url") {
                object.insert("type".into(), json!("http"));
            }
        }
    }
    v
}

pub fn validate_config(mut value: Value) -> Result<Value> {
    if let Some(servers) = value.get("mcpServers").and_then(Value::as_object) {
        ensure!(
            servers.len() == 1,
            "{}",
            tr(
                "每次添加一个 MCP 配置",
                "Add one MCP configuration at a time"
            )
        );
        value = servers.values().next().unwrap().clone();
    }
    let object = value.as_object().context(tr(
        "JSON 配置必须是对象",
        "JSON configuration must be an object",
    ))?;
    let command = object
        .get("command")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());
    let url = object
        .get("url")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty());
    if let Some(kind) = object.get("type") {
        ensure!(
            matches!(
                kind.as_str(),
                Some("stdio" | "http" | "streamable-http" | "sse")
            ),
            "{}",
            tr(
                "仅支持 stdio、Streamable HTTP 或 SSE 配置",
                "Only stdio, Streamable HTTP, and SSE configurations are supported"
            )
        );
        ensure!(
            (command.is_some() && kind == "stdio") || (url.is_some() && kind != "stdio"),
            "{}",
            tr(
                "type 与 command / url 不匹配",
                "type does not match command / url"
            )
        );
    }
    ensure!(
        command.is_some() ^ url.is_some(),
        "{}",
        tr(
            "配置应包含 command 或 url（两者选一）",
            "Configuration must contain either command or url"
        )
    );
    if let Some(url) = url {
        let parsed = url::Url::parse(url)?;
        ensure!(
            matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some(),
            "HTTP(S) URL required"
        );
    }
    if let Some(args) = object.get("args") {
        ensure!(
            args.as_array()
                .is_some_and(|a| a.iter().all(Value::is_string)),
            "args must be an array of strings"
        );
    }
    for name in ["env", "headers", "http_headers"] {
        if let Some(v) = object.get(name) {
            ensure!(
                v.as_object()
                    .is_some_and(|o| o.values().all(Value::is_string)),
                "{name} must contain string values"
            );
        }
    }
    ensure!(
        !has_placeholder(&value),
        "{}",
        tr(
            "请先补齐 JSON 中的 <填写…> 参数",
            "Complete the <required…> values in the JSON first"
        )
    );
    Ok(value)
}

#[derive(Debug, PartialEq, Eq)]
pub struct RequiredConfigInput {
    pub name: String,
    pub path: String,
}

/// List unresolved placeholders without exposing any configured values.
pub fn required_config_inputs(value: &Value) -> Vec<RequiredConfigInput> {
    fn visit(value: &Value, path: &str, inputs: &mut Vec<RequiredConfigInput>) {
        match value {
            Value::String(text) => {
                for prefix in ["<required:", "<填写:"] {
                    let mut remaining = text.as_str();
                    while let Some(start) = remaining.find(prefix) {
                        let rest = &remaining[start + prefix.len()..];
                        let end = rest.find('>').unwrap_or(rest.len());
                        let name = rest[..end].trim();
                        let input = RequiredConfigInput {
                            name: if name.is_empty() { path } else { name }.to_owned(),
                            path: path.to_owned(),
                        };
                        if !inputs.contains(&input) {
                            inputs.push(input);
                        }
                        if end == rest.len() {
                            break;
                        }
                        remaining = &rest[end + 1..];
                    }
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    visit(item, &format!("{path}[{index}]"), inputs);
                }
            }
            Value::Object(object) => {
                for (key, item) in object {
                    let path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    visit(item, &path, inputs);
                }
            }
            _ => {}
        }
    }
    let mut inputs = vec![];
    visit(value, "", &mut inputs);
    inputs
}

fn has_placeholder(v: &Value) -> bool {
    !required_config_inputs(v).is_empty()
}

fn native_config(config: &Value, target: ClientTarget) -> Result<Value> {
    let mut v = config.clone();
    let o = v.as_object_mut().context("MCP object required")?;
    if matches!(target, ClientTarget::CodexCli | ClientTarget::CodexDesktop) {
        ensure!(
            o.get("type").and_then(Value::as_str) != Some("sse"),
            "{}",
            tr(
                "Codex 需要 Streamable HTTP 或 stdio，不支持旧 SSE 配置",
                "Codex needs Streamable HTTP or stdio, not legacy SSE"
            )
        );
        o.remove("type");
        o.remove("enabled");
        if let Some(headers) = o.remove("headers") {
            o.insert("http_headers".into(), headers);
        }
    } else if target == ClientTarget::ClaudeDesktop && o.contains_key("url") {
        let mut args = vec![json!("-y"), json!("mcp-remote"), o["url"].clone()];
        if let Some(headers) = o.get("headers").and_then(Value::as_object) {
            for (key, value) in headers {
                args.push(json!("--header"));
                args.push(json!(format!(
                    "{key}: {}",
                    value.as_str().unwrap_or_default()
                )));
            }
        }
        v = json!({"command":"npx", "args":args});
    } else if o.contains_key("url") {
        let kind = if o.get("type").and_then(Value::as_str) == Some("sse") {
            "sse"
        } else {
            "http"
        };
        o.insert("type".into(), json!(kind));
    }
    #[cfg(windows)]
    resolve_windows_npx(&mut v);
    Ok(v)
}

/// Use node.exe directly when available. This avoids relying on a client's
/// ability to spawn npx.cmd and avoids shell interpretation of MCP arguments.
#[cfg(windows)]
fn resolve_windows_npx(config: &mut Value) {
    if !matches!(config["command"].as_str(), Some("npx" | "npx.cmd")) {
        return;
    }
    let dirs: Vec<_> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let Some(node) = dirs
        .iter()
        .map(|d| d.join("node.exe"))
        .find(|p| p.is_file())
    else {
        return;
    };
    let script = node
        .parent()
        .into_iter()
        .map(|d| d.join("node_modules/npm/bin/npx-cli.js"))
        .chain(
            dirs.iter()
                .map(|d| d.join("node_modules/npm/bin/npx-cli.js")),
        )
        .find(|p| p.is_file());
    let Some(script) = script else {
        return;
    };
    let mut args = vec![json!(script.to_string_lossy())];
    if let Some(old) = config["args"].as_array() {
        args.extend(old.iter().cloned());
    }
    config["command"] = json!(node.to_string_lossy());
    config["args"] = json!(args);
}

enum Parsed {
    Toml(DocumentMut),
    Json(Value),
}
struct ConfigDocument {
    original: Option<Vec<u8>>,
    parsed: Parsed,
}
impl ConfigDocument {
    fn enable_claude_code(&mut self, scope: &[String], key: &str) {
        let Parsed::Json(root) = &mut self.parsed else {
            return;
        };
        let remove_disabled = |value: &mut Value| {
            if let Some(array) = value
                .get_mut("disabledMcpServers")
                .and_then(Value::as_array_mut)
            {
                array.retain(|v| v.as_str() != Some(key));
            }
        };
        if scope.is_empty() {
            remove_disabled(root);
            if let Some(projects) = root.get_mut("projects").and_then(Value::as_object_mut) {
                projects.values_mut().for_each(remove_disabled);
            }
        } else {
            let mut node = root;
            for part in scope {
                let Some(child) = node.get_mut(part) else {
                    return;
                };
                node = child;
            }
            remove_disabled(node);
        }
    }
    fn read(path: &Path, format: &Format) -> Result<Self> {
        let original = optional_bytes(path)?;
        let text = std::str::from_utf8(original.as_deref().unwrap_or_default())
            .context("Configuration must be UTF-8")?
            .trim_start_matches('\u{feff}');
        let parsed = match format {
            Format::Codex => Parsed::Toml(
                text.parse()
                    .with_context(|| format!("Invalid TOML: {}", path.display()))?,
            ),
            Format::Json => {
                let value = if text.trim().is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(text)
                        .with_context(|| format!("Invalid JSON: {}", path.display()))?
                };
                ensure!(
                    value.is_object(),
                    "Configuration root must be an object: {}",
                    path.display()
                );
                Parsed::Json(value)
            }
        };
        Ok(Self { original, parsed })
    }
    fn entries(&self, scope: &[String]) -> Result<BTreeMap<String, Value>> {
        match &self.parsed {
            Parsed::Toml(doc) => {
                let value: Value = toml_edit::de::from_str(&doc.to_string())?;
                match value.get("mcp_servers") {
                    None => Ok(BTreeMap::new()),
                    Some(v) => {
                        serde_json::from_value(v.clone()).context("mcp_servers must be an object")
                    }
                }
            }
            Parsed::Json(value) => {
                let mut node = value;
                for part in scope {
                    let Some(child) = node.get(part) else {
                        return Ok(BTreeMap::new());
                    };
                    node = child;
                }
                match node.get("mcpServers") {
                    None => Ok(BTreeMap::new()),
                    Some(v) => {
                        serde_json::from_value(v.clone()).context("mcpServers must be an object")
                    }
                }
            }
        }
    }
    fn set(&mut self, scope: &[String], key: &str, value: Option<Value>) -> Result<()> {
        match &mut self.parsed {
            Parsed::Toml(doc) => {
                if doc.get("mcp_servers").is_none() {
                    doc["mcp_servers"] = Item::Table(Table::new());
                }
                let table = doc["mcp_servers"]
                    .as_table_like_mut()
                    .context("mcp_servers must be a table")?;
                if let Some(value) = value {
                    table.insert(
                        key,
                        Item::Table(json_table(
                            value.as_object().context("MCP object required")?,
                        )?),
                    );
                } else {
                    table.remove(key);
                }
            }
            Parsed::Json(root) => {
                let mut node = root;
                for part in scope {
                    node = node
                        .as_object_mut()
                        .context("Invalid MCP scope")?
                        .entry(part.clone())
                        .or_insert_with(|| json!({}));
                }
                let servers = node
                    .as_object_mut()
                    .context("Invalid MCP scope")?
                    .entry("mcpServers")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .context("mcpServers must be an object")?;
                if let Some(value) = value {
                    servers.insert(key.into(), value);
                } else {
                    servers.remove(key);
                }
            }
        }
        Ok(())
    }
    fn bytes(&self) -> Result<Vec<u8>> {
        Ok(match &self.parsed {
            Parsed::Toml(doc) => doc.to_string().into_bytes(),
            Parsed::Json(v) => {
                let mut b = serde_json::to_vec_pretty(v)?;
                b.push(b'\n');
                b
            }
        })
    }
}

fn json_table(object: &Map<String, Value>) -> Result<Table> {
    let mut table = Table::new();
    for (key, value) in object {
        if let Some(o) = value.as_object() {
            table.insert(key, Item::Table(json_table(o)?));
        } else {
            let doc = toml_edit::ser::to_document(&BTreeMap::from([("value", value)]))?;
            table.insert(key, doc["value"].clone());
        }
    }
    Ok(table)
}
fn optional_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Cannot read {}", path.display())),
    }
}

struct Change {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Vec<u8>,
}
fn commit_changes(store: &Path, changes: &[Change]) -> Result<()> {
    for c in changes {
        ensure!(
            optional_bytes(&c.path)? == c.before,
            "{}: {}",
            tr(
                "配置已被其他程序修改，请刷新后重试",
                "Configuration changed in another application; refresh and try again"
            ),
            c.path.display()
        );
    }
    let snapshot: BTreeMap<_, _> = changes
        .iter()
        .map(|c| (c.path.to_string_lossy().into_owned(), c.before.clone()))
        .collect();
    let encrypted = gui_settings::protect(&Zeroizing::new(serde_json::to_vec(&snapshot)?))?;
    backup::atomic_write(
        &store.with_file_name(format!("mcp-backup-{:016x}.dpapi", rand::random::<u64>())),
        &encrypted,
    )?;
    for (index, c) in changes.iter().enumerate() {
        let result = (|| {
            ensure!(
                optional_bytes(&c.path)? == c.before,
                "Configuration changed: {}",
                c.path.display()
            );
            backup::atomic_write(&c.path, &c.after)
        })();
        if let Err(error) = result {
            let mut failures = vec![];
            for done in changes[..index].iter().rev() {
                let rollback = if let Some(bytes) = &done.before {
                    backup::atomic_write(&done.path, bytes)
                } else {
                    backup::remove_file_if_exists(&done.path).map(|_| ())
                };
                if let Err(e) = rollback {
                    failures.push(format!("{}: {e}", done.path.display()));
                }
            }
            if !failures.is_empty() {
                bail!(
                    "{error:#}\n{}: {}",
                    tr(
                        "回滚失败，已保留加密备份",
                        "Rollback failed; encrypted backup retained"
                    ),
                    failures.join("; ")
                );
            }
            return Err(error.context(tr(
                "MCP 保存失败，已回滚",
                "MCP save failed; changes rolled back",
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("mungbean-mcp-{:016x}", rand::random::<u64>()));
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }
        fn repo(&self) -> Repository {
            Repository::open(
                self.root.join("store.dpapi"),
                ClientTarget::ALL
                    .into_iter()
                    .map(|target| Source {
                        target,
                        path: self.root.join(format!(
                            "{}.{}",
                            target.id(),
                            if matches!(target, ClientTarget::CodexCli | ClientTarget::CodexDesktop)
                            {
                                "toml"
                            } else {
                                "json"
                            }
                        )),
                        format: if matches!(
                            target,
                            ClientTarget::CodexCli | ClientTarget::CodexDesktop
                        ) {
                            Format::Codex
                        } else {
                            Format::Json
                        },
                        scope: vec![],
                    })
                    .collect(),
            )
            .unwrap()
        }
        fn path(&self, target: ClientTarget) -> PathBuf {
            self.repo()
                .sources
                .into_iter()
                .find(|s| s.target == target)
                .unwrap()
                .path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn codex_node_repl_detection_requires_the_bundled_runtime_path() {
        for command in [
            r"C:\Users\test\AppData\Local\OpenAI\Codex\runtimes\cua_node\runtime-v1\bin\node_repl.exe",
            "C:/Users/test/AppData/Local/OpenAI/Codex/runtimes/cua_node/runtime-v2/bin/node_repl.exe",
            "/Users/test/Library/Application Support/Codex/runtimes/cua_node/runtime-v1/bin/node_repl",
            "/Users/test/Library/Application Support/com.openai.codex/runtimes/cua_node/runtime-v1/bin/node_repl",
        ] {
            assert!(is_codex_node_repl_command(command), "{command}");
        }
        for command in [
            "node_repl.exe",
            r"C:\Tools\node_repl.exe",
            r"C:\Users\test\AppData\Local\OpenAI\Codex\runtimes\cua_node\runtime\bin\other.exe",
            r"C:\Users\test\AppData\Local\OpenAI\Codex\runtimes\cua_node\runtime\..\custom\bin\node_repl.exe",
            "relative/AppData/Local/OpenAI/Codex/runtimes/cua_node/runtime/bin/node_repl.exe",
        ] {
            assert!(!is_codex_node_repl_command(command), "{command}");
        }
    }

    #[test]
    fn codex_builtin_mcp_rejects_mutations_without_changing_any_files() {
        let fixture = Fixture::new();
        let mut repo = fixture.repo();
        let path = fixture.path(ClientTarget::CodexDesktop);
        let original = r"# Codex-managed runtime
[mcp_servers.node_repl]
command = 'C:\Users\test\AppData\Local\OpenAI\Codex\runtimes\cua_node\runtime-v1\bin\node_repl.exe'
args = []
startup_timeout_sec = 120
[mcp_servers.node_repl.env]
EXAMPLE = 'keep this value'
[mcp_servers.other]
command = 'user-server'
";
        fs::write(&path, original).unwrap();
        repo.refresh().unwrap();
        let server = repo
            .records
            .iter()
            .find(|r| r.name == "node_repl")
            .unwrap()
            .clone();
        assert!(server.is_codex_builtin());
        assert!(server.display_description().contains("Codex"));
        let records = repo.records.clone();
        let store = fs::read(&repo.store_path).unwrap();
        assert!(repo.toggle(&server.id).is_err());
        assert!(repo.delete(&server.id).is_err());
        assert!(
            repo.save(
                Some(&server.id),
                "renamed",
                "changed",
                json!({"command":"other"}),
                &[ClientTarget::ClaudeCode],
                None
            )
            .is_err()
        );
        assert_eq!(repo.records, records);
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(fs::read(&repo.store_path).unwrap(), store);

        let mut custom = server.clone();
        custom.config = json!({"command": r"C:\Tools\node_repl.exe"});
        assert!(!custom.is_codex_builtin());
        custom = server.clone();
        custom.name = "custom-service".into();
        assert!(!custom.is_codex_builtin());
        custom = server.clone();
        custom.bindings[0].target = ClientTarget::ClaudeCode;
        assert!(!custom.is_codex_builtin());
        custom = server.clone();
        custom.config = json!({"command": "/Users/test/Library/Application Support/com.openai.codex/runtimes/cua_node/runtime-v1/bin/node_repl"});
        assert!(custom.is_codex_builtin());

        repo.save(
            None,
            "node_repl",
            "User-owned server",
            json!({"command":"custom-repl"}),
            &[ClientTarget::ClaudeCode],
            None,
        )
        .unwrap();
        let custom_id = repo
            .records
            .iter()
            .find(|r| r.name == "node_repl" && !r.is_codex_builtin())
            .unwrap()
            .id
            .clone();
        repo.toggle(&custom_id).unwrap();
        repo.save(
            Some(&custom_id),
            "user-node-repl",
            "User-owned server",
            json!({"command":"custom-repl"}),
            &[ClientTarget::ClaudeCode],
            None,
        )
        .unwrap();
        repo.delete(&custom_id).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn four_client_save_preserves_unrelated_configuration_and_roundtrips_disable() {
        let fixture = Fixture::new();
        let mut repo = fixture.repo();
        for source in &repo.sources {
            fs::write(
                &source.path,
                if source.format == Format::Codex {
                    "# keep me\nmodel = \"original\"\n[mcp_servers.other]\ncommand = \"keep\"\n"
                } else {
                    "{\"theme\":\"dark\",\"mcpServers\":{\"other\":{\"command\":\"keep\"}}}"
                },
            )
            .unwrap();
        }
        repo.refresh().unwrap();
        repo.save(
            None,
            "demo",
            "my description",
            json!({"command":"node","args":["server.js"],"env":{"MY_KEY":"secret"}}),
            &ClientTarget::ALL,
            None,
        )
        .unwrap();
        let record = repo
            .records
            .iter()
            .find(|r| r.name == "demo")
            .unwrap()
            .clone();
        assert_eq!(record.targets().len(), 4);
        for source in &repo.sources {
            let doc = ConfigDocument::read(&source.path, &source.format).unwrap();
            let values = doc.entries(&[]).unwrap();
            assert!(values.contains_key("demo"));
            assert_eq!(values["other"]["command"], "keep");
            let text = fs::read_to_string(&source.path).unwrap();
            if source.format == Format::Codex {
                assert!(text.contains("# keep me"));
                assert!(text.contains("original"));
            } else {
                assert!(text.contains("dark"));
            }
        }
        repo.toggle(&record.id).unwrap();
        for source in &repo.sources {
            let entries = ConfigDocument::read(&source.path, &source.format)
                .unwrap()
                .entries(&[])
                .unwrap();
            if source.format == Format::Codex {
                assert_eq!(entries["demo"]["enabled"], false);
            } else {
                assert!(!entries.contains_key("demo"));
            }
        }
        let mut reloaded = fixture.repo();
        let record = reloaded.records.iter().find(|r| r.id == record.id).unwrap();
        assert!(!record.enabled());
        assert_eq!(record.targets().len(), 4);
        let id = record.id.clone();
        reloaded.toggle(&id).unwrap();
        let bytes = fs::read(fixture.root.join("store.dpapi")).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("MY_KEY"));
        reloaded.delete(&id).unwrap();
        for source in &reloaded.sources {
            let entries = ConfigDocument::read(&source.path, &source.format)
                .unwrap()
                .entries(&[])
                .unwrap();
            assert!(!entries.contains_key("demo"));
            assert!(entries.contains_key("other"));
        }
    }
    #[test]
    fn toggling_each_card_keeps_order_and_other_servers_unchanged_after_reload() {
        let fixture = Fixture::new();
        let mut repo = fixture.repo();
        for name in ["Zebra", "Alpha", "Middle"] {
            repo.save(
                None,
                name,
                "",
                json!({"command":"node", "args":[format!("{name}.js")]}),
                &ClientTarget::ALL,
                None,
            )
            .unwrap();
        }
        let original = repo.records.clone();
        let original_ids: Vec<_> = original.iter().map(|s| s.id.clone()).collect();
        for index in [0, 2, 1] {
            for expected_enabled in [false, true] {
                let before = repo.records.clone();
                repo.toggle(&original_ids[index]).unwrap();
                for records in [&repo.records, &fixture.repo().records] {
                    assert_eq!(
                        records.iter().map(|s| &s.id).collect::<Vec<_>>(),
                        original_ids.iter().collect::<Vec<_>>()
                    );
                    for (i, record) in records.iter().enumerate() {
                        assert_eq!(
                            record.enabled(),
                            if i == index {
                                expected_enabled
                            } else {
                                before[i].enabled()
                            }
                        );
                        assert_eq!(record.config, original[i].config);
                        assert_eq!(record.targets(), ClientTarget::ALL);
                    }
                }
                repo = fixture.repo();
            }
        }
    }

    #[test]
    fn edit_renames_stably_and_moves_targets_without_touching_other_mcp() {
        let f = Fixture::new();
        let mut repo = f.repo();
        repo.save(
            None,
            "first",
            "",
            json!({"command":"node"}),
            &[ClientTarget::CodexDesktop, ClientTarget::ClaudeCode],
            None,
        )
        .unwrap();
        let id = repo.records[0].id.clone();
        repo.save(
            Some(&id),
            "renamed",
            "updated",
            json!({"url":"https://example.com/mcp","headers":{"X-Key":"secret"}}),
            &[ClientTarget::CodexDesktop, ClientTarget::ClaudeDesktop],
            Some("registry/demo".into()),
        )
        .unwrap();
        assert_eq!(repo.records[0].id, id);
        assert_eq!(repo.records[0].description, "updated");
        assert!(
            !fs::read_to_string(f.path(ClientTarget::ClaudeCode))
                .unwrap()
                .contains("first")
        );
        let desktop = ConfigDocument::read(&f.path(ClientTarget::CodexDesktop), &Format::Codex)
            .unwrap()
            .entries(&[])
            .unwrap();
        assert_eq!(desktop["renamed"]["http_headers"]["X-Key"], "secret");
        let claude = ConfigDocument::read(&f.path(ClientTarget::ClaudeDesktop), &Format::Json)
            .unwrap()
            .entries(&[])
            .unwrap();
        assert!(
            claude["renamed"]["command"]
                .as_str()
                .is_some_and(|s| s == "npx" || s.ends_with("node.exe"))
        );
        assert!(
            claude["renamed"]["args"]
                .as_array()
                .unwrap()
                .contains(&json!("mcp-remote"))
        );
    }
    #[test]
    fn conflicts_and_invalid_target_files_do_not_modify_any_client() {
        let f = Fixture::new();
        let mut repo = f.repo();
        repo.save(
            None,
            "existing",
            "",
            json!({"command":"keep"}),
            &[ClientTarget::ClaudeCode],
            None,
        )
        .unwrap();
        let path = f.path(ClientTarget::ClaudeCode);
        let original = fs::read(&path).unwrap();
        assert!(
            repo.save(
                None,
                "existing",
                "",
                json!({"command":"overwrite"}),
                &[ClientTarget::CodexDesktop, ClientTarget::ClaudeCode],
                None
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(!f.path(ClientTarget::CodexDesktop).exists());
        let invalid = f.path(ClientTarget::CodexDesktop);
        fs::write(&invalid, "invalid [ toml").unwrap();
        assert!(
            repo.save(
                None,
                "new",
                "",
                json!({"command":"x"}),
                &[ClientTarget::ClaudeCode, ClientTarget::CodexDesktop],
                None
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    #[test]
    fn external_edits_are_not_overwritten() {
        let f = Fixture::new();
        let mut repo = f.repo();
        repo.save(
            None,
            "demo",
            "",
            json!({"command":"original"}),
            &[ClientTarget::ClaudeCode],
            None,
        )
        .unwrap();
        let id = repo.records[0].id.clone();
        let path = f.path(ClientTarget::ClaudeCode);
        fs::write(
            &path,
            r#"{"mcpServers":{"demo":{"command":"changed externally"}}}"#,
        )
        .unwrap();
        assert!(repo.delete(&id).is_err());
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("changed externally")
        );
    }
    #[test]
    fn refreshed_worker_must_match_the_ui_snapshot_before_saving() {
        let f = Fixture::new();
        let mut repo = f.repo();
        repo.save(
            None,
            "demo",
            "",
            json!({"command":"original"}),
            &[ClientTarget::ClaudeCode],
            None,
        )
        .unwrap();
        let snapshot = repo.records[0].clone();
        fs::write(
            f.path(ClientTarget::ClaudeCode),
            r#"{"mcpServers":{"demo":{"command":"changed externally"}}}"#,
        )
        .unwrap();
        let refreshed = f.repo();
        assert!(refreshed.ensure_unchanged(&snapshot).is_err());
    }

    #[test]
    fn codex_disable_refresh_and_reenable_keeps_stable_record_and_metadata() {
        let f = Fixture::new();
        let mut repo = f.repo();
        repo.save(
            None,
            "demo",
            "description",
            json!({"command":"npx", "args":["-y", "sample-mcp"]}),
            &[ClientTarget::CodexDesktop],
            Some("example/demo".into()),
        )
        .unwrap();
        let id = repo.records[0].id.clone();
        repo.toggle(&id).unwrap();
        let snapshot = repo.records[0].clone();
        let mut refreshed = f.repo();
        refreshed.ensure_unchanged(&snapshot).unwrap();
        refreshed.toggle(&id).unwrap();
        let again = f.repo();
        again.ensure_unchanged(&refreshed.records[0]).unwrap();
        assert_eq!(again.records[0].description, "description");
    }

    #[test]
    fn claude_desktop_writes_account_and_gateway_profiles_and_preserves_deployment_mode() {
        let f = Fixture::new();
        let paths = [
            f.root.join("Claude/config.json"),
            f.root.join("Claude-3p/config.json"),
        ];
        let sources = paths
            .iter()
            .map(|path| Source {
                target: ClientTarget::ClaudeDesktop,
                path: path.clone(),
                format: Format::Json,
                scope: vec![],
            })
            .collect();
        for path in &paths {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                path,
                r#"{"deploymentMode":"3p","mcpServers":{"keep":{"command":"keep"}}}"#,
            )
            .unwrap();
        }
        let mut repo = Repository::open(f.root.join("store.dpapi"), sources).unwrap();
        repo.save(
            None,
            "demo",
            "",
            json!({"command":"node"}),
            &[ClientTarget::ClaudeDesktop],
            None,
        )
        .unwrap();
        assert_eq!(
            repo.records
                .iter()
                .find(|r| r.name == "demo")
                .unwrap()
                .bindings
                .len(),
            2
        );
        for path in paths {
            let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            assert_eq!(value["deploymentMode"], "3p");
            assert_eq!(value["mcpServers"]["keep"]["command"], "keep");
            assert_eq!(value["mcpServers"]["demo"]["command"], "node");
        }
    }

    #[test]
    fn project_scope_and_same_name_different_configuration_are_preserved() {
        let f = Fixture::new();
        let path = f.root.join("claude.json");
        fs::write(&path,r#"{"auth":"keep","mcpServers":{"demo":{"command":"user"}},"projects":{"C:/project":{"trusted":true,"mcpServers":{"demo":{"command":"project"}}}}}"#).unwrap();
        let sources = vec![
            Source {
                target: ClientTarget::ClaudeCode,
                path: path.clone(),
                format: Format::Json,
                scope: vec![],
            },
            Source {
                target: ClientTarget::ClaudeCode,
                path: path.clone(),
                format: Format::Json,
                scope: vec!["projects".into(), "C:/project".into()],
            },
        ];
        let mut repo = Repository::open(f.root.join("store.dpapi"), sources).unwrap();
        assert_eq!(repo.records.len(), 2);
        let id = repo
            .records
            .iter()
            .find(|r| r.config["command"] == "project")
            .unwrap()
            .id
            .clone();
        repo.toggle(&id).unwrap();
        let v: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["auth"], "keep");
        assert_eq!(v["projects"]["C:/project"]["trusted"], true);
        assert_eq!(v["mcpServers"]["demo"]["command"], "user");
        assert!(
            v["projects"]["C:/project"]["mcpServers"]
                .as_object()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn failed_multi_file_write_rolls_back_earlier_writes() {
        let f = Fixture::new();
        let first = f.root.join("first.json");
        fs::write(&first, b"original").unwrap();
        let parent = f.root.join("not-a-directory");
        fs::write(&parent, b"file").unwrap();
        let changes = vec![
            Change {
                path: first.clone(),
                before: Some(b"original".to_vec()),
                after: b"updated".to_vec(),
            },
            Change {
                path: parent.join("second.json"),
                before: None,
                after: b"new".to_vec(),
            },
        ];
        assert!(commit_changes(&f.root.join("store.dpapi"), &changes).is_err());
        assert_eq!(fs::read(first).unwrap(), b"original");
    }
    #[test]
    fn required_inputs_report_names_and_paths_without_configured_secrets() {
        let config = json!({
            "command": "npx",
            "args": ["-y", "example", "<填写:directory>"],
            "env": {"AI_2KW_API_KEY": "<required:AI_2KW_API_KEY>", "READY": "configured-secret"},
            "headers": {"Authorization": "Bearer <required:TOKEN> <required:TOKEN>"},
            "url": "https://example.com/<required:tenant>/<required:region>"
        });
        let inputs = required_config_inputs(&config);
        assert_eq!(inputs.len(), 5);
        assert!(inputs.contains(&RequiredConfigInput {
            name: "AI_2KW_API_KEY".into(),
            path: "env.AI_2KW_API_KEY".into(),
        }));
        assert!(inputs.contains(&RequiredConfigInput {
            name: "directory".into(),
            path: "args[2]".into(),
        }));
        assert!(!format!("{inputs:?}").contains("configured-secret"));
        assert!(required_config_inputs(&json!({"env":{"TOKEN":"filled"}})).is_empty());
        assert!(has_placeholder(&json!("<required:unfinished")));
    }

    #[test]
    fn configuration_validation_rejects_unfilled_parameters_and_invalid_transports() {
        assert!(validate_config(json!({"command":"node","url":"https://example.com"})).is_err());
        assert!(
            validate_config(json!({"command":"node","env":{"TOKEN":"<required:TOKEN>"}})).is_err()
        );
        assert!(validate_config(json!({"url":"file:///etc/passwd"})).is_err());
        assert!(validate_config(json!({"command":"node","args":"wrong"})).is_err());
        assert_eq!(
            validate_config(json!({"mcpServers":{"demo":{"command":"node"}}})).unwrap()["command"],
            "node"
        );
    }
}
