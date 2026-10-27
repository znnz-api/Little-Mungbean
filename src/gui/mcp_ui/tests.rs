use super::super::test_render::rasterize;
use super::*;
use std::collections::HashMap;

#[test]
fn model_source_menu_is_centered_and_hover_region_does_not_extend_sideways() {
    let button = Rect::from_min_size(egui::pos2(260.0, 300.0), egui::vec2(34.0, 34.0));
    let popup = model_source_popup_rect(button, 124.0);

    assert_eq!(popup.center().x, button.center().x);
    assert_eq!(popup.bottom(), button.top() - 6.0);
    assert!(model_source_menu_hovered(
        egui::pos2(button.center().x, button.top() - 3.0),
        popup,
        button
    ));
    assert!(!model_source_menu_hovered(
        egui::pos2(popup.right() + 7.0, popup.center().y),
        popup,
        button
    ));
}

fn sample() -> State {
    let mut state = State {
        open: true,
        market_loaded: true,
        ..Default::default()
    };
    for (name, description, registry) in [
        (
            "Filesystem",
            "读取、搜索和整理指定目录中的文件。很长的说明不会撑宽卡片，悬停时显示完整内容。",
            Some("example/filesystem"),
        ),
        ("GitHub", "浏览代码仓库，处理 Issue 与 Pull Request。", None),
        ("Memory", "保存项目知识，让对话保留长期记忆。", None),
    ] {
        state.records.push(mcp::Server {
            id: name.into(),
            name: name.into(),
            description: description.into(),
            config: serde_json::json!({"command":"node"}),
            bindings: if name == "Filesystem" {
                ClientTarget::ALL
                    .into_iter()
                    .map(|target| {
                        serde_json::from_value(serde_json::json!({
                            "target": target,
                            "path": "preview-mcp.json",
                            "format": "Json",
                            "scope": [],
                            "key": name,
                            "native": {"command": "node"},
                            "enabled": true,
                        }))
                        .unwrap()
                    })
                    .collect()
            } else {
                vec![]
            },
            registry_name: registry.map(str::to_owned),
        });
    }
    state.entries = vec![
        mcp_registry::Entry {
            name: "example/filesystem".into(),
            title: "Filesystem".into(),
            description: "读取、搜索和整理指定目录中的文件。".into(),
            website_url: None,
            config: Some(serde_json::json!({"command":"node"})),
            requires_setup: false,
            setup_note: "Node.js".into(),
        },
        mcp_registry::Entry {
            name: "example/browser".into(),
            title: "Browser".into(),
            description: "浏览网页与自动化操作，支持完整说明文字悬停显示。".into(),
            website_url: Some("https://example.com/browser".into()),
            config: Some(serde_json::json!({"command":"node"})),
            requires_setup: false,
            setup_note: "Node.js".into(),
        },
    ];
    state
}

fn codex_builtin_node_repl() -> mcp::Server {
    let config = serde_json::json!({"command": r"C:\Users\test\AppData\Local\OpenAI\Codex\runtimes\cua_node\runtime-v1\bin\node_repl.exe", "args": []});
    serde_json::from_value(serde_json::json!({
        "id": "builtin-node-repl",
        "name": "node_repl",
        "description": "",
        "config": config,
        "bindings": [{
            "target": ClientTarget::CodexDesktop,
            "path": "preview-codex-config.toml",
            "format": "Codex",
            "scope": [],
            "key": "node_repl",
            "native": config,
            "enabled": true
        }],
        "registry_name": null
    }))
    .unwrap()
}

