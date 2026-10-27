//! Opt-in fixture export consumed by scripts/test-mcp-live.cjs.
//! All repository writes stay below the runner's fresh temporary directory.
use super::*;

#[test]
#[ignore = "run through scripts/test-mcp-live.cjs with isolated runtime dependencies"]
fn export_live_mcp_configurations() -> Result<()> {
    let root = PathBuf::from(std::env::var("MUNGBEAN_MCP_LIVE_ROOT")?);
    ensure!(
        root.is_absolute() && root.is_dir(),
        "Fresh test directory required"
    );
    ensure!(
        !root.join("manifest.json").exists(),
        "Refusing to reuse fixtures"
    );
    let endpoint = std::env::var("MUNGBEAN_MCP_LIVE_URL")?;
    ensure!(
        endpoint.starts_with("http://127.0.0.1:"),
        "Loopback test endpoint required"
    );
    let files = root.join("allowed files");
    fs::create_dir_all(&files)?;
    let marker = files.join("你好 MCP.txt");
    fs::write(&marker, "Little Mungbean real MCP tool call succeeded.\n")?;
    let sources: Vec<_> = ClientTarget::ALL
        .into_iter()
        .map(|target| Source {
            target,
            path: root.join("working").join(target.id()).join(match target {
                ClientTarget::CodexCli | ClientTarget::CodexDesktop => "config.toml",
                ClientTarget::ClaudeCode => ".claude.json",
                ClientTarget::ClaudeDesktop => "claude_desktop_config.json",
            }),
            format: if matches!(target, ClientTarget::CodexCli | ClientTarget::CodexDesktop) {
                Format::Codex
            } else {
                Format::Json
            },
            scope: vec![],
        })
        .collect();
    let mut repo = Repository::open(root.join("store.dpapi"), sources)?;
    let filesystem = json!({
        "command": "npx",
        "args": ["-y", "@modelcontextprotocol/server-filesystem@1.0.0", files],
    });
    repo.save(
        None,
        "filesystem",
        "Official filesystem test",
        filesystem.clone(),
        &ClientTarget::ALL,
        None,
    )?;
    repo.save(
        None,
        "probe-http",
        "Authenticated loopback test",
        json!({
            "type": "http", "url": endpoint,
            "headers": {"X-Mungbean-Probe": "local-test-header"}
        }),
        &ClientTarget::ALL,
        None,
    )?;
    repo.save(
        None,
        "probe-sse",
        "Legacy SSE test",
        json!({
            "type": "sse", "url": endpoint.replace("/mcp", "/sse"),
            "headers": {"X-Mungbean-Probe": "local-test-header"}
        }),
        &[ClientTarget::ClaudeCode, ClientTarget::ClaudeDesktop],
        None,
    )?;
    let ids: Vec<_> = repo.records.iter().map(|r| r.id.clone()).collect();
    let mut phases = Map::new();
    let mut snapshot = |phase: &str, repo: &Repository| -> Result<()> {
        let mut clients = vec![];
        for source in &repo.sources {
            let destination = root
                .join(phase)
                .join(source.target.id())
                .join(source.path.file_name().unwrap());
            fs::create_dir_all(destination.parent().unwrap())?;
            fs::copy(&source.path, &destination)?;
            let servers = ConfigDocument::read(&source.path, &source.format)?.entries(&[])?;
            clients
                .push(json!({"target":source.target.id(), "path":destination, "servers":servers}));
        }
        phases.insert(phase.into(), json!(clients));
        Ok(())
    };
    snapshot("enabled", &repo)?;
    for id in &ids {
        repo.toggle(id)?;
    }
    snapshot("disabled", &repo)?;
    repo = Repository::open(repo.store_path.clone(), repo.sources.clone())?;
    for id in &ids {
        repo.toggle(id)?;
    }
    snapshot("reenabled", &repo)?;
    repo.save(
        Some(&ids[0]),
        "filesystem-renamed",
        "Renamed live test",
        filesystem,
        &ClientTarget::ALL,
        None,
    )?;
    snapshot("renamed", &repo)?;
    for id in &ids {
        repo.delete(id)?;
    }
    snapshot("deleted", &repo)?;
    fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "marker": marker, "phases": phases,
        }))?,
    )?;
    Ok(())
}
