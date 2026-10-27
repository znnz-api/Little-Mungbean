use super::*;

struct Fixture {
    root: PathBuf,
    repository: Repository,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("mungbean-skills-test-{}", nonce()));
        fs::create_dir_all(&root).unwrap();
        // macOS temp paths may pass through /var -> /private/var. Test roots
        // must use their real path so the linked-root guard stays meaningful.
        let root = fs::canonicalize(root).unwrap();
        let repository = Repository {
            store: root.join("store"),
            export_dir: root.join("Downloads/Little-Mungbean-Skills"),
            roots: vec![
                Root {
                    path: root.join("codex/skills"),
                    targets: vec![ClientTarget::CodexDesktop],
                    install: true,
                },
                Root {
                    path: root.join("cli/skills"),
                    targets: vec![ClientTarget::CodexCli],
                    install: true,
                },
                Root {
                    path: root.join("claude/skills"),
                    targets: vec![ClientTarget::ClaudeCode],
                    install: true,
                },
                Root {
                    path: root.join("agents/skills"),
                    targets: vec![ClientTarget::CodexCli, ClientTarget::CodexDesktop],
                    install: false,
                },
            ],
        };
        Self { root, repository }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn complete_bundle() -> Bundle {
    let mut files = template().files;
    files.insert(
        "scripts/helper.py".into(),
        b"print('preserved, not executed')\n".to_vec(),
    );
    files.insert("references/guide.md".into(), b"reference\n".to_vec());
    files.insert("assets/sample.bin".into(), vec![0, 1, 2, 255]);
    Bundle::from_files(files).unwrap()
}
#[test]
fn zip_roundtrip_preserves_all_supporting_files() {
    let bundle = complete_bundle();
    let decoded = Bundle::from_zip(&bundle.zip_bytes().unwrap(), None).unwrap();
    assert_eq!(bundle.files, decoded.files);
    assert_eq!(bundle.fingerprint(), decoded.fingerprint());
}
#[test]
fn frontmatter_accepts_folded_yaml_and_rejects_missing_fields() {
    let doc =
        "---\nname: useful-skill\ndescription: >-\n  First line\n  second line\n---\n\nBody\n";
    let bundle = template().with_document(doc).unwrap();
    assert_eq!(bundle.metadata.description, "First line second line");
    assert!(template().with_document("# No frontmatter").is_err());
    assert!(
        template()
            .with_document("---\nname: bad/name\ndescription: useful\n---\nBody")
            .is_err()
    );
}
#[test]
fn unsafe_archive_paths_are_rejected() {
    for path in [
        "../evil",
        "/root",
        "a/../evil",
        "a\\b",
        "C:/evil",
        "a./bad",
        "a//b",
    ] {
        assert!(safe_relative(path).is_err(), "{path}");
    }
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("../escape", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(b"escape").unwrap();
    assert!(Bundle::from_zip(&zip.finish().unwrap().into_inner(), None).is_err());
}
#[test]
fn rejects_case_collisions_and_multiple_skills_in_zip() {
    let mut files = template().files;
    files.insert("skill.md".into(), b"duplicate".to_vec());
    assert!(Bundle::from_files(files).is_err());
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for name in ["a/SKILL.md", "b/SKILL.md"] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(template().document().as_bytes()).unwrap();
    }
    assert!(Bundle::from_zip(&zip.finish().unwrap().into_inner(), None).is_err());
}
#[test]
fn install_edit_toggle_restore_and_remove_are_complete() {
    let fixture = Fixture::new();
    let repo = &fixture.repository;
    let bundle = complete_bundle();
    repo.save(
        None,
        &bundle,
        &[ClientTarget::CodexDesktop, ClientTarget::ClaudeCode],
        None,
    )
    .unwrap();
    let records = repo.list().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].locations.len(), 2);
    let changed = bundle
        .with_document(
            &bundle
                .document()
                .replace("Write instructions here.", "New instructions."),
        )
        .unwrap();
    repo.save(Some(&records[0]), &changed, &records[0].targets(), None)
        .unwrap();
    let record = repo.list().unwrap().remove(0);
    assert_eq!(record.bundle().unwrap().files, changed.files);
    repo.toggle(&record).unwrap();
    let record = repo.list().unwrap().remove(0);
    assert!(!record.enabled());
    assert!(!fixture.root.join("codex/skills/my-skill").exists());
    repo.toggle(&record).unwrap();
    let record = repo.list().unwrap().remove(0);
    assert!(record.enabled());
    assert_eq!(record.bundle().unwrap().files, changed.files);
    repo.delete(&record).unwrap();
    assert!(repo.list().unwrap().is_empty());
    assert!(
        fs::read_dir(fixture.root.join("codex/.little-mungbean-skill-backups"))
            .unwrap()
            .count()
            > 0
    );
}
#[test]
fn existing_same_name_and_external_edits_are_never_overwritten() {
    let f = Fixture::new();
    let repo = &f.repository;
    let bundle = complete_bundle();
    repo.save(None, &bundle, &[ClientTarget::CodexDesktop], None)
        .unwrap();
    assert!(
        repo.save(None, &bundle, &[ClientTarget::CodexDesktop], None)
            .is_err()
    );
    let r = repo.list().unwrap().remove(0);
    let file = &r.locations[0].path.join("scripts/helper.py");
    fs::write(file, b"user changes").unwrap();
    assert!(repo.toggle(&r).is_err());
    assert!(repo.delete(&r).is_err());
    assert!(repo.save(Some(&r), &bundle, &r.targets(), None).is_err());
    assert_eq!(fs::read(file).unwrap(), b"user changes");
}
#[test]
fn shared_directory_is_grouped_and_not_duplicated() {
    let f = Fixture::new();
    let repo = &f.repository;
    let bundle = complete_bundle();
    bundle
        .write_to(&f.root.join("agents/skills/my-skill"))
        .unwrap();
    let r = repo.list().unwrap().remove(0);
    assert_eq!(r.targets().len(), 2);
    repo.save(Some(&r), &bundle, &[ClientTarget::CodexDesktop], None)
        .unwrap();
    let r = repo.list().unwrap().remove(0);
    assert_eq!(r.locations.len(), 1);
    assert_eq!(r.targets().len(), 2);
}
#[test]
fn failed_later_target_rolls_back_previous_target() {
    let f = Fixture::new();
    let repo = &f.repository;
    let bundle = complete_bundle();
    fs::write(f.root.join("claude"), b"blocked parent").unwrap();
    assert!(
        repo.save(
            None,
            &bundle,
            &[ClientTarget::CodexDesktop, ClientTarget::ClaudeCode],
            None
        )
        .is_err()
    );
    assert!(!f.root.join("codex/skills/my-skill").exists());
    assert!(!repo.store.join("index.json").exists());
}
#[test]
fn index_write_failure_restores_replaced_skill() {
    let f = Fixture::new();
    let repo = &f.repository;
    let bundle = complete_bundle();
    repo.save(None, &bundle, &[ClientTarget::CodexDesktop], None)
        .unwrap();
    let r = repo.list().unwrap().remove(0);
    fs::remove_file(repo.store.join("index.json")).unwrap();
    fs::create_dir(repo.store.join("index.json")).unwrap();
    let plans = BTreeMap::from([(r.locations[0].path.clone(), None)]);
    assert!(repo.commit(plans, &Index::default()).is_err());
    assert_eq!(r.bundle().unwrap().files, bundle.files);
}
#[test]
fn claude_desktop_export_is_not_reported_as_native_install() {
    let f = Fixture::new();
    let repo = &f.repository;
    let message = repo
        .save(
            None,
            &complete_bundle(),
            &[ClientTarget::ClaudeDesktop],
            None,
        )
        .unwrap();
    assert!(message.contains("Customize > Skills"));
    assert!(message.contains(&repo.export_dir.display().to_string()));
    assert!(repo.list().unwrap().is_empty());
    let zip = fs::read_dir(&repo.export_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(Bundle::import(&zip).unwrap().files, complete_bundle().files);
}

#[test]
fn renaming_changes_folder_and_editing_disabled_skill_keeps_it_disabled() {
    let f = Fixture::new();
    let repo = &f.repository;
    let bundle = complete_bundle();
    repo.save(None, &bundle, &[ClientTarget::CodexDesktop], None)
        .unwrap();
    let r = repo.list().unwrap().remove(0);
    repo.toggle(&r).unwrap();
    let r = repo.list().unwrap().remove(0);
    let renamed = bundle
        .with_document(
            &bundle
                .document()
                .replace("name: my-skill", "name: renamed-skill"),
        )
        .unwrap();
    repo.save(Some(&r), &renamed, &r.targets(), None).unwrap();
    let r = repo.list().unwrap().remove(0);
    assert!(!r.enabled());
    assert_eq!(r.name, "renamed-skill");
    repo.toggle(&r).unwrap();
    assert!(f.root.join("codex/skills/renamed-skill/SKILL.md").is_file());
    assert!(!f.root.join("codex/skills/my-skill").exists());
}

#[test]
fn another_writer_prevents_all_mutations() {
    let f = Fixture::new();
    let repo = &f.repository;
    let _guard = repo.lock().unwrap();
    assert!(
        repo.save(
            None,
            &complete_bundle(),
            &[ClientTarget::CodexDesktop],
            None
        )
        .is_err()
    );
    assert!(!f.root.join("codex/skills/my-skill").exists());
}

#[test]
fn repository_links_are_listed_without_modifying_their_target() {
    let f = Fixture::new();
    let bundle = complete_bundle();
    let original = f.root.join("original");
    bundle.write_to(&original).unwrap();
    let linked = f.root.join("codex/skills/linked-skill");
    fs::create_dir_all(linked.parent().unwrap()).unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let status = std::process::Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(linked.to_string_lossy().replace('/', "\\"))
            .arg(original.to_string_lossy().replace('/', "\\"))
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&original, &linked).unwrap();
    let record = f.repository.list().unwrap().remove(0);
    assert!(record.read_only);
    assert_eq!(record.bundle().unwrap().files, bundle.files);
    assert!(f.repository.toggle(&record).is_err());
    assert!(f.repository.delete(&record).is_err());
    assert!(original.join("SKILL.md").exists());
    #[cfg(windows)]
    fs::remove_dir(&linked).unwrap();
    #[cfg(unix)]
    fs::remove_file(&linked).unwrap();
}

