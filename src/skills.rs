//! Agent Skills are complete directories, never executable installers.
use crate::gui_worker::ClientTarget;
use crate::{backup, config::ClientPaths, i18n};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
};

mod import_dialog;
pub use import_dialog::pick_import;

const MAX_FILES: usize = 512;
const MAX_BYTES: usize = 20 * 1024 * 1024;
const MAX_DOCUMENT: usize = 512 * 1024;

#[derive(Clone, Debug)]
pub struct Bundle {
    pub files: BTreeMap<String, Vec<u8>>,
    pub metadata: Metadata,
    executables: BTreeSet<String>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct Metadata {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub compatibility: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Origin {
    pub owner: String,
    pub slug: String,
    pub version: String,
}
impl Origin {
    pub fn key(&self) -> String {
        format!("{}/{}", self.owner, self.slug)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Location {
    pub path: PathBuf,
    pub targets: Vec<ClientTarget>,
    pub fingerprint: String,
    pub disabled: bool,
    #[serde(default)]
    pub archived: Option<PathBuf>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub name: String,
    pub description: String,
    pub locations: Vec<Location>,
    pub origin: Option<Origin>,
    #[serde(default)]
    pub read_only: bool,
}
impl Record {
    pub fn targets(&self) -> Vec<ClientTarget> {
        ClientTarget::ALL
            .into_iter()
            .filter(|t| self.locations.iter().any(|l| l.targets.contains(t)))
            .collect()
    }
    pub fn enabled(&self) -> bool {
        self.locations.iter().any(|l| !l.disabled)
    }
    pub fn bundle(&self) -> Result<Bundle> {
        let l = self.locations.first().context("Skill has no location")?;
        let source = l.archived.as_deref().unwrap_or(&l.path);
        let bundle = Bundle::from_directory(&if self.read_only {
            fs::canonicalize(source)?
        } else {
            source.to_path_buf()
        })?;
        ensure!(
            bundle.fingerprint() == l.fingerprint,
            "{}",
            i18n::tr(
                "Skill 文件已在外部修改，请刷新后重试",
                "Skill files changed externally; refresh and retry"
            )
        );
        Ok(bundle)
    }
}
#[derive(Clone)]
struct Root {
    path: PathBuf,
    targets: Vec<ClientTarget>,
    install: bool,
}
pub struct Repository {
    roots: Vec<Root>,
    store: PathBuf,
    export_dir: PathBuf,
}
#[derive(Default, Serialize, Deserialize)]
struct Index {
    records: Vec<Record>,
}

impl Bundle {
    pub fn from_files(files: BTreeMap<String, Vec<u8>>) -> Result<Self> {
        ensure!(
            !files.is_empty() && files.len() <= MAX_FILES,
            "Skill must contain 1–512 files"
        );
        let mut total = 0usize;
        let mut case_names = BTreeSet::new();
        for (name, bytes) in &files {
            safe_relative(name)?;
            ensure!(
                case_names.insert(name.to_lowercase()),
                "Duplicate Skill path: {name}"
            );
            total = total
                .checked_add(bytes.len())
                .context("Skill size overflow")?;
            ensure!(total <= MAX_BYTES, "Skill exceeds 20 MiB");
        }
        let doc = files
            .get("SKILL.md")
            .context(i18n::tr("缺少 SKILL.md", "Missing SKILL.md"))?;
        ensure!(doc.len() <= MAX_DOCUMENT, "SKILL.md exceeds 512 KiB");
        let text = std::str::from_utf8(doc).context("SKILL.md must be UTF-8")?;
        let normalized = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
        let tail = normalized.strip_prefix("---\n").context(i18n::tr(
            "SKILL.md 需要 YAML frontmatter（name 和 description）",
            "SKILL.md requires YAML frontmatter (name and description)",
        ))?;
        let end = tail
            .find("\n---\n")
            .or_else(|| tail.strip_suffix("\n---").map(str::len))
            .context("Missing closing --- in SKILL.md")?;
        let metadata: Metadata =
            serde_yaml_ng::from_str(&tail[..end]).context("Invalid SKILL.md frontmatter")?;
        validate_name(&metadata.name)?;
        ensure!(
            !metadata.description.trim().is_empty() && metadata.description.chars().count() <= 1024,
            "description must contain 1–1024 characters"
        );
        Ok(Self {
            files,
            metadata,
            executables: BTreeSet::new(),
        })
    }
    pub fn from_directory(path: &Path) -> Result<Self> {
        ensure!(
            path.is_dir(),
            "{}: {}",
            i18n::tr(
                "请选择包含 SKILL.md 的目录",
                "Choose a directory containing SKILL.md"
            ),
            path.display()
        );
        ensure!(
            !metadata_is_link(&fs::symlink_metadata(path)?),
            "Linked Skill directories are read-only; import the original directory"
        );
        fn visit(
            root: &Path,
            dir: &Path,
            files: &mut BTreeMap<String, Vec<u8>>,
            size: &mut usize,
            depth: usize,
        ) -> Result<()> {
            ensure!(depth <= 20, "Skill directory is too deeply nested");
            for item in fs::read_dir(dir)? {
                let item = item?;
                let name = item.file_name();
                if name == ".git" || name == ".DS_Store" {
                    continue;
                }
                let m = fs::symlink_metadata(item.path())?;
                ensure!(
                    !metadata_is_link(&m),
                    "Linked Skill files are not supported: {}",
                    item.path().display()
                );
                if m.is_dir() {
                    visit(root, &item.path(), files, size, depth + 1)?;
                } else {
                    ensure!(m.is_file(), "Unsupported Skill file");
                    ensure!(
                        files.len() < MAX_FILES && m.len() <= MAX_BYTES as u64,
                        "Skill exceeds file or size limit"
                    );
                    let bytes = read_limited(&item.path(), MAX_BYTES.saturating_sub(*size))?;
                    *size += bytes.len();
                    let key = item
                        .path()
                        .strip_prefix(root)?
                        .to_str()
                        .context("Skill path must be UTF-8")?
                        .replace('\\', "/");
                    files.insert(key, bytes);
                }
            }
            Ok(())
        }
        let mut files = BTreeMap::new();
        visit(path, path, &mut files, &mut 0, 0)?;
        let bundle = Self::from_files(files)?;
        #[cfg(unix)]
        let bundle = {
            use std::os::unix::fs::PermissionsExt;
            let mut bundle = bundle;
            for name in bundle.files.keys() {
                if fs::metadata(path.join(name))?.permissions().mode() & 0o111 != 0 {
                    bundle.executables.insert(name.clone());
                }
            }
            bundle
        };
        Ok(bundle)
    }
    pub fn from_zip(bytes: &[u8], subdirectory: Option<&str>) -> Result<Self> {
        ensure!(bytes.len() <= MAX_BYTES, "ZIP exceeds 20 MiB");
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("Invalid ZIP")?;
        ensure!(zip.len() <= MAX_FILES * 4, "ZIP contains too many entries");
        let mut all = BTreeMap::new();
        let mut executable_names = BTreeSet::new();
        let mut size = 0;
        for i in 0..zip.len() {
            let mut file = zip.by_index(i)?;
            let name = file.name()?.trim_end_matches('/').to_owned();
            safe_relative(&name)?;
            ensure!(!file.is_symlink(), "ZIP links are not supported");
            if file.is_dir() {
                continue;
            }
            if file.unix_mode().unwrap_or(0) & 0o111 != 0 {
                executable_names.insert(name.clone());
            }
            ensure!(file.size() <= MAX_BYTES as u64, "ZIP file too large");
            let mut value = Vec::new();
            (&mut file)
                .take((MAX_BYTES - size + 1) as u64)
                .read_to_end(&mut value)?;
            size += value.len();
            ensure!(
                size <= MAX_BYTES && all.len() < MAX_FILES * 4,
                "ZIP exceeds size limit"
            );
            ensure!(all.insert(name, value).is_none(), "Duplicate ZIP path");
        }
        let prefix = if let Some(sub) = subdirectory {
            if !matches!(sub, "" | ".") {
                safe_relative(sub)?;
            }
            let first = all
                .keys()
                .next()
                .context("Empty ZIP")?
                .split('/')
                .next()
                .unwrap();
            if matches!(sub, "" | ".") {
                format!("{first}/")
            } else {
                format!("{first}/{}/", sub.trim_matches('/'))
            }
        } else if all.contains_key("SKILL.md") {
            String::new()
        } else {
            let candidates: Vec<_> = all
                .keys()
                .filter_map(|p| p.strip_suffix("/SKILL.md"))
                .collect();
            ensure!(
                candidates.len() == 1,
                "{}",
                i18n::tr(
                    "ZIP 需仅包含一个 Skill；请选择单个 Skill 目录",
                    "ZIP must contain one Skill; choose an individual Skill directory"
                )
            );
            format!("{}/", candidates[0])
        };
        let mut bundle = Self::from_files(
            all.into_iter()
                .filter_map(|(p, v)| p.strip_prefix(&prefix).map(|k| (k.to_owned(), v)))
                .collect(),
        )?;
        bundle.executables = executable_names
            .into_iter()
            .filter_map(|n| n.strip_prefix(&prefix).map(str::to_owned))
            .collect();
        Ok(bundle)
    }
    pub fn import(path: &Path) -> Result<Self> {
        if path.is_dir() {
            Self::from_directory(path)
        } else {
            Self::from_zip(&read_limited(path, MAX_BYTES)?, None)
        }
    }
    pub fn document(&self) -> String {
        String::from_utf8_lossy(&self.files["SKILL.md"]).into_owned()
    }
    pub fn with_document(&self, document: &str) -> Result<Self> {
        let mut files = self.files.clone();
        files.insert("SKILL.md".into(), document.as_bytes().to_vec());
        let mut bundle = Self::from_files(files)?;
        bundle.executables = self.executables.clone();
        Ok(bundle)
    }
    pub fn fingerprint(&self) -> String {
        let mut hash = Sha256::new();
        for (path, bytes) in &self.files {
            hash.update((path.len() as u64).to_le_bytes());
            hash.update(path);
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        format!("{:x}", hash.finalize())
    }
    fn write_to(&self, path: &Path) -> Result<()> {
        ensure!(!path.exists(), "Destination exists: {}", path.display());
        fs::create_dir_all(path)?;
        for (name, bytes) in &self.files {
            let file = path.join(safe_relative(name)?);
            fs::create_dir_all(file.parent().unwrap())?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file)?;
            output.write_all(bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    file,
                    fs::Permissions::from_mode(if self.executables.contains(name) {
                        0o700
                    } else {
                        0o600
                    }),
                )?;
            }
        }
        Ok(())
    }
    pub fn zip_bytes(&self) -> Result<Vec<u8>> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, bytes) in &self.files {
            zip.start_file(
                format!("{}/{path}", self.metadata.name),
                zip::write::SimpleFileOptions::default()
                    .unix_permissions(if self.executables.contains(path) {
                        0o755
                    } else {
                        0o644
                    })
                    .compression_method(zip::CompressionMethod::Deflated),
            )?;
            zip.write_all(bytes)?;
        }
        Ok(zip.finish()?.into_inner())
    }
}
pub fn template() -> Bundle {
    Bundle::from_files(BTreeMap::from([("SKILL.md".into(), b"---\nname: my-skill\ndescription: Describe when to use this skill.\n---\n\n# My Skill\n\nWrite instructions here.\n".to_vec())])).unwrap()
}
fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && !name.starts_with('-')
            && !name.ends_with('-')
            && !name.contains("--")
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "Skill name: use 1–64 lowercase letters, digits and single hyphens"
    );
    ensure!(
        !matches!(
            name,
            "synced" | "anthropic-skills" | "con" | "prn" | "aux" | "nul"
        ) && !(name.len() == 4
            && (name.starts_with("com") || name.starts_with("lpt"))
            && name.ends_with(|c: char| c.is_ascii_digit())),
        "Reserved Skill name"
    );
    Ok(())
}
fn safe_relative(value: &str) -> Result<PathBuf> {
    ensure!(
        !value.is_empty()
            && !value.contains(['\\', ':', '\0', '<', '>', '"', '|', '?', '*'])
            && value
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".." && !p.ends_with(['.', ' '])),
        "Unsafe Skill path: {value}"
    );
    let p = PathBuf::from(value);
    for part in value.split('/') {
        let stem = part.split('.').next().unwrap().to_ascii_lowercase();
        let device = matches!(
            stem.as_str(),
            "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
        ) || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit());
        ensure!(
            !device && part.len() <= 255,
            "Unsupported Skill path: {value}"
        );
    }
    ensure!(
        p.components().all(|c| matches!(c, Component::Normal(_))),
        "Unsafe Skill path: {value}"
    );
    Ok(p)
}
fn read_limited(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "Skill exceeds size limit");
    Ok(bytes)
}
fn nonce() -> String {
    format!("{:016x}", rand::random::<u64>())
}
fn has_link_ancestor(path: &Path) -> Result<bool> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata_is_link(&metadata) => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}
fn metadata_is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Includes directory junctions, which FileType::is_symlink can omit.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

