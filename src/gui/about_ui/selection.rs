//! One read-only selection spans the About page's separately styled text blocks.
use super::*;
use egui::text::{CCursor, CCursorRange};
use std::sync::Arc;

#[derive(Clone)]
struct Block {
    response: egui::Response,
    pos: egui::Pos2,
    galley: Arc<egui::Galley>,
    painter: egui::Painter,
    color: Color32,
    offset: usize,
    length: usize,
}

#[derive(Clone, Default)]
struct Document {
    blocks: Vec<Block>,
    text: String,
    length: usize,
}

#[derive(Clone, Default)]
struct Selection {
    range: Option<CCursorRange>,
    dragging: bool,
    text: String,
}

fn document_id(ctx: &egui::Context) -> egui::Id {
    egui::Id::new(("about-selectable-document", ctx.viewport_id()))
}

pub(super) fn begin(ctx: &egui::Context) {
    let id = document_id(ctx);
    ctx.data_mut(|data| data.insert_temp(id, Document::default()));
}

pub(super) fn paint(
    ui: &egui::Ui,
    response: &egui::Response,
    pos: egui::Pos2,
    galley: Arc<egui::Galley>,
    color: Color32,
) {
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    let id = document_id(ui.ctx());
    ui.ctx().data_mut(|data| {
        let doc = data.get_temp_mut_or_default::<Document>(id);
        if !doc.text.is_empty() {
            doc.text.push('\n');
            doc.length += 1;
        }
        let length = galley.text().chars().count();
        doc.blocks.push(Block {
            response: response.clone(),
            pos,
            galley: galley.clone(),
            painter: ui.painter().clone(),
            color,
            offset: doc.length,
            length,
        });
        doc.text.push_str(galley.text());
        doc.length += length;
    });
}

pub(super) fn finish(ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let id = document_id(&ctx);
    let doc = ctx
        .data_mut(|data| data.remove_temp::<Document>(id))
        .unwrap_or_default();
    let mut state = ctx
        .data(|data| data.get_temp::<Selection>(id.with("selection")))
        .unwrap_or_default();
    if state.text != doc.text {
        state.range = None;
        state.dragging = false;
        state.text.clone_from(&doc.text);
    }
    let pointer = ui.input(|input| input.pointer.interact_pos());
    let context_click = cfg!(target_os = "macos") && ui.input(|input| input.modifiers.ctrl);
    let started = ui.input(|input| input.pointer.primary_pressed()) && !context_click;
    if started {
        if let Some(block) = doc
            .blocks
            .iter()
            .find(|block| block.response.contains_pointer())
            && let Some(pos) = pointer
        {
            let cursor = CCursor::new(cursor_at(block, pos));
            let anchor = if ui.input(|input| input.modifiers.shift) {
                state.range.map_or(cursor, |range| range.secondary)
            } else {
                cursor
            };
            state.range = Some(CCursorRange::two(anchor, cursor));
            state.dragging = true;
            if let Some(focused) = ctx.memory(|memory| memory.focused()) {
                ctx.memory_mut(|memory| memory.surrender_focus(focused));
            }
        } else {
            state.range = None;
        }
    }
    if state.dragging
        && ui.input(|input| input.pointer.primary_down() || input.pointer.primary_released())
        && let Some(pos) = pointer
        && let Some(block) = doc.blocks.iter().min_by(|a, b| {
            a.response
                .rect
                .distance_to_pos(pos)
                .total_cmp(&b.response.rect.distance_to_pos(pos))
        })
        && let Some(range) = state.range.as_mut()
    {
        range.primary = CCursor::new(cursor_at(block, pos));
    }
    if ui.input(|input| input.pointer.primary_released()) {
        state.dragging = false;
    }
    let mut action = None;
    if ctx.memory(|memory| memory.focused().is_none()) {
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::A)) {
            action = Some(text_menu::Action::SelectAll);
        } else if ui.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Copy))
        }) {
            action = Some(text_menu::Action::Copy);
        }
    }
    for block in &doc.blocks {
        let can_copy = state.range.is_some_and(|range| !range.is_empty());
        if let Some(chosen) =
            text_menu::read_only_menu(ui, &block.response, can_copy, !doc.text.is_empty())
        {
            action = Some(chosen);
            break;
        }
    }
    match action {
        Some(text_menu::Action::SelectAll) => {
            state.range = Some(CCursorRange::two(CCursor::new(0), CCursor::new(doc.length)));
            ctx.request_repaint();
        }
        Some(text_menu::Action::Copy) => {
            if let Some(range) = state.range.filter(|range| !range.is_empty()) {
                ctx.copy_text(range.slice_str(&doc.text).to_owned());
            }
        }
        _ => {}
    }
    for block in doc.blocks {
        let mut galley = block.galley;
        if let Some(range) = state.range {
            let [start, end] = range.sorted_cursors();
            let start = start.index.0.saturating_sub(block.offset).min(block.length);
            let end = end.index.0.saturating_sub(block.offset).min(block.length);
            egui::text_selection::visuals::paint_text_selection(
                &mut galley,
                ui.visuals(),
                &CCursorRange::two(CCursor::new(start), CCursor::new(end)),
                None,
            );
        }
        block.painter.galley(block.pos, galley, block.color);
    }
    if state.dragging {
        ctx.set_cursor_icon(egui::CursorIcon::Text);
    }
    ctx.data_mut(|data| data.insert_temp(id.with("selection"), state));
}