#[test]
fn github_subdirectory_extracts_only_selected_skill() {
    let bundle = complete_bundle();
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in &bundle.files {
        zip.start_file(
            format!("repo-commit/skills/my-skill/{path}"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.start_file(
        "repo-commit/README.md",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(b"repository root").unwrap();
    let archive = zip.finish().unwrap().into_inner();
    assert_eq!(
        Bundle::from_zip(&archive, Some("skills/my-skill"))
            .unwrap()
            .files,
        bundle.files
    );
}

#[test]
fn external_reinstall_does_not_merge_into_a_disabled_record() {
    let f = Fixture::new();
    let repo = &f.repository;
    let bundle = complete_bundle();
    bundle
        .write_to(&f.root.join("codex/skills/my-skill"))
        .unwrap();
    let record = repo.list().unwrap().remove(0);
    repo.toggle(&record).unwrap();
    bundle
        .write_to(&f.root.join("codex/skills/my-skill"))
        .unwrap();
    let records = repo.list().unwrap();
    assert_eq!(records.len(), 2);
    let disabled = records.iter().find(|r| !r.enabled()).unwrap();
    assert!(repo.toggle(disabled).is_err());
    repo.delete(disabled).unwrap();
    let records = repo.list().unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].enabled());
    assert_eq!(records[0].bundle().unwrap().files, bundle.files);
}

#[test]
#[ignore = "Requires installed Codex clients; optionally calls Claude Code with a small test prompt"]
fn real_clients_discover_saved_skills_and_observe_toggle() {
    let mut fixture = Fixture::new();
    fixture.repository.roots[2].path = fixture.root.join("claude-project/.claude/skills");
    fs::create_dir_all(fixture.root.join("workspace")).unwrap();
    let proof = format!("MUNGBEAN_SKILL_PROOF_{}", nonce());
    let mut files = BTreeMap::from([(
        "SKILL.md".into(),
        b"---\nname: mungbean-skill-check\ndescription: Verify Little-Mungbean Skill installation by reading the bundled proof file. Use only when explicitly asked to run the Little-Mungbean Skill check.\n---\n\nRead references/proof.txt relative to this SKILL.md file. Reply with only that file's exact contents. Do not invent the proof and do not modify files.\n".to_vec(),
    )]);
    files.insert("references/proof.txt".into(), proof.as_bytes().to_vec());
    let bundle = Bundle::from_files(files).unwrap();
    let source = fixture.root.join("import-source");
    bundle.write_to(&source).unwrap();
    let directory_import = Bundle::import(&source).unwrap();
    let archive = fixture.root.join("import.zip");
    fs::write(&archive, directory_import.zip_bytes().unwrap()).unwrap();
    let zip_import = Bundle::import(&archive).unwrap();
    fixture
        .repository
        .save(None, &zip_import, &ClientTarget::ALL, None)
        .unwrap();
    let exported = fs::read_dir(&fixture.repository.export_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(Bundle::import(&exported).unwrap().files, bundle.files);

    let verify = |phase: &str| {
        let output = std::process::Command::new("node")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-skills-clients.mjs"))
            .arg(&fixture.root)
            .arg(phase)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{phase}: {stdout}\n{stderr}");
        println!("{stdout}");
    };
    verify("installed");
    let record = fixture.repository.list().unwrap().remove(0);
    fixture.repository.toggle(&record).unwrap();
    verify("disabled");
    let record = fixture.repository.list().unwrap().remove(0);
    fixture.repository.toggle(&record).unwrap();
    verify("enabled");
    let record = fixture.repository.list().unwrap().remove(0);
    fixture.repository.delete(&record).unwrap();
    verify("removed");
    assert!(fixture.repository.list().unwrap().is_empty());
}

#[tokio::test]
#[ignore = "Explicit ClawHub network test; writes only to temporary client roots"]
async fn live_market_download_install_export_and_remove() {
    let fixture = Fixture::new();
    let page = crate::skill_registry::fetch("", None).await.unwrap();
    assert!(!page.entries.is_empty());
    let next = crate::skill_registry::fetch("", Some(page.cursor.as_deref().unwrap()))
        .await
        .unwrap();
    assert!(!next.entries.is_empty());
    assert!(
        next.entries
            .iter()
            .any(|entry| !page.entries.iter().any(|old| old.key() == entry.key()))
    );
    let search = crate::skill_registry::fetch("pdf", None).await.unwrap();
    assert!(!search.entries.is_empty());
    let entry = crate::skill_registry::Entry {
        owner: "awspace".into(),
        slug: "pdf".into(),
        title: "PDF".into(),
        description: String::new(),
        version: "0.1.0".into(),
    };
    let (bundle, origin) = crate::skill_registry::download(&entry).await.unwrap();
    assert!(bundle.files.len() > 1);
    fixture
        .repository
        .save(None, &bundle, &ClientTarget::ALL, Some(origin.clone()))
        .unwrap();
    let record = fixture.repository.list().unwrap().remove(0);
    assert_eq!(record.locations.len(), 3);
    assert_eq!(record.origin.as_ref(), Some(&origin));
    for location in &record.locations {
        assert_eq!(
            Bundle::from_directory(&location.path).unwrap().files,
            bundle.files
        );
    }
    let zip = fs::read_dir(&fixture.repository.export_dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(Bundle::import(&zip).unwrap().files, bundle.files);
    fixture.repository.toggle(&record).unwrap();
    let record = fixture.repository.list().unwrap().remove(0);
    assert!(!record.enabled());
    fixture.repository.toggle(&record).unwrap();
    let record = fixture.repository.list().unwrap().remove(0);
    assert_eq!(record.bundle().unwrap().files, bundle.files);
    fixture.repository.delete(&record).unwrap();
    assert!(fixture.repository.list().unwrap().is_empty());
    let report = serde_json::json!({
        "catalogEntries": page.entries.len(),
        "paginationEntries": next.entries.len(),
        "searchEntries": search.entries.len(),
        "source": origin.key(),
        "version": origin.version,
        "filesPreserved": bundle.files.len(),
        "nativeClientCopies": 3,
        "zipExportMatches": true,
        "toggleRestoreDelete": true,
    });
    if let Some(folder) = std::env::var_os("MUNGBEAN_SKILLS_REPORT_DIR") {
        fs::create_dir_all(&folder).unwrap();
        fs::write(
            PathBuf::from(folder).join("market.json"),
            serde_json::to_vec_pretty(&report).unwrap(),
        )
        .unwrap();
    }
    println!("{report}");
}