impl Repository {
    fn lock(&self) -> Result<fs::File> {
        fs::create_dir_all(&self.store)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.store.join("write.lock"))?;
        lock.try_lock().map_err(|error| {
            anyhow::anyhow!(
                "{}: {error}",
                i18n::tr(
                    "其他 Skill 操作正在写入，请稍后重试",
                    "Another Skill operation is writing; try again shortly"
                )
            )
        })?;
        Ok(lock)
    }
    pub fn discover() -> Result<Self> {
        let desktop = ClientPaths::discover(None, None)?;
        let cli = ClientPaths::launcher_codex_cli_profile()?;
        let home = &desktop.user_home;
        let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        let mut roots = vec![
            Root {
                path: cli.codex_home.join("skills"),
                targets: vec![ClientTarget::CodexCli],
                install: true,
            },
            Root {
                path: desktop.codex_home.join("skills"),
                targets: vec![ClientTarget::CodexDesktop],
                install: true,
            },
            Root {
                path: claude.join("skills"),
                targets: vec![ClientTarget::ClaudeCode],
                install: true,
            },
            Root {
                path: home.join(".agents/skills"),
                targets: vec![ClientTarget::CodexCli, ClientTarget::CodexDesktop],
                install: false,
            },
        ];
        if let Some(home) = std::env::var_os("CODEX_HOME") {
            let path = PathBuf::from(home).join("skills");
            if !roots.iter().any(|r| r.path == path) {
                roots.push(Root {
                    path,
                    targets: vec![ClientTarget::CodexDesktop],
                    install: false,
                });
            }
        }
        Ok(Self {
            roots,
            store: crate::platform_paths::data_root()?.join("gui/skills"),
            export_dir: directories::UserDirs::new()
                .and_then(|dirs| dirs.download_dir().map(Path::to_path_buf))
                .unwrap_or_else(|| home.join("Downloads"))
                .join("Little-Mungbean-Skills"),
        })
    }
    fn index(&self) -> Result<Index> {
        let path = self.store.join("index.json");
        if !path.exists() {
            return Ok(Index::default());
        }
        serde_json::from_slice(&fs::read(path)?)
            .context("Cannot read Skill index; files have not been changed")
    }
    fn write_index(&self, index: &Index) -> Result<()> {
        backup::atomic_write(
            &self.store.join("index.json"),
            &serde_json::to_vec_pretty(index)?,
        )
    }
    pub fn list(&self) -> Result<Vec<Record>> {
        let index = self.index()?;
        let mut records: Vec<Record> = Vec::new();
        for root in &self.roots {
            if !root.path.exists() {
                continue;
            }
            for item in fs::read_dir(&root.path)? {
                let path = item?.path();
                if !path.join("SKILL.md").is_file() {
                    continue;
                }
                let read_only = has_link_ancestor(&path)?;
                let source = if read_only {
                    fs::canonicalize(&path)?
                } else {
                    path.clone()
                };
                let Ok(bundle) = Bundle::from_directory(&source) else {
                    continue;
                };
                let fingerprint = bundle.fingerprint();
                let saved = index
                    .records
                    .iter()
                    .find(|r| r.locations.iter().any(|l| l.path == path && !l.disabled));
                let mut id = saved
                    .map(|r| r.id.clone())
                    .unwrap_or_else(|| format!("{}-{}", bundle.metadata.name, &fingerprint[..12]));
                if saved.is_none()
                    && index
                        .records
                        .iter()
                        .any(|r| r.id == id && r.locations.iter().any(|l| l.disabled))
                {
                    // A client may have reinstalled a skill while our copy is
                    // disabled. Keep those two independent instead of merging
                    // an active directory with a different archived copy.
                    id.push_str("-active");
                }
                let location = Location {
                    path: path.clone(),
                    targets: root.targets.clone(),
                    fingerprint,
                    disabled: false,
                    archived: None,
                };
                if let Some(existing) = records.iter_mut().find(|r| r.id == id) {
                    existing.read_only |= read_only;
                    if let Some(l) = existing.locations.iter_mut().find(|l| l.path == path) {
                        for t in &root.targets {
                            if !l.targets.contains(t) {
                                l.targets.push(*t);
                            }
                        }
                    } else {
                        existing.locations.push(location);
                    }
                } else {
                    records.push(Record {
                        id,
                        name: bundle.metadata.name,
                        description: bundle.metadata.description,
                        locations: vec![location],
                        origin: saved.and_then(|r| r.origin.clone()),
                        read_only,
                    });
                }
            }
        }
        for saved in index.records {
            for location in saved
                .locations
                .iter()
                .filter(|l| l.disabled && l.archived.as_ref().is_some_and(|p| p.is_dir()))
            {
                if let Some(record) = records.iter_mut().find(|r| r.id == saved.id) {
                    record.locations.push(location.clone());
                } else {
                    let mut record = saved.clone();
                    record.locations = vec![location.clone()];
                    records.push(record);
                }
            }
        }
        records.sort_by_key(|r| (r.name.to_lowercase(), r.id.clone()));
        Ok(records)
    }
    fn check(&self, record: &Record) -> Result<()> {
        ensure!(
            !record.read_only,
            "{}",
            i18n::tr(
                "链接目录为只读；如需修改，请从原始目录重新导入",
                "Linked directories are read-only; import the original directory to make changes"
            )
        );
        for l in &record.locations {
            ensure!(
                !has_link_ancestor(&l.path)?,
                "Skill location changed to a link; refresh before retrying"
            );
            ensure!(
                self.roots
                    .iter()
                    .any(|r| l.path.parent() == Some(r.path.as_path())),
                "Skill path is outside configured roots"
            );
            let path = l.archived.as_deref().unwrap_or(&l.path);
            if let Some(archived) = &l.archived {
                ensure!(
                    archived.parent() == Some(self.store.join("disabled").as_path()),
                    "Invalid disabled Skill archive location"
                );
            }
            ensure!(
                Bundle::from_directory(path)?.fingerprint() == l.fingerprint,
                "{}",
                i18n::tr(
                    "Skill 文件已在外部修改，请刷新后重试",
                    "Skill files changed externally; refresh and retry"
                )
            );
        }
        Ok(())
    }
    pub fn save(
        &self,
        original: Option<&Record>,
        bundle: &Bundle,
        targets: &[ClientTarget],
        origin: Option<Origin>,
    ) -> Result<String> {
        let _guard = self.lock()?;
        ensure!(
            !targets.is_empty(),
            "{}",
            i18n::tr(
                "请选择至少一个目标客户端",
                "Select at least one target client"
            )
        );
        if let Some(record) = original {
            self.check(record)?;
        }
        let mut destinations: BTreeMap<PathBuf, Vec<ClientTarget>> = BTreeMap::new();
        if let Some(original) = original {
            for l in &original.locations {
                // Existing shared directories retain all their shared targets.
                if l.targets.iter().any(|t| targets.contains(t)) {
                    destinations.insert(
                        l.path.parent().unwrap().join(&bundle.metadata.name),
                        l.targets.clone(),
                    );
                }
            }
        }
        for target in targets
            .iter()
            .filter(|t| **t != ClientTarget::ClaudeDesktop)
        {
            if destinations.values().any(|ts| ts.contains(target)) {
                continue;
            }
            let root = self
                .roots
                .iter()
                .find(|r| r.install && r.targets.contains(target))
                .context("No Skill directory for this client")?;
            destinations
                .entry(root.path.join(&bundle.metadata.name))
                .or_default()
                .push(*target);
        }
        let id = original.map(|r| r.id.clone()).unwrap_or_else(nonce);
        let fingerprint = bundle.fingerprint();
        let mut plans = BTreeMap::new();
        if let Some(record) = original {
            for l in &record.locations {
                if !l.disabled {
                    plans.insert(l.path.clone(), None);
                }
            }
        }
        let mut locations = Vec::new();
        let keep_disabled = original.is_some_and(|r| !r.enabled());
        for (path, ts) in destinations {
            let own =
                original.is_some_and(|r| r.locations.iter().any(|l| l.path == path && !l.disabled));
            ensure!(
                !path.exists() || own,
                "{}: {}",
                i18n::tr(
                    "已存在同名 Skill，未覆盖",
                    "A Skill with this name exists; nothing was overwritten"
                ),
                path.display()
            );
            let archived = if keep_disabled {
                let archive = self.store.join("disabled").join(nonce());
                bundle.write_to(&archive)?;
                Some(archive)
            } else {
                plans.insert(path.clone(), Some(bundle.clone()));
                None
            };
            locations.push(Location {
                path,
                targets: ts,
                fingerprint: fingerprint.clone(),
                disabled: keep_disabled,
                archived,
            });
        }
        let mut exported = None;
        if targets.contains(&ClientTarget::ClaudeDesktop) {
            fs::create_dir_all(&self.export_dir)?;
            let path =
                self.export_dir
                    .join(format!("{}-{}.zip", bundle.metadata.name, &nonce()[..8]));
            backup::atomic_write(&path, &bundle.zip_bytes()?)?;
            exported = Some(path);
        }
        let mut index = self.index()?;
        index.records.retain(|r| r.id != id);
        if !locations.is_empty() {
            index.records.push(Record {
                id,
                name: bundle.metadata.name.clone(),
                description: bundle.metadata.description.clone(),
                locations,
                origin,
                read_only: false,
            });
        }
        if let Err(error) = self.commit(plans, &index) {
            if let Some(path) = &exported {
                let _ = fs::remove_file(path);
            }
            return Err(error);
        }
        Ok(if let Some(path) = exported {
            format!(
                "{} • {}: {}",
                bundle.metadata.name,
                i18n::tr(
                    "已导出 ZIP；请在 Claude Desktop 的 Customize > Skills 导入",
                    "ZIP exported; import it in Claude Desktop > Customize > Skills"
                ),
                path.display()
            )
        } else {
            format!(
                "{} • {}",
                bundle.metadata.name,
                i18n::tr(
                    "已保存 • 请在客户端重新加载",
                    "Saved • Reload in your client"
                )
            )
        })
    }
    pub fn toggle(&self, record: &Record) -> Result<String> {
        let _guard = self.lock()?;
        self.check(record)?;
        let disable = record.enabled();
        let mut updated = record.clone();
        let mut plans = BTreeMap::new();
        for l in &mut updated.locations {
            if disable && !l.disabled {
                let archive = self.store.join("disabled").join(nonce());
                let b = Bundle::from_directory(&l.path)?;
                b.write_to(&archive)?;
                plans.insert(l.path.clone(), None);
                l.disabled = true;
                l.archived = Some(archive);
            } else if !disable && l.disabled {
                ensure!(
                    !l.path.exists(),
                    "Skill destination already exists: {}",
                    l.path.display()
                );
                plans.insert(
                    l.path.clone(),
                    Some(Bundle::from_directory(l.archived.as_ref().unwrap())?),
                );
                l.disabled = false;
                l.archived = None;
            }
        }
        let mut index = self.index()?;
        index.records.retain(|r| r.id != record.id);
        index.records.push(updated);
        self.commit(plans, &index)?;
        Ok(format!(
            "{} • {} • {}",
            record.name,
            if disable {
                i18n::tr("已停用", "Disabled")
            } else {
                i18n::tr("已启用", "Enabled")
            },
            i18n::tr("请在客户端重新加载", "Reload in your client")
        ))
    }
    pub fn delete(&self, record: &Record) -> Result<String> {
        let _guard = self.lock()?;
        self.check(record)?;
        let plans = record
            .locations
            .iter()
            .filter(|l| !l.disabled)
            .map(|l| (l.path.clone(), None))
            .collect();
        let mut index = self.index()?;
        index.records.retain(|r| r.id != record.id);
        self.commit(plans, &index)?;
        Ok(format!(
            "{} • {}",
            record.name,
            i18n::tr(
                "已删除 • 已保留文件备份 • 请在客户端重新加载",
                "Removed • File backup retained • Reload in your client"
            )
        ))
    }
    fn commit(&self, plans: BTreeMap<PathBuf, Option<Bundle>>, index: &Index) -> Result<()> {
        let mut changes: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
        let result = (|| -> Result<()> {
            for (path, bundle) in plans {
                let root = path.parent().context("Missing Skill root")?;
                ensure!(
                    path.is_absolute() && self.roots.iter().any(|r| r.path == root),
                    "Skill write is outside configured roots"
                );
                ensure!(!has_link_ancestor(root)?, "Linked Skill root is read-only");
                fs::create_dir_all(root)?;
                let history = root
                    .parent()
                    .context("Missing Skill parent")?
                    .join(".little-mungbean-skill-backups");
                fs::create_dir_all(&history)?;
                let archived = if path.exists() {
                    let archived = history.join(format!(
                        "{}-{}",
                        path.file_name().unwrap().to_string_lossy(),
                        nonce()
                    ));
                    fs::rename(&path, &archived)?;
                    Some(archived)
                } else {
                    None
                };
                changes.push((path.clone(), archived));
                if let Some(bundle) = bundle {
                    let stage = history.join(format!("stage-{}", nonce()));
                    let outcome = bundle
                        .write_to(&stage)
                        .and_then(|()| fs::rename(&stage, &path).map_err(Into::into));
                    if outcome.is_err() {
                        let _ = fs::remove_dir_all(&stage);
                    }
                    outcome?;
                }
            }
            self.write_index(index)
        })();
        if let Err(error) = result {
            for (path, archived) in changes.into_iter().rev() {
                if path.exists() {
                    fs::remove_dir_all(&path)
                        .with_context(|| format!("Rollback failed: {}", path.display()))?;
                }
                if let Some(old) = archived {
                    fs::rename(old, &path)
                        .with_context(|| format!("Restore backup failed: {}", path.display()))?;
                }
            }
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