fn cursor_at(block: &Block, pos: egui::Pos2) -> usize {
    block.offset + block.galley.cursor_from_pos(pos - block.pos).index.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_across_paragraphs_and_native_copy_preserve_unicode_selection() {
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        let mut time = 0.0;
        let first = "第一段 green🙂";
        let second = "Second paragraph";
        let mut draw = |events| {
            time += 0.1;
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(500.0, 250.0),
                    )),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    begin(&ctx);
                    paragraph(ui, first, 14.0, TEXT_MAIN);
                    paragraph(ui, second, 14.0, TEXT_MAIN);
                    finish(ui);
                },
            )
        };
        draw(vec![]);
        let output = draw(vec![]);
        let position = |needle, index| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == needle => Some(
                        text.pos
                            + text
                                .galley
                                .pos_from_cursor(CCursor::new(index))
                                .center()
                                .to_vec2(),
                    ),
                    _ => None,
                })
                .unwrap()
        };
        let start = position(first, 1);
        let end = position(second, 6);
        let button = |pos, button, pressed| egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        draw(vec![egui::Event::PointerMoved(start)]);
        draw(vec![button(start, egui::PointerButton::Primary, true)]);
        draw(vec![egui::Event::PointerMoved(end)]);
        draw(vec![button(end, egui::PointerButton::Primary, false)]);
        text_menu::test_select_next(&ctx, text_menu::Action::Copy);
        draw(vec![button(end, egui::PointerButton::Secondary, true)]);
        let output = draw(vec![button(end, egui::PointerButton::Secondary, false)]);
        let expected = "一段 green🙂\nSecond";
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(text) if text == expected)
        }));
        assert_eq!(
            text_menu::test_entries(&ctx),
            vec![
                (text_menu::Action::Copy, true),
                (text_menu::Action::SelectAll, true)
            ]
        );
        text_menu::test_select_next(&ctx, text_menu::Action::SelectAll);
        draw(vec![button(end, egui::PointerButton::Secondary, true)]);
        draw(vec![button(end, egui::PointerButton::Secondary, false)]);
        let output = draw(vec![
            egui::Event::Copy,
            egui::Event::Text("replacement".into()),
            egui::Event::Paste("paste".into()),
        ]);
        assert!(output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(text) if text == &format!("{first}\n{second}"))
        }));
    }
}
