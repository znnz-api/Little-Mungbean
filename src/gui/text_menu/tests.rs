use super::*;

fn frame(
    ctx: &egui::Context,
    text: &mut String,
    options: Options,
    selected: Option<std::ops::Range<usize>>,
    action: Action,
    paste: Option<&str>,
) -> (egui::FullOutput, Response) {
    let mut response = None;
    let output = ctx.run_ui(egui::RawInput::default(), |ui| {
        let mut output = egui::TextEdit::multiline(&mut *text)
            .id_salt("text-menu-test")
            .password(options.password)
            .show(ui);
        if let Some(range) = &selected {
            output.state.cursor.set_char_range(Some(CCursorRange::two(
                CCursor::new(range.start),
                CCursor::new(range.end),
            )));
        }
        apply(ctx, &mut output, text, options, action, paste);
        response = Some(output.response.response);
    });
    (output, response.unwrap())
}

#[test]
fn unicode_cut_paste_and_keyboard_undo_share_history() {
    let ctx = egui::Context::default();
    let options = Options::default();
    let mut text = "A绿豆🙂Z".to_owned();
    let (output, response) = frame(&ctx, &mut text, options, Some(1..4), Action::Cut, None);
    assert_eq!(text, "AZ");
    assert!(response.changed());
    assert!(output.platform_output.commands.iter().any(|command| {
        matches!(command, egui::OutputCommand::CopyText(text) if text == "绿豆🙂")
    }));
    frame(&ctx, &mut text, options, None, Action::Undo, None);
    assert_eq!(text, "A绿豆🙂Z");
    frame(&ctx, &mut text, options, None, Action::Redo, None);
    assert_eq!(text, "AZ");
    frame(
        &ctx,
        &mut text,
        options,
        None,
        Action::Paste,
        Some("中\r\n🙂"),
    );
    assert_eq!(text, "A中  🙂Z");
    // egui's normal keyboard handler must undo a paste made through the menu.
    let _ = ctx.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            }],
            ..Default::default()
        },
        |ui| {
            egui::TextEdit::multiline(&mut text)
                .id_salt("text-menu-test")
                .show(ui);
        },
    );
    assert_eq!(text, "AZ");
}

#[test]
fn multiline_paste_select_all_and_copy_preserve_document() {
    let ctx = egui::Context::default();
    let options = Options {
        multiline: true,
        password: false,
    };
    let mut text = "before".to_owned();
    frame(
        &ctx,
        &mut text,
        options,
        Some(0..6),
        Action::Paste,
        Some("绿豆\nsecond line"),
    );
    frame(&ctx, &mut text, options, None, Action::SelectAll, None);
    let (output, response) = frame(&ctx, &mut text, options, None, Action::Copy, None);
    assert_eq!(text, "绿豆\nsecond line");
    assert!(!response.changed());
    assert!(output.platform_output.commands.iter().any(|command| {
        matches!(command, egui::OutputCommand::CopyText(text) if text == "绿豆\nsecond line")
    }));
}

#[test]
fn masked_passwords_disable_copy_and_cut_and_still_allow_paste() {
    let ctx = egui::Context::default();
    let options = Options {
        password: true,
        multiline: false,
    };
    let mut text = "secret".to_owned();
    for action in [Action::Copy, Action::Cut] {
        let (output, _) = frame(&ctx, &mut text, options, Some(0..6), action, None);
        assert_eq!(text, "secret");
        assert!(output.platform_output.commands.is_empty());
    }
    frame(
        &ctx,
        &mut text,
        options,
        None,
        Action::Paste,
        Some("replacement"),
    );
    assert_eq!(text, "replacement");
}

#[test]
fn menu_enables_actions_for_selection_clipboard_and_history() {
    let ctx = egui::Context::default();
    let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
        let mut text = "hello".to_owned();
        let mut output = egui::TextEdit::singleline(&mut text).show(ui);
        output
            .state
            .cursor
            .set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(5))));
        let entries = available_actions(&output, &text, Options::default(), None);
        assert!(entries.contains(&(Action::Copy, true)));
        assert!(entries.contains(&(Action::Cut, true)));
        assert!(entries.contains(&(Action::Paste, false)));
        assert!(entries.contains(&(Action::Redo, false)));
        let entries = available_actions(
            &output,
            &text,
            Options {
                password: true,
                multiline: false,
            },
            Some("paste"),
        );
        assert!(entries.contains(&(Action::Copy, false)));
        assert!(entries.contains(&(Action::Cut, false)));
        assert!(entries.contains(&(Action::Paste, true)));
    });
}

#[cfg(any(windows, target_os = "macos"))]
#[test]
fn native_menu_events_only_apply_to_the_active_text_menu() {
    let id = muda::MenuId::new("test-text-menu-copy");
    ACTIVE_MENU.with_borrow_mut(|menu| {
        menu.items = vec![(id.clone(), Action::Copy)];
    });
    on_menu_event(&muda::MenuEvent {
        id: muda::MenuId::new("tray-open"),
    });
    ACTIVE_MENU.with_borrow(|menu| assert_eq!(menu.chosen, None));
    on_menu_event(&muda::MenuEvent { id: id.clone() });
    ACTIVE_MENU.with_borrow_mut(|menu| {
        assert_eq!(menu.chosen, Some(Action::Copy));
        *menu = MenuSelection::default();
    });
    on_menu_event(&muda::MenuEvent { id });
    ACTIVE_MENU.with_borrow(|menu| assert_eq!(menu.chosen, None));
}

#[test]
fn right_button_press_preserves_selection_and_redo_until_the_menu_opens() {
    let ctx = egui::Context::default();
    let mut text = "A绿豆🙂Z".to_owned();
    let mut rect = egui::Rect::NOTHING;
    let mut time = 0.0;
    let mut draw = |events, action| {
        time += 0.1;
        let mut selected = None;
        let mut entries = None;
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(420.0, 200.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                let mut output = egui::TextEdit::singleline(&mut text)
                    .id_salt("right-click-selection-test")
                    .show(ui);
                rect = output.response.rect;
                if action == Some(Action::Cut) {
                    output
                        .state
                        .cursor
                        .set_char_range(Some(CCursorRange::two(CCursor::new(1), CCursor::new(4))));
                }
                if let Some(action) = action {
                    apply(
                        &ctx,
                        &mut output,
                        &mut text,
                        Options::default(),
                        action,
                        None,
                    );
                }
                preserve_context_selection(ui, &mut output);
                selected = Some(selection(&output, &text).slice_str(&text).to_owned());
                entries = Some(available_actions(&output, &text, Options::default(), None));
                remember_selection(&output);
            },
        );
        (rect, selected.unwrap(), entries.unwrap())
    };
    // A menu edit followed by Undo leaves both a nonempty selection and Redo.
    draw(vec![], None);
    draw(vec![], Some(Action::Cut));
    let (rect, selected, entries) = draw(vec![], Some(Action::Undo));
    assert_eq!(selected, "绿豆🙂");
    assert!(entries.contains(&(Action::Redo, true)));
    let pos = rect.center();
    for pressed in [true, false] {
        let (_, selected, entries) = draw(
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            None,
        );
        assert_eq!(
            selected, "绿豆🙂",
            "right mouse press must retain the selection"
        );
        assert!(entries.contains(&(Action::Copy, true)));
        assert!(entries.contains(&(Action::Cut, true)));
        assert!(entries.contains(&(Action::Redo, true)));
    }
}
