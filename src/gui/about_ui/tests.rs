use super::*;
use crate::gui::test_render::rasterize;
use std::collections::HashMap;

#[test]
fn about_select_all_copies_the_full_document_including_scrolled_out_sections() {
    for language in [i18n::Language::ZhCn, i18n::Language::En] {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        configure_style(&ctx);
        let mut state = State { open: true };
        let screen = Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(layout::WINDOW_WIDTH, layout::WINDOW_HEIGHT),
        );
        let mut draw = |events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| state.render_language(ui, screen.shrink(15.0), language, &[], &[]),
            )
        };
        draw(vec![]);
        draw(vec![egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        }]);
        let output = draw(vec![egui::Event::Copy]);
        let copied = output
            .platform_output
            .commands
            .iter()
            .find_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text),
                _ => None,
            })
            .expect("Select All must copy the complete About page");
        assert!(copied.starts_with(language.text("关于", "About")));
        assert!(copied.contains(language.text(FAQ.last().unwrap().2, FAQ.last().unwrap().3)));
        assert!(copied.ends_with(language.text(
            "本项目使用 Apache License 2.0 开源。",
            "This project is open source under the Apache License 2.0."
        )));
    }
}

#[test]
fn about_uses_fixed_panel_renders_both_languages_and_closes_with_escape() {
    for (name, language) in [("zh", i18n::Language::ZhCn), ("en", i18n::Language::En)] {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        configure_style(&ctx);
        let assets = GuiAssets::load(&ctx);
        let mut state = State { open: true };
        let mut textures = HashMap::new();
        let mut last = None;
        let screen = Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(layout::WINDOW_WIDTH, layout::WINDOW_HEIGHT),
        );
        let mut render = |events, time| {
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
                    render_header(ui, &assets, header);
                    let status = Rect::from_min_max(
                        egui::pos2(0.0, screen.bottom() - layout::STATUS_HEIGHT),
                        screen.max,
                    );
                    let content = Rect::from_min_max(
                        header.left_bottom() + egui::vec2(0.0, layout::DIVIDER_HEIGHT),
                        egui::pos2(screen.right(), status.top() - layout::DIVIDER_HEIGHT),
                    );
                    state.render_language(
                        ui,
                        mcp_ui::panel_rect(content, status),
                        language,
                        &[],
                        &[],
                    );
                },
            )
        };
        for frame in 0..3 {
            let output = render(vec![], frame as f64 * 0.25);
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
        let panel = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Rect(r) if r.fill == PANEL && r.rect.width() > 300.0 => Some(r.rect),
                _ => None,
            })
            .unwrap();
        assert!((panel.width() - (layout::WINDOW_WIDTH - 30.0)).abs() < 0.01);
        let text = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.text()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains(language.text("关于", "About")));
        let tray_answer = language.text(FAQ[5].2, FAQ[5].3);
        assert!(output.shapes.iter().any(|s| matches!(&s.shape,
            egui::Shape::Text(t) if t.galley.text() == tray_answer
                && t.galley.job.sections.iter().all(|section| section.format.color == TEXT_MUTED)
        )), "FAQ answers must keep the answer color even when they contain a bullet separator");
        assert!(text.contains(language.text("主要功能", "Main features")));
        assert!(text.contains(language.text("暂无赞助商", "No sponsors yet")));
        assert!(
            text.contains(
                language.text("MCP Server 管理与市场", "MCP Server management and market")
            )
        );
        if language == i18n::Language::En {
            assert!(!text.contains("cmdpe.com") && !text.contains("作者博客"));
            assert!(text.contains("You may also become a sponsor. Your info will be shown below or in vendor presets. Email is available at the bottom."));
            assert!(output.shapes.iter().any(|s| matches!(&s.shape,
                egui::Shape::Text(t) if t.galley.job.sections.iter().any(|section|
                    &t.galley.job.text[section.byte_range.start.0..section.byte_range.end.0] == "coffee"
                        && section.format.color == ABOUT_ACCENT
                )
            )));
        }
        for label in ["Star", "heimen115@gmail.com"] {
            assert!(
                output.shapes.iter().any(|s| matches!(&s.shape,
                    egui::Shape::Text(t) if t.galley.job.sections.iter().any(|section|
                    &t.galley.job.text[section.byte_range.start.0..section.byte_range.end.0] == label
                            && section.format.color == ABOUT_ACCENT
                    )
                )),
                "{label} should use the link color in {name}"
            );
        }
        let lights: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Circle(circle)
                    if circle.radius == 3.0 && circle.fill != Color32::TRANSPARENT =>
                {
                    Some(circle.fill)
                }
                _ => None,
            })
            .collect();
        let mut expected_lights = vec![BLUE_BRIGHT; FEATURES.len()];
        expected_lights.extend([
            TEXT_MUTED,
            TEXT_MUTED,
            BLUE_BRIGHT,
            GREEN,
            Color32::BLACK,
            GREEN,
        ]);
        assert_eq!(lights, expected_lights);
        assert!(!output.shapes.iter().any(|shape| matches!(
            &shape.shape,
            egui::Shape::Circle(circle)
                if circle.radius == 3.0
                    && circle.fill == Color32::TRANSPARENT
                    && circle.stroke.color == TEXT_MUTED
        )));
        let main_heading = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t)
                    if t.galley.text() == language.text("主要功能", "Main features") =>
                {
                    Some(t)
                }
                _ => None,
            })
            .unwrap();
        assert!(
            (main_heading.pos.x + main_heading.galley.size().x / 2.0 - panel.center().x).abs()
                < 0.1
        );
        for (zh, en) in FEATURES {
            let feature = language.text(zh, en);
            assert!(text.contains(feature));
            assert!(!feature.chars().any(|c| c as u32 >= 0x1f300));
        }
        let intro = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t)
                    if t.galley.text().contains("AI")
                        && t.galley
                            .text()
                            .starts_with(language.text("小绿豆", "Little Mungbean")) =>
                {
                    Some(t)
                }
                _ => None,
            })
            .unwrap();
        let first_feature = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t)
                    if t.galley.text() == language.text(FEATURES[0].0, FEATURES[0].1) =>
                {
                    Some(t)
                }
                _ => None,
            })
            .unwrap();
        let first_row = &intro.galley.rows[0];
        let last_row = first_feature.galley.rows.last().unwrap();
        let start = intro.pos + egui::vec2(0.5, first_row.rect().center().y);
        let end = first_feature.pos
            + egui::vec2(last_row.rect().right() - 0.5, last_row.rect().center().y);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: egui::Modifiers::NONE,
        };
        render(vec![egui::Event::PointerMoved(start)], 0.75);
        render(vec![button(start, true)], 0.8);
        render(vec![egui::Event::PointerMoved(end)], 0.9);
        render(vec![button(end, false)], 1.0);
        let copied = render(vec![egui::Event::Copy], 1.1);
        assert!(copied.platform_output.commands.iter().any(|command| matches!(command,
            egui::OutputCommand::CopyText(text) if text.contains("AI") && text.contains("GitHub")
                && text.contains(language.text("主要功能", "Main features")) && text.contains("Codex CLI")
        )), "About text should support selecting and copying across paragraphs in {name}");
        if let Some(folder) = std::env::var_os("MUNGBEAN_ABOUT_PREVIEW_DIR") {
            fs::create_dir_all(&folder).unwrap();
            rasterize(&ctx, output, &textures)
                .save_png(PathBuf::from(&folder).join(format!("about-{name}.png")))
                .unwrap();
            let mut scrolled = None;
            for frame in 0..3 {
                scrolled = Some(render(
                    vec![
                        egui::Event::PointerMoved(egui::pos2(300.0, 250.0)),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, -600.0),
                            phase: egui::TouchPhase::Move,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    2.0 + frame as f64 * 0.25,
                ));
            }
            rasterize(&ctx, scrolled.unwrap(), &textures)
                .save_png(PathBuf::from(folder).join(format!("about-{name}-scrolled.png")))
                .unwrap();
        }
        render(
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            3.0,
        );
        assert!(!state.open);
    }
}

