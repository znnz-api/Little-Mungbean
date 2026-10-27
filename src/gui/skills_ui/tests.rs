use super::*;
use crate::gui::test_render::rasterize;
use std::collections::HashMap;

#[test]
fn skill_editor_horizontal_scrollbar_only_appears_for_overflow() {
    crate::gui::test_render::assert_horizontal_editor_scrollbar_tracks_overflow(|ui, document| {
        document_editor(ui, document, &mut 120.0, true);
    });
}

fn sample() -> State {
    let mut state = State {
        open: true,
        loaded: true,
        loaded_query: Some(String::new()),
        ..Default::default()
    };
    for (name, description, targets) in [
        (
            "pdf",
            "Extract text and tables, merge documents and fill PDF forms.",
            vec![ClientTarget::CodexCli, ClientTarget::CodexDesktop],
        ),
        (
            "web-design",
            "审查网页的可用性与视觉表现，支持完整的描述提示。",
            vec![ClientTarget::ClaudeCode],
        ),
    ] {
        state.records.push(skills::Record {
            id: name.into(),
            name: name.into(),
            description: description.into(),
            origin: Some(skills::Origin {
                owner: "example".into(),
                slug: name.into(),
                version: "1.0.0".into(),
            }),
            read_only: false,
            locations: vec![skills::Location {
                path: PathBuf::from(name),
                targets,
                fingerprint: String::new(),
                disabled: false,
                archived: None,
            }],
        });
    }
    for (slug, title, description) in [
        (
            "pdf",
            "PDF",
            "Extract and edit PDF documents, including text, tables and forms.",
        ),
        (
            "data-analysis",
            "Data Analysis",
            "Analyse spreadsheets and other structured data, preserving sources and showing useful charts.",
        ),
    ] {
        state.entries.push(registry::Entry {
            owner: "example".into(),
            slug: slug.into(),
            title: title.into(),
            description: description.into(),
            version: "1.0.0".into(),
        });
    }
    state.list_status();
    state
}
fn frame(
    ctx: &egui::Context,
    assets: &GuiAssets,
    state: &mut State,
    events: Vec<egui::Event>,
    time: f64,
) -> egui::FullOutput {
    let screen = Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(layout::WINDOW_WIDTH, layout::WINDOW_HEIGHT),
    );
    ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(screen),
            time: Some(time),
            events,
            ..Default::default()
        },
        |ui| {
            paint_window_background(ui, screen);
            let header = Rect::from_min_size(
                screen.min,
                egui::vec2(screen.width(), layout::HEADER_HEIGHT),
            );
            render_header(ui, assets, header);
            let divider = Rect::from_min_size(
                header.left_bottom(),
                egui::vec2(screen.width(), layout::DIVIDER_HEIGHT),
            );
            render_divider(ui, divider);
            let status = Rect::from_min_max(
                egui::pos2(0.0, screen.bottom() - layout::STATUS_HEIGHT),
                screen.max,
            );
            let content = Rect::from_min_max(
                divider.left_bottom(),
                egui::pos2(screen.right(), status.top() - layout::DIVIDER_HEIGHT),
            );
            state.render(ui, mcp_ui::panel_rect(content, status), assets, false);
            let center = egui::pos2(
                screen.right()
                    - layout::STATUS_PADDING_RIGHT
                    - layout::STATUS_LOG_BUTTON_SIZE / 2.0,
                status.center().y,
            );
            ui.painter().rect_filled(
                Rect::from_center_size(center, Vec2::splat(layout::STATUS_LOG_BUTTON_SIZE)),
                12,
                Color32::from_gray(59),
            );
            mcp_ui::icon_at(ui, &assets.log, center, 18.0);
            mcp_ui::icon_at(ui, &assets.prompt, egui::pos2(23.0, center.y), 16.0);
            ellipsis(
                ui,
                Rect::from_min_max(
                    egui::pos2(40.0, center.y - 12.0),
                    egui::pos2(center.x - 25.0, center.y + 12.0),
                ),
                &state.status,
                11.0,
                TEXT_MAIN,
            );
        },
    )
}
#[test]
fn skill_panels_use_fixed_mcp_geometry_and_render_all_views() {
    for case in ["local", "market", "search", "form", "form-claude"] {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        configure_style(&ctx);
        let assets = GuiAssets::load(&ctx);
        let mut state = sample();
        if case == "market" {
            state.market = true;
            state.expanded = Some("example/data-analysis".into());
            state.targets = vec![ClientTarget::CodexDesktop];
        }
        if case == "search" {
            state.search_open = true;
            state.search_text = "pdf".into();
            state.query = "pdf".into();
        }
        if case.starts_with("form") {
            let mut form = Form::new(skills::template());
            if case == "form-claude" {
                form.targets = vec![ClientTarget::ClaudeDesktop];
                form.height = 120.0;
            }
            state.form = Some(form);
        }
        let mut textures = HashMap::new();
        let mut last = None;
        for i in 0..3 {
            let events = if case == "form-claude" {
                vec![
                    egui::Event::PointerMoved(egui::pos2(300.0, 250.0)),
                    egui::Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, -500.0),
                        phase: egui::TouchPhase::Move,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            } else {
                vec![]
            };
            let output = frame(&ctx, &assets, &mut state, events, i as f64 * 0.25);
            for (id, delta) in &output.textures_delta.set {
                let egui::ImageData::Color(image) = &delta.image;
                if let Some([x, y]) = delta.pos {
                    let target: &mut egui::ColorImage = textures.get_mut(id).unwrap();
                    for row in 0..image.height() {
                        for col in 0..image.width() {
                            target[(x + col, y + row)] = image[(col, row)];
                        }
                    }
                } else {
                    textures.insert(*id, image.as_ref().clone());
                }
            }
            last = Some(output);
        }
        let output = last.unwrap();
        assert!(state.open && state.job.is_none());
        let panel = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Rect(r) if r.fill == PANEL && r.rect.width() > 300.0 => Some(r.rect),
                _ => None,
            })
            .unwrap();
        assert!(panel.bottom() < layout::WINDOW_HEIGHT - layout::STATUS_PADDING_BOTTOM);
        if case == "local" || case == "market" {
            let cards: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Rect(r) if r.fill == ROW && r.rect.height() >= 44.0 => {
                        Some(r.rect)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(cards.len(), 2);
            assert!(
                cards
                    .iter()
                    .all(|r| (panel.right() - r.right() - 10.0).abs() < 0.01)
            );
        }
        if case == "form" {
            let has_text = |output: &egui::FullOutput, needle: &str| {
                output.shapes.iter().any(|shape| {
                    matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains(needle))
                })
            };
            assert!(has_text(
                &output,
                i18n::tr(
                    "请选择一个包含 SKILL.md 的目录",
                    "Choose a folder containing SKILL.md"
                )
            ));
            assert!(!has_text(
                &output,
                i18n::tr(
                    "Claude Desktop：ZIP 导出至下载文件夹",
                    "Claude Desktop: exports a ZIP"
                )
            ));
        }
        if case == "form-claude" {
            let note = output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Rect(r) if r.fill == Color32::from_rgb(58, 37, 30) => Some(r.rect),
                    _ => None,
                })
                .expect("Claude Desktop notice is visible after scrolling");
            assert!((note.left() - panel.left() - 10.0).abs() < 0.1);
            assert!(
                (panel.right() - note.right() - 10.0).abs() < 0.1,
                "notice should align with the editor: {note:?}, {panel:?}"
            );
            assert!(output.shapes.iter().any(|s| matches!(&s.shape,
                egui::Shape::Text(t) if t.galley.text().starts_with("Claude Desktop")
                    && t.galley.job.sections.iter().all(|section| section.format.color == Color32::from_rgb(255, 180, 84) && section.format.line_height == Some(15.0))
            )));
        }
        if let Some(folder) = std::env::var_os("MUNGBEAN_SKILLS_PREVIEW_DIR") {
            fs::create_dir_all(&folder).unwrap();
            rasterize(&ctx, output, &textures)
                .save_png(PathBuf::from(folder).join(format!("skills-{case}.png")))
                .unwrap();
        }
    }
}
#[test]
fn search_button_hides_empty_input_and_submits_nonempty_input() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    configure_style(&ctx);
    let assets = GuiAssets::load(&ctx);
    let mut state = sample();
    let output = frame(&ctx, &assets, &mut state, vec![], 0.0);
    let search = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == assets.search.id() => {
                Some(mesh.calc_bounds().center())
            }
            _ => None,
        })
        .unwrap();
    let click = |state: &mut State, time| {
        frame(
            &ctx,
            &assets,
            state,
            vec![
                egui::Event::PointerMoved(search),
                egui::Event::PointerButton {
                    pos: search,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            time,
        );
        frame(
            &ctx,
            &assets,
            state,
            vec![egui::Event::PointerButton {
                pos: search,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            time + 0.1,
        );
    };
    click(&mut state, 0.1);
    assert!(state.search_open);
    click(&mut state, 0.4);
    assert!(!state.search_open);
    let output = frame(&ctx, &assets, &mut state, vec![], 0.6);
    assert!(output.shapes.iter().any(|shape| matches!(
        &shape.shape, egui::Shape::Text(text) if text.galley.text() == "Skills"
    )));
    click(&mut state, 0.7);
    state.search_text = "pdf".into();
    click(&mut state, 1.0);
    assert!(state.search_open);
    assert_eq!(state.query, "pdf");
    state.search_text = "  ".into();
    click(&mut state, 1.3);
    assert!(!state.search_open);
    assert!(state.query.is_empty());
    assert_eq!(state.records.len(), 2);
    assert!(
        state.job.is_none(),
        "local search must not fetch market data"
    );
}

#[test]
fn local_search_includes_description_and_empty_status_is_a_warning() {
    let mut state = sample();
    assert!(local_match(&state.records[0], "tables"));
    assert!(local_match(&state.records[0], "PDF forms"));
    assert!(!local_match(&state.records[0], "browser"));
    state.query = "does-not-exist".into();
    state.list_status();
    assert!(state.warning);
    assert!(
        state
            .status
            .contains(i18n::tr("未找到匹配的 Skill", "No matching Skills"))
    );
    assert!(
        state
            .status
            .starts_with(&format!("Skills • {} •", i18n::tr("本地", "Local")))
    );
    state.market = true;
    state.entries.clear();
    state.list_status();
    assert!(
        state
            .status
            .starts_with(&format!("Skills • {} •", i18n::tr("市场", "Market")))
    );
}

#[test]
fn claude_desktop_save_status_is_short_and_keeps_export_path_in_log() {
    let ctx = egui::Context::default();
    let mut state = sample();
    let bundle = skills::template();
    let name = bundle.metadata.name.clone();
    let mut form = Form::new(bundle);
    form.targets = vec![ClientTarget::ClaudeDesktop];
    state.form = Some(form);
    let (send, recv) = mpsc::channel();
    state.job = Some(recv);
    let log_message = format!("{name} • ZIP path: C:\\skills\\{name}.zip");
    send.send(Ok(Reply {
        records: Some(vec![]),
        page: None,
        form: None,
        message: log_message.clone(),
        saved: true,
    }))
    .unwrap();

    let event = state.poll(&ctx).unwrap();

    assert_eq!(
        state.status,
        format!(
            "{name} • {}",
            i18n::tr(
                "已保存 • 已导出 ZIP • 请查看运行日志",
                "Saved • ZIP exported • See runtime log"
            )
        )
    );
    assert_eq!(event.0, log_message);
    assert!(state.form.is_none());
}
#[test]
fn stale_market_search_cannot_replace_current_results() {
    let ctx = egui::Context::default();
    let mut state = sample();
    state.query = "new".into();
    state.job_query = "old".into();
    state.job_market = true;
    state.open = false;
    let (send, recv) = mpsc::channel();
    state.job = Some(recv);
    send.send(Ok(Reply {
        records: None,
        page: Some(registry::Page {
            entries: vec![],
            cursor: None,
        }),
        form: None,
        message: String::new(),
        saved: false,
    }))
    .unwrap();
    state.poll(&ctx);
    assert_eq!(state.entries.len(), 2);
}
#[test]
fn installing_identity_does_not_confuse_different_owners() {
    let state = sample();
    let origin = state.records[0].origin.as_ref().unwrap();
    assert_eq!(origin.key(), state.entries[0].key());
    let mut other = state.entries[0].clone();
    other.owner = "another-author".into();
    assert_ne!(origin.key(), other.key());
}

#[test]
fn shared_directory_target_selection_updates_both_codex_clients() {
    let state = sample();
    let record = &state.records[0];
    let before = vec![ClientTarget::CodexCli, ClientTarget::CodexDesktop];
    let mut selected = vec![ClientTarget::CodexDesktop];
    sync_shared_targets(Some(record), &before, &mut selected);
    assert!(selected.is_empty());
    let mut selected = vec![ClientTarget::CodexDesktop];
    sync_shared_targets(Some(record), &[], &mut selected);
    assert!(selected.contains(&ClientTarget::CodexCli));
}