#[test]
fn builtin_node_repl_card_hides_mutations_and_opens_a_read_only_view() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    configure_style(&ctx);
    let assets = GuiAssets::load(&ctx);
    let builtin = codex_builtin_node_repl();
    let mut custom = builtin.clone();
    custom.id = "user-node-repl".into();
    custom.config = serde_json::json!({"command": r"C:\Tools\node_repl.exe"});
    let mut state = State {
        open: true,
        records: vec![builtin, custom],
        ..Default::default()
    };
    assert!(matches_local_search(&state.records[0], "JavaScript"));
    let mut time = 0.0;
    let mut step = |events: Vec<egui::Event>| {
        time += 0.25;
        let mut command = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(380.0, 394.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                ui.set_width(320.0);
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 6.0);
                command = state.local_cards(ui, &assets, false);
            },
        );
        assert!(
            command.is_none(),
            "protected controls must not dispatch writes"
        );
        output
    };
    let output = step(vec![]);
    let cards: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(r) if r.fill == ROW && r.rect.width() == 320.0 => Some(r.rect),
            _ => None,
        })
        .collect();
    assert_eq!(cards.len(), 2);
    let toggles = output.shapes.iter().filter(|shape| matches!(&shape.shape, egui::Shape::Rect(r) if r.rect.size() == egui::vec2(24.0, 14.0))).count();
    assert_eq!(toggles, 1, "the custom namesake keeps its switch");
    let title_right = cards[0].right() - 41.0;
    for pos in [
        egui::pos2(title_right - 8.0, cards[0].top() + 15.0),
        egui::pos2(title_right - 26.0, cards[0].top() + 15.0),
    ] {
        step(vec![egui::Event::PointerMoved(pos)]);
        step(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        let output = step(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        for texture in [&assets.history_edit, &assets.history_delete] {
            assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Mesh(mesh) if mesh.texture_id == texture.id())), "protected card must not show edit or delete");
        }
    }
    let view = egui::pos2(cards[0].right() - 20.0, cards[0].center().y);
    step(vec![egui::Event::PointerMoved(view)]);
    step(vec![egui::Event::PointerButton {
        pos: view,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    }]);
    step(vec![egui::Event::PointerButton {
        pos: view,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    let form = state
        .form
        .as_ref()
        .expect("view button opens configuration");
    assert!(form.read_only());
    assert!(form.description.contains("JavaScript"));
    assert!(state.delete_confirm.is_none());
    let output = frame(&ctx, &assets, &mut state, vec![], 8.0);
    assert!(output.shapes.iter().any(
        |shape| matches!(&shape.shape, egui::Shape::Text(t) if t.galley.text() == "查看 MCP Server")
    ));
    assert!(
        !output
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape, egui::Shape::Text(t) if t.galley.text() == "保存"))
    );
    assert!(state.job.is_none());
}