#[test]
fn sponsor_cards_wrap_after_eight_align_left_and_allow_missing_websites() {
    use crate::gui_providers::{Provider, ProviderVisibility};

    let ctx = egui::Context::default();
    install_fonts(&ctx);
    configure_style(&ctx);
    let mut providers = (0..9)
        .map(|index| Provider {
            id: format!("sponsor-{index}"),
            name: format!("Sponsor {index}"),
            endpoint: String::new(),
            icon_url: String::new(),
            icon_bytes: Vec::new(),
            website: Some(format!("https://sponsor-{index}.example")),
            lang: None,
            api_path_mode: None,
            protocols: None,
            expires: None,
            show_in: ProviderVisibility::About,
        })
        .collect::<Vec<_>>();
    providers.push(Provider {
        id: "preset-only".into(),
        name: "Preset only".into(),
        endpoint: "https://api.example/v1".into(),
        icon_url: String::new(),
        icon_bytes: Vec::new(),
        website: Some("https://provider.example".into()),
        lang: None,
        api_path_mode: None,
        protocols: None,
        expires: None,
        show_in: ProviderVisibility::ProviderPresets,
    });
    providers.push(Provider {
        id: "missing-website".into(),
        name: "Missing website".into(),
        endpoint: String::new(),
        icon_url: String::new(),
        icon_bytes: Vec::new(),
        website: None,
        lang: None,
        api_path_mode: None,
        protocols: None,
        expires: None,
        show_in: ProviderVisibility::Both,
    });

    let screen = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 160.0));
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        },
        |ui| render_sponsors(ui, &providers, &[], i18n::Language::En),
    );
    let mut cards: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect)
                if (rect.rect.width() - layout::PROVIDER_BUTTON_SIZE).abs() < 0.01
                    && (rect.rect.height() - layout::PROVIDER_BUTTON_SIZE).abs() < 0.01
                    && rect.stroke.width > 0.0 =>
            {
                Some(rect.rect)
            }
            _ => None,
        })
        .collect();
    cards.sort_by(|left, right| left.top().total_cmp(&right.top()));
    assert_eq!(cards.len(), 10);
    assert!(
        cards[..8]
            .iter()
            .all(|card| (card.top() - cards[0].top()).abs() < 0.01)
    );
    assert!(cards[8].top() > cards[0].top());
    assert!((cards[8].left() - cards[0].left()).abs() < 0.01);
    assert!((cards[9].top() - cards[8].top()).abs() < 0.01);
}