#[test]
fn builtin_node_repl_json_can_be_selected_and_copied_but_not_edited() {
    let ctx = egui::Context::default();
    let builtin = codex_builtin_node_repl();
    let mut form = Form {
        original: Some(builtin.clone()),
        json: serde_json::to_string_pretty(&builtin.config).unwrap(),
        ..Default::default()
    };
    let original = form.json.clone();
    let mut step = |events: Vec<egui::Event>| {
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(380.0, 394.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                ui.set_width(320.0);
                assert!(!json_editor(ui, &mut form));
            },
        );
        assert_eq!(form.json, original);
        output
    };
    step(vec![]);
    let pos = egui::pos2(50.0, 25.0);
    step(vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    step(vec![egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    let modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    step(vec![egui::Event::Key {
        key: egui::Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }]);
    let copied = step(vec![egui::Event::Copy]);
    assert!(copied.platform_output.commands.iter().any(
        |command| matches!(command, egui::OutputCommand::CopyText(text) if text == &original)
    ));
    step(vec![
        egui::Event::Paste("replacement".into()),
        egui::Event::Text("changed".into()),
        egui::Event::Key {
            key: egui::Key::Backspace,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
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
                egui::pos2(0.0, header.bottom()),
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
            let rect = panel_rect(content, status);
            let log_center = egui::pos2(
                screen.right()
                    - layout::STATUS_PADDING_RIGHT
                    - layout::STATUS_LOG_BUTTON_SIZE / 2.0,
                status.top()
                    + layout::STATUS_PADDING_TOP
                    + (status.height()
                        - layout::STATUS_PADDING_TOP
                        - layout::STATUS_PADDING_BOTTOM)
                        / 2.0,
            );
            assert!(
                (log_center.y - layout::STATUS_LOG_BUTTON_SIZE / 2.0 - rect.bottom() - 2.0).abs()
                    < 0.01
            );
            state.render(ui, rect, assets, false);
            let log_rect =
                Rect::from_center_size(log_center, Vec2::splat(layout::STATUS_LOG_BUTTON_SIZE));
            ui.painter()
                .rect_filled(log_rect, 12, Color32::from_gray(59));
            icon_at(ui, &assets.log, log_center, 18.0);
            icon_at(ui, &assets.prompt, egui::pos2(23.0, log_center.y), 16.0);
            ellipsis(
                ui,
                Rect::from_min_max(
                    egui::pos2(40.0, log_center.y - 12.0),
                    egui::pos2(log_rect.left() - 6.0, log_center.y + 12.0),
                ),
                "MCP Server",
                11.0,
                TEXT_MAIN,
            );
        },
    )
}

#[test]
fn fixed_panel_form_and_market_layouts_render_with_internal_scrolling() {
    for case in [
        "local",
        "search",
        "search-filled",
        "form",
        "form-empty",
        "market",
        "market-menu",
    ] {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        configure_style(&ctx);
        let assets = GuiAssets::load(&ctx);
        let mut state = sample();
        if case.starts_with("search") {
            state.search_open = true;
            if case == "search-filled" {
                state.search_text = "Memory".into();
                state.search_query = "Memory".into();
            }
        }
        if case == "form" {
            state.form = Some(Form {
                name: "my-mcp-server".into(),
                description: "简要说明此 MCP Server 的用途".into(),
                targets: vec![ClientTarget::CodexDesktop],
                ..Default::default()
            });
        }
        if case == "form-empty" {
            state.form = Some(Form::default());
        }
        if case.starts_with("market") {
            state.market = true;
            state.expanded = Some("example/browser".into());
        }
        let mut textures = HashMap::new();
        let mut last = None;
        let mut picker = egui::Pos2::ZERO;
        for i in 0..4 {
            let events = if case == "market-menu" && i > 0 {
                let mut events = vec![egui::Event::PointerMoved(picker)];
                if i < 3 {
                    events.push(egui::Event::PointerButton {
                        pos: picker,
                        button: egui::PointerButton::Primary,
                        pressed: i == 1,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                events
            } else {
                vec![]
            };
            let output = frame(&ctx, &assets, &mut state, events, f64::from(i) * 0.25);
            if case == "market-menu" && i == 0 {
                picker = output
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Rect(r) if r.rect.size() == egui::vec2(118.0, 30.0) => {
                            Some(r.rect.center())
                        }
                        _ => None,
                    })
                    .unwrap();
            }
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
        if case == "market-menu" {
            assert_eq!(output.shapes.iter().filter(|s| matches!(&s.shape,
                egui::Shape::Text(t) if t.galley.text() == i18n::tr("MCP 官方市场", "MCP Official Registry")
            )).count(), 2);
        }
        assert!(state.open);
        assert!(state.job.is_none());
        let panel = output
            .shapes
            .iter()
            .find_map(|s| {
                if let egui::Shape::Rect(r) = &s.shape {
                    (r.fill == PANEL && r.rect.width() > 300.0).then_some(r.rect)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(panel.bottom() < layout::WINDOW_HEIGHT - layout::STATUS_PADDING_BOTTOM);
        assert!(
            output
                .shapes
                .iter()
                .filter_map(|s| if let egui::Shape::Text(t) = &s.shape {
                    Some(t.galley.text())
                } else {
                    None
                })
                .any(|text| text.contains("MCP Server"))
        );
        if let Some(folder) = std::env::var_os("MUNGBEAN_MCP_PREVIEW_DIR") {
            fs::create_dir_all(&folder).unwrap();
            rasterize(&ctx, output, &textures)
                .save_png(PathBuf::from(folder).join(format!("mcp-{case}.png")))
                .unwrap();
        }
    }
}

#[test]
fn installed_market_card_collapses_pending_targets() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    configure_style(&ctx);
    let assets = GuiAssets::load(&ctx);
    let mut state = sample();
    state.market = true;
    state.expanded = Some("example/browser".into());
    state.targets = vec![ClientTarget::CodexDesktop];
    let output = frame(&ctx, &assets, &mut state, vec![], 0.0);
    let card = output
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.fill == ROW && r.rect.height() == layout::MCP_CARD_HEIGHT => {
                Some(r.rect)
            }
            _ => None,
        })
        .unwrap();
    let pos = card.center();
    frame(
        &ctx,
        &assets,
        &mut state,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
        0.25,
    );
    frame(
        &ctx,
        &assets,
        &mut state,
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
        0.5,
    );
    assert!(state.expanded.is_none());
    assert!(state.targets.is_empty());
    assert_eq!(
        state.status,
        format!("Filesystem • {}", i18n::tr("已安装", "Installed"))
    );
}

#[test]
fn market_load_failure_is_reported_in_runtime_log_only() {
    let ctx = egui::Context::default();
    let (sender, receiver) = std::sync::mpsc::channel();
    sender.send(Err("registry timeout".to_owned())).unwrap();
    let mut state = State {
        job: Some(receiver),
        job_is_market: true,
        ..Default::default()
    };

    let event = state.poll(&ctx).unwrap();

    assert_eq!(event.0, "registry timeout");
    assert!(event.1);
    assert!(state.error.is_empty());
    assert!(state.warning);
    assert!(state.status.contains("See runtime log") || state.status.contains("查看运行日志"));
}

#[test]
fn market_description_tooltip_works_on_collapsed_and_expanded_summaries() {
    for expanded in [false, true] {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        configure_style(&ctx);
        let assets = GuiAssets::load(&ctx);
        let mut state = sample();
        state.market = true;
        state.entries.remove(0);
        if expanded {
            state.expanded = Some("example/browser".into());
        }
        let initial = frame(&ctx, &assets, &mut state, vec![], 0.0);
        let card = initial
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Rect(r) if r.fill == ROW && r.rect.width() > 250.0 => Some(r.rect),
                _ => None,
            })
            .unwrap();
        let pointer = card.min + egui::vec2(80.0, 20.0);
        let mut output = initial;
        for index in 1..=6 {
            output = frame(
                &ctx,
                &assets,
                &mut state,
                if index == 1 {
                    vec![egui::Event::PointerMoved(pointer)]
                } else {
                    vec![]
                },
                f64::from(index) * 0.5,
            );
        }
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text)
                if text.galley.text() == state.entries[0].description
                    && text.galley.job.sections.iter().any(|section| section.format.color == Color32::WHITE)
        )), "hovering the market summary must show its full description, expanded={expanded}");
        assert_eq!(state.expanded.is_some(), expanded);
        assert!(state.job.is_none());
    }
}

#[test]
fn json_editor_keeps_long_lines_on_one_row_without_widening_the_form() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    configure_style(&ctx);
    let mut form = Form {
        json: format!(
            "{{\"url\":\"https://example.test/{}\"}}",
            "long-path/".repeat(30)
        ),
        ..Default::default()
    };
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(380.0, 394.0),
            )),
            ..Default::default()
        },
        |ui| {
            ui.set_width(300.0);
            json_editor(ui, &mut form);
            assert!(ui.min_rect().width() <= 300.0);
        },
    );
    let text = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == form.json => Some(text),
            _ => None,
        })
        .unwrap();
    assert_eq!(text.galley.rows.len(), 1);
    assert!(text.galley.size().x > 300.0);
}

#[test]
fn json_editor_horizontal_scrollbar_only_appears_for_overflow() {
    super::super::test_render::assert_horizontal_editor_scrollbar_tracks_overflow(
        |ui, document| {
            let mut form = Form {
                json: document.clone(),
                json_height: 120.0,
                ..Default::default()
            };
            json_editor(ui, &mut form);
            *document = form.json;
        },
    );
}

#[test]
fn mark_colors_are_stable_and_vary_by_server_name() {
    assert_eq!(mark_colors("filesystem"), mark_colors("filesystem"));
    assert_ne!(mark_colors("filesystem"), mark_colors("memory"));
}

#[test]
fn local_toggle_clicks_dispatch_only_the_card_under_the_pointer() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    configure_style(&ctx);
    let assets = GuiAssets::load(&ctx);
    let mut state = sample();
    let mut time = 0.0;
    let mut step = |events| {
        time += 0.25;
        let mut command = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(380.0, 394.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                ui.set_width(320.0);
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 6.0);
                command = state.local_cards(ui, &assets, false);
            },
        );
        assert!(
            state.job.is_none(),
            "UI interaction must not write user configuration in this test"
        );
        (command, output)
    };
    let (_, output) = step(vec![]);
    let toggles: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.rect.size() == egui::vec2(24.0, 14.0) => {
                Some(r.rect.center())
            }
            _ => None,
        })
        .collect();
    assert_eq!(toggles.len(), 3);
    for index in [0, 2, 1, 0] {
        step(vec![egui::Event::PointerMoved(toggles[index])]);
        let (command, _) = step(vec![egui::Event::PointerButton {
            pos: toggles[index],
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(command.is_none());
        let (command, _) = step(vec![egui::Event::PointerButton {
            pos: toggles[index],
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        let Some(Command::Toggle(record)) = command else {
            panic!("one card toggle expected")
        };
        assert_eq!(record.id, ["Filesystem", "GitHub", "Memory"][index]);
    }
}

#[test]
fn card_hover_outline_includes_summary_install_and_target_controls() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    configure_style(&ctx);
    let assets = GuiAssets::load(&ctx);
    let mut state = sample();
    state.market = true;
    state.expanded = Some("example/browser".into());
    let output = frame(&ctx, &assets, &mut state, vec![], 0.0);
    let expanded = output
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.fill == ROW && r.rect.height() == expanded_card_height() => {
                Some(r.rect)
            }
            _ => None,
        })
        .unwrap();
    let panel = output
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.fill == PANEL && r.rect.width() > 300.0 => Some(r.rect),
            _ => None,
        })
        .unwrap();
    assert!((panel.right() - expanded.right() - 10.0).abs() < 0.01);
    for (index, pos) in [
        expanded.min + egui::vec2(20.0, 20.0),
        expanded.right_top() + egui::vec2(-22.0, 22.0),
        expanded.min + egui::vec2(25.0, 80.0),
    ]
    .into_iter()
    .enumerate()
    {
        for repeat in 0..3 {
            let output = frame(
                &ctx,
                &assets,
                &mut state,
                vec![egui::Event::PointerMoved(pos)],
                1.0 + index as f64 + f64::from(repeat) * 0.25,
            );
            assert!(output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.rect == expanded && r.stroke.color == Color32::from_rgba_unmultiplied(20, 126, 255, 84))), "card outline must remain visible over descendants");
        }
    }
}

#[test]
fn model_source_hover_menu_selects_gateway_and_respects_disabled_state() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    configure_style(&ctx);
    let assets = GuiAssets::load(&ctx);
    let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(380.0, 394.0));
    let rect = Rect::from_min_size(egui::pos2(277.0, 303.0), Vec2::splat(34.0));
    let mut time = 0.0;
    let mut step = |events, enabled| {
        time += 0.25;
        let mut choice = None;
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                choice = model_source_button(ui, rect, ModelListMode::BuiltIn, enabled, &assets);
            },
        );
        (choice, output)
    };
    let (_, output) = step(vec![egui::Event::PointerMoved(rect.center())], true);
    // Newly created Areas can require one frame to settle their geometry.
    let (_, next) = step(vec![egui::Event::PointerMoved(rect.center())], true);
    let menu = next
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.fill == Color32::from_rgb(48, 48, 48) => Some(r.rect),
            _ => None,
        })
        .unwrap();
    assert!((menu.center().x - rect.center().x).abs() < 0.01);
    assert!((menu.bottom() - (rect.top() - 6.0)).abs() < 0.01);
    let option = next
        .shapes
        .iter()
        .chain(output.shapes.iter())
        .find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == model_mode_label(ModelListMode::Gateway) => {
                Some(t.pos + t.galley.size() / 2.0)
            }
            _ => None,
        })
        .unwrap();
    step(vec![egui::Event::PointerMoved(option)], true);
    step(
        vec![egui::Event::PointerButton {
            pos: option,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
        true,
    );
    let (choice, _) = step(
        vec![egui::Event::PointerButton {
            pos: option,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
        true,
    );
    assert_eq!(choice, Some(ModelListMode::Gateway));
    let (choice, output) = step(vec![egui::Event::PointerMoved(rect.center())], false);
    assert!(choice.is_none());
    assert!(!output.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t)if t.galley.text()==model_mode_label(ModelListMode::Gateway))));
    let icon = output
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == assets.default_models.id() => Some(mesh),
            _ => None,
        })
        .unwrap();
    assert!(
        icon.vertices
            .iter()
            .all(|v| v.color == Color32::from_gray(128))
    );
}

#[test]
fn local_save_keeps_toolbar_appearance() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    configure_style(&ctx);
    let assets = GuiAssets::load(&ctx);
    let mut state = sample();
    let before = frame(&ctx, &assets, &mut state, vec![], 0.0);
    let (_sender, receiver) = mpsc::channel();
    state.job = Some(receiver);
    state.writing = true;
    let during = frame(&ctx, &assets, &mut state, vec![], 0.25);
    let add_color = |output: &egui::FullOutput| {
        output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == i18n::tr("添加", "Add") => {
                    Some(t.fallback_color)
                }
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(add_color(&before), add_color(&during));
    let refresh_positions = |output: &egui::FullOutput| {
        output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Mesh(mesh) if mesh.texture_id == assets.refresh.id() => {
                    Some(mesh.vertices.iter().map(|v| v.pos).collect::<Vec<_>>())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(refresh_positions(&before), refresh_positions(&during));
}

#[test]
fn search_submit_and_clear_filter_local_cards_without_changing_records() {
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
            egui::Shape::Mesh(mesh) if mesh.texture_id == assets.search.id() => Some(
                (mesh
                    .vertices
                    .iter()
                    .map(|vertex| vertex.pos.to_vec2())
                    .fold(Vec2::ZERO, |total, position| total + position)
                    / mesh.vertices.len() as f32)
                    .to_pos2(),
            ),
            _ => None,
        })
        .unwrap();
    let click = |state: &mut State, position, time| {
        frame(
            &ctx,
            &assets,
            state,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
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
                pos: position,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            time + 0.1,
        )
    };
    click(&mut state, search, 0.1);
    assert!(state.search_open);
    frame(&ctx, &assets, &mut state, vec![], 0.3);
    frame(
        &ctx,
        &assets,
        &mut state,
        vec![egui::Event::Text("MEMORY".into())],
        0.4,
    );
    let output = frame(
        &ctx,
        &assets,
        &mut state,
        vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        0.5,
    );
    assert_eq!(state.search_query, "MEMORY");
    assert!(
        output.shapes.iter().any(
            |shape| matches!(&shape.shape, egui::Shape::Text(t) if t.galley.text() == "Memory")
        )
    );
    assert!(!output.shapes.iter().any(
        |shape| matches!(&shape.shape, egui::Shape::Text(t) if t.galley.text() == "Filesystem")
    ));
    click(&mut state, search - egui::vec2(22.0, 0.0), 0.6);
    assert!(state.search_text.is_empty());
    assert!(state.search_query.is_empty());
    assert_eq!(state.records.len(), 3);
    assert!(state.job.is_none());
    assert!(matches_local_search(&state.records[0], "整理"));
    assert!(!matches_local_search(&state.records[0], "unknown-server"));

    click(&mut state, search, 0.8);
    assert!(!state.search_open, "an empty search must restore the title");
    let output = frame(&ctx, &assets, &mut state, vec![], 1.0);
    assert!(output.shapes.iter().any(|shape| matches!(
        &shape.shape, egui::Shape::Text(text) if text.galley.text() == "MCP Server"
    )));
    click(&mut state, search, 1.1);
    assert!(state.search_open);
    state.search_text = "Filesystem".into();
    click(&mut state, search, 1.4);
    assert!(state.search_open, "a nonempty search must remain open");
    assert_eq!(state.search_query, "Filesystem");
    state.search_text = "   ".into();
    click(&mut state, search, 1.7);
    assert!(!state.search_open);
    assert!(
        state.search_query.is_empty(),
        "closing an empty field clears the active filter"
    );
    assert_eq!(state.records.len(), 3);
}

#[test]
fn clearing_search_discards_outdated_market_results_and_errors() {
    let ctx = egui::Context::default();
    for failure in [false, true] {
        let mut state = sample();
        state.search_query = "new-query".into();
        state.job_query = Some("old-query".into());
        state.job_is_market = true;
        let (sender, receiver) = mpsc::channel();
        state.job = Some(receiver);
        sender
            .send(if failure {
                Err("obsolete network failure".into())
            } else {
                Ok(Reply {
                    records: None,
                    page: Some(mcp_registry::Page {
                        entries: vec![],
                        cursor: Some("obsolete-cursor".into()),
                        search_query: None,
                    }),
                    append: false,
                    message: "old results".into(),
                    warning: String::new(),
                    saved: false,
                })
            })
            .unwrap();
        assert!(state.poll(&ctx).is_none());
        assert_eq!(state.entries.len(), 2);
        assert!(state.cursor.is_none());
        assert!(state.warning);
        assert_eq!(
            state.status,
            format!(
                "MCP Server • {} • {}",
                i18n::tr("本地", "Local"),
                i18n::tr("未找到匹配的 MCP Server", "No matching MCP Servers")
            )
        );
        assert!(!state.market_loaded);
        assert!(state.job.is_none());
    }
}

#[test]
fn empty_search_feedback_stays_in_status_bar_until_search_is_complete() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    configure_style(&ctx);
    let assets = GuiAssets::load(&ctx);
    let mut state = sample();
    state.search_text = "unknown-server".into();
    state.submit_search(&ctx);
    assert!(state.warning);
    assert_eq!(
        state.status,
        format!(
            "MCP Server • {} • {}",
            i18n::tr("本地", "Local"),
            i18n::tr("未找到匹配的 MCP Server", "No matching MCP Servers")
        )
    );
    let output = frame(&ctx, &assets, &mut state, vec![], 0.0);
    assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains(i18n::tr("未找到匹配的 MCP Server", "No matching MCP Servers")))));
    state.market = true;
    state.market_loaded = true;
    state.entries.clear();
    state.list_status();
    assert!(state.warning);
    assert!(
        state
            .status
            .starts_with(&format!("MCP Server • {}", i18n::tr("市场", "Market")))
    );
    state.cursor = Some("more-candidates".into());
    state.list_status();
    assert!(
        !state.warning,
        "unfinished candidate pages must not report no results"
    );
    let (_sender, receiver) = mpsc::channel();
    state.job = Some(receiver);
    state.job_is_market = true;
    state.market_searching = true;
    state.list_status();
    assert_eq!(state.status, market_progress(true));
    let output = frame(&ctx, &assets, &mut state, vec![], 0.25);
    assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == market_progress(true))));
}

#[test]
fn save_conflict_stays_above_save_and_disappears_after_returning_to_list() {
    let ctx = egui::Context::default();
    install_fonts(&ctx);
    configure_style(&ctx);
    let assets = GuiAssets::load(&ctx);
    let mut state = sample();
    state.form = Some(Form {
        name: "node_repl".into(),
        json: "{\"command\":\"node\"}".into(),
        json_height: 120.0,
        ..Default::default()
    });
    let conflict = "同名 MCP 已存在，未覆盖: test-config.toml • node_repl";
    let (sender, receiver) = mpsc::channel();
    state.job = Some(receiver);
    state.job_is_save = true;
    state.writing = true;
    sender.send(Err(conflict.into())).unwrap();
    assert_eq!(state.poll(&ctx), Some((conflict.into(), true)));
    assert_eq!(state.form.as_ref().unwrap().error, conflict);
    assert!(
        state.error.is_empty(),
        "form failures must not become list notices"
    );
    let help = ctx.run_ui(egui::RawInput::default(), |ui| {
        ui.set_width(300.0);
        form_help(ui, state.form.as_ref().unwrap());
    });
    assert!(help.shapes.iter().any(
        |shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == conflict)
    ));

    let output = frame(&ctx, &assets, &mut state, vec![], 0.0);
    let panel = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect) if rect.fill == PANEL && rect.rect.width() > 300.0 => {
                Some(rect.rect)
            }
            _ => None,
        })
        .unwrap();
    let back = panel.min
        + egui::vec2(
            layout::MCP_PANEL_PADDING + 8.0,
            layout::MCP_PANEL_PADDING + 9.0,
        );
    frame(
        &ctx,
        &assets,
        &mut state,
        vec![
            egui::Event::PointerMoved(back),
            egui::Event::PointerButton {
                pos: back,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
        0.25,
    );
    frame(
        &ctx,
        &assets,
        &mut state,
        vec![egui::Event::PointerButton {
            pos: back,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
        0.5,
    );
    assert!(state.form.is_none());
    assert!(state.error.is_empty());
    assert!(!state.warning);
    assert_eq!(state.status, "MCP Server");
    let output = frame(&ctx, &assets, &mut state, vec![], 0.75);
    assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains("同名 MCP"))));

    // A pending save may finish after the user has already returned to the list.
    let (sender, receiver) = mpsc::channel();
    state.job = Some(receiver);
    state.job_is_save = true;
    state.writing = true;
    sender.send(Err(conflict.into())).unwrap();
    assert_eq!(state.poll(&ctx), Some((conflict.into(), false)));
    assert!(state.error.is_empty());
    assert!(!state.warning);
    assert_eq!(state.status, "MCP Server");
}

#[test]
fn market_progress_distinguishes_fetch_buttons_from_search_with_a_query_present() {
    let ctx = egui::Context::default();
    let mut state = sample();
    state.market = true;
    let (_sender, receiver) = mpsc::channel();
    // Keep a request pending so testing progress never contacts the real registry.
    state.job = Some(receiver);
    state.job_is_market = true;
    state.search_text = "filesystem".into();
    state.submit_search(&ctx);
    assert_eq!(state.status, "MCP Server • 正在拉取 & 搜索市场...");
    state.list_status();
    assert_eq!(state.status, market_progress(true));
    for cursor in [None, Some("next-page".into())] {
        state.fetch_market(&ctx, cursor, false);
        assert_eq!(state.search_query, "filesystem");
        assert_eq!(state.status, "MCP Server • 正在拉取市场...");
        state.list_status();
        assert_eq!(state.status, market_progress(false));
    }
    state.search_text = "memory".into();
    state.submit_search(&ctx);
    assert_eq!(state.status, market_progress(true));
}

#[test]
fn successful_local_scan_updates_status_without_a_runtime_log() {
    let ctx = egui::Context::default();
    for warning in ["", "Configuration could not be fully read"] {
        let mut state = sample();
        let (sender, receiver) = mpsc::channel();
        state.job = Some(receiver);
        sender
            .send(Ok(Reply {
                records: Some(state.records.clone()),
                page: None,
                append: false,
                message: "MCP Server • 本地 • 3".into(),
                warning: warning.into(),
                saved: false,
            }))
            .unwrap();
        let (log, is_warning) = state.poll(&ctx).unwrap();
        assert_eq!(state.status, "MCP Server • 本地 • 3");
        assert_eq!(state.records.len(), 3);
        if warning.is_empty() {
            assert!(log.is_empty());
            assert!(!is_warning);
        } else {
            assert!(log.contains(warning));
            assert!(is_warning);
        }
    }
}
