//! Native popup menus edit egui's own buffer, selection and undo history.
use super::{egui, i18n};
use egui::{Response, text::CCursor, text::CCursorRange, text_edit::TextEditOutput};

#[derive(Clone, Copy, Default)]
pub(super) struct Options {
    pub password: bool,
    pub multiline: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

pub(super) fn initialize(creation: &eframe::CreationContext<'_>) -> anyhow::Result<()> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = creation.window_handle()?.as_raw();
        #[cfg(target_os = "macos")]
        let RawWindowHandle::AppKit(handle) = handle else {
            anyhow::bail!("The text menu needs an AppKit window");
        };
        #[cfg(target_os = "macos")]
        let owner = NativeOwner(handle.ns_view.as_ptr() as usize);
        #[cfg(windows)]
        let RawWindowHandle::Win32(handle) = handle else {
            anyhow::bail!("The text menu needs a Win32 window");
        };
        #[cfg(windows)]
        let owner = NativeOwner(handle.hwnd.get() as usize);
        creation.egui_ctx.data_mut(|data| {
            data.insert_temp(egui::Id::new("native-text-menu-owner"), owner);
        });
        // macOS shares muda's single event handler with the menu bar icon.
        #[cfg(windows)]
        muda::MenuEvent::set_event_handler(Some(|event: muda::MenuEvent| {
            on_menu_event(&event);
        }));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = creation;
    Ok(())
}

pub(super) fn attach(
    ui: &mut egui::Ui,
    mut output: TextEditOutput,
    text: &mut String,
    options: Options,
) -> Response {
    preserve_context_selection(ui, &mut output);
    let requested = output.response.secondary_clicked()
        || (cfg!(target_os = "macos")
            && output.response.clicked()
            && ui.input(|input| input.modifiers.ctrl));
    let open = cfg!(not(any(windows, target_os = "macos")))
        && egui::Popup::context_menu(&output.response).is_open();
    if ui.is_enabled() && (requested || open) {
        output.response.request_focus();
        let clipboard = clipboard_text();
        let entries = available_actions(&output, text, options, clipboard.as_deref());
        if let Some(action) = show_menu(ui, &output.response, &entries) {
            // Read again after native menu tracking, so Paste uses current clipboard text.
            let paste = (action == Action::Paste).then(clipboard_text).flatten();
            apply(
                ui.ctx(),
                &mut output,
                text,
                options,
                action,
                paste.as_deref(),
            );
        }
        ui.ctx().request_repaint();
    }
    remember_selection(&output);
    output.response.response
}

#[derive(Clone, Copy)]
struct SavedSelection(Option<CCursorRange>);

fn selection_id(id: egui::Id) -> egui::Id {
    id.with("selection-before-context-menu")
}

fn remember_selection(output: &TextEditOutput) {
    output.response.ctx.data_mut(|data| {
        data.insert_temp(
            selection_id(output.response.id),
            SavedSelection(output.state.cursor.char_range()),
        );
    });
}

fn preserve_context_selection(ui: &egui::Ui, output: &mut TextEditOutput) {
    // egui 0.35 collapses a TextEdit selection on *any* pointer press,
    // including the secondary button. Restore the last completed selection on
    // press and release, before building the menu or feeding the next undo frame.
    let context_press = ui.input(|input| {
        input.pointer.secondary_pressed()
            || (cfg!(target_os = "macos")
                && input.modifiers.ctrl
                && input.pointer.primary_pressed())
    });
    let context_click = output.response.secondary_clicked()
        || (cfg!(target_os = "macos")
            && output.response.clicked()
            && ui.input(|input| input.modifiers.ctrl));
    if context_click || (output.response.contains_pointer() && context_press) {
        let saved = ui
            .ctx()
            .data(|data| data.get_temp::<SavedSelection>(selection_id(output.response.id)));
        if let Some(SavedSelection(Some(range))) = saved {
            output.state.cursor.set_char_range(Some(range));
            output.state.clone().store(ui.ctx(), output.response.id);
            output.response.request_focus();
        }
    }
}

pub(super) fn attach_read_only(
    ui: &mut egui::Ui,
    mut output: TextEditOutput,
    text: &str,
) -> Response {
    preserve_context_selection(ui, &mut output);
    let range = selection(&output, text);
    if let Some(action) = read_only_menu(ui, &output.response, !range.is_empty(), !text.is_empty())
    {
        match action {
            Action::Copy if !range.is_empty() => {
                ui.ctx().copy_text(range.slice_str(text).to_owned())
            }
            Action::SelectAll => {
                output.state.cursor.set_char_range(Some(CCursorRange::two(
                    CCursor::new(0),
                    CCursor::new(text.chars().count()),
                )));
                output.state.clone().store(ui.ctx(), output.response.id);
            }
            _ => {}
        }
        output.response.request_focus();
        ui.ctx().request_repaint();
    }
    remember_selection(&output);
    output.response.response
}

pub(super) fn read_only_menu(
    ui: &mut egui::Ui,
    response: &Response,
    can_copy: bool,
    can_select_all: bool,
) -> Option<Action> {
    let requested = response.secondary_clicked()
        || (cfg!(target_os = "macos")
            && response.clicked()
            && ui.input(|input| input.modifiers.ctrl));
    let open = cfg!(not(any(windows, target_os = "macos")))
        && egui::Popup::context_menu(response).is_open();
    if requested || open {
        show_menu(
            ui,
            response,
            &[
                (Action::Copy, can_copy),
                (Action::SelectAll, can_select_all),
            ],
        )
    } else {
        None
    }
}

fn clipboard_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

fn selection(output: &TextEditOutput, text: &str) -> CCursorRange {
    let mut range = output
        .state
        .cursor
        .char_range()
        .unwrap_or_else(|| CCursorRange::one(CCursor::new(text.chars().count())));
    let len = text.chars().count().into();
    range.primary.index = range.primary.index.min(len);
    range.secondary.index = range.secondary.index.min(len);
    range
}

fn available_actions(
    output: &TextEditOutput,
    text: &str,
    options: Options,
    clipboard: Option<&str>,
) -> [(Action, bool); 6] {
    let range = selection(output, text);
    let state = (range, text.to_owned());
    let undoer = output.state.undoer();
    let can_copy = !options.password && !range.is_empty();
    [
        (Action::Undo, undoer.has_undo(&state)),
        (Action::Redo, undoer.has_redo(&state)),
        (Action::Cut, can_copy),
        (Action::Copy, can_copy),
        (
            Action::Paste,
            clipboard.is_some_and(|text| !text.is_empty()),
        ),
        (Action::SelectAll, !text.is_empty()),
    ]
}

fn apply(
    ctx: &egui::Context,
    output: &mut TextEditOutput,
    text: &mut String,
    options: Options,
    action: Action,
    clipboard: Option<&str>,
) {
    let mut range = selection(output, text);
    let current = (range, text.clone());
    let mut undoer = output.state.undoer();
    match action {
        Action::Undo | Action::Redo => {
            let restored = if action == Action::Undo {
                undoer.undo(&current)
            } else {
                undoer.redo(&current)
            };
            if let Some((cursor, restored)) = restored {
                range = *cursor;
                text.clone_from(restored);
            }
        }
        Action::SelectAll => {
            range = CCursorRange::two(CCursor::new(0), CCursor::new(text.chars().count()));
        }
        Action::Copy | Action::Cut if !options.password && !range.is_empty() => {
            ctx.copy_text(range.slice_str(text).to_owned());
            if action == Action::Cut {
                undoer.add_undo(&current);
                range = replace_selection(text, range, "");
            }
        }
        Action::Paste => {
            if let Some(paste) = clipboard.filter(|text| !text.is_empty()) {
                undoer.add_undo(&current);
                let paste = if options.multiline {
                    paste.to_owned()
                } else {
                    paste.replace(['\r', '\n'], " ")
                };
                range = replace_selection(text, range, &paste);
            }
        }
        _ => {}
    }
    if *text != current.1 {
        if matches!(action, Action::Cut | Action::Paste) {
            let changed = (range, text.clone());
            undoer.feed_state(ctx.input(|input| input.time), &changed);
            undoer.add_undo(&changed);
        }
        output.response.mark_changed();
    }
    output.state.set_undoer(undoer);
    output.state.cursor.set_char_range(Some(range));
    output.state.clone().store(ctx, output.response.id);
    output.response.request_focus();
    ctx.request_repaint();
}

fn replace_selection(text: &mut String, range: CCursorRange, insert: &str) -> CCursorRange {
    let start = range.primary.index.min(range.secondary.index).0;
    let end = range.primary.index.max(range.secondary.index).0;
    let byte_index = |index| {
        text.char_indices()
            .nth(index)
            .map_or(text.len(), |(i, _)| i)
    };
    text.replace_range(byte_index(start)..byte_index(end), insert);
    CCursorRange::one(CCursor::new(start + insert.chars().count()))
}

fn label(action: Action) -> &'static str {
    match action {
        Action::Undo => i18n::tr("撤销", "Undo"),
        Action::Redo => i18n::tr("重做", "Redo"),
        Action::Cut => i18n::tr("剪切", "Cut"),
        Action::Copy => i18n::tr("复制", "Copy"),
        Action::Paste => i18n::tr("粘贴", "Paste"),
        Action::SelectAll => i18n::tr("全选", "Select All"),
    }
}

#[cfg(any(windows, target_os = "macos"))]
#[derive(Clone, Copy)]
struct NativeOwner(usize);

#[cfg(any(windows, target_os = "macos"))]
#[derive(Default)]
struct MenuSelection {
    items: Vec<(muda::MenuId, Action)>,
    chosen: Option<Action>,
}

#[cfg(any(windows, target_os = "macos"))]
thread_local! {
    // muda dispatches menu actions synchronously on the UI thread.
    static ACTIVE_MENU: std::cell::RefCell<MenuSelection> = Default::default();
}

#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn on_menu_event(event: &muda::MenuEvent) {
    ACTIVE_MENU.with_borrow_mut(|menu| {
        if let Some((_, action)) = menu.items.iter().find(|(id, _)| *id == event.id) {
            menu.chosen = Some(*action);
        }
    });
}

#[cfg(any(windows, target_os = "macos"))]
fn show_menu(
    ui: &mut egui::Ui,
    _response: &Response,
    entries: &[(Action, bool)],
) -> Option<Action> {
    use muda::accelerator::{Accelerator, CMD_OR_CTRL, Code, Modifiers};
    use muda::{ContextMenu, Menu, MenuItem, PredefinedMenuItem};
    #[cfg(test)]
    if ui
        .ctx()
        .data(|data| data.get_temp::<NativeOwner>(egui::Id::new("native-text-menu-owner")))
        .is_none()
    {
        return test_menu(ui.ctx(), entries);
    }
    let owner = ui
        .ctx()
        .data(|data| data.get_temp::<NativeOwner>(egui::Id::new("native-text-menu-owner")))?;
    let menu = Menu::new();
    let mut items = Vec::new();
    for &(action, enabled) in entries {
        if matches!(action, Action::Cut | Action::SelectAll) {
            menu.append(&PredefinedMenuItem::separator()).ok()?;
        }
        let (modifiers, key) = match action {
            Action::Undo => (CMD_OR_CTRL, Code::KeyZ),
            Action::Redo if cfg!(target_os = "macos") => {
                (CMD_OR_CTRL | Modifiers::SHIFT, Code::KeyZ)
            }
            Action::Redo => (CMD_OR_CTRL, Code::KeyY),
            Action::Cut => (CMD_OR_CTRL, Code::KeyX),
            Action::Copy => (CMD_OR_CTRL, Code::KeyC),
            Action::Paste => (CMD_OR_CTRL, Code::KeyV),
            Action::SelectAll => (CMD_OR_CTRL, Code::KeyA),
        };
        let item = MenuItem::new(
            label(action),
            enabled,
            Some(Accelerator::new(Some(modifiers), key)),
        );
        menu.append(&item).ok()?;
        items.push((item.id().clone(), action));
    }
    ACTIVE_MENU.with_borrow_mut(|active| {
        *active = MenuSelection {
            items,
            chosen: None,
        }
    });
    // SAFETY: initialized from eframe's live window, used only on the GUI thread.
    // No egui or menu-event locks are held while the OS tracks the popup.
    unsafe {
        #[cfg(target_os = "macos")]
        menu.show_context_menu_for_nsview(owner.0 as *const std::ffi::c_void, None);
        #[cfg(windows)]
        menu.show_context_menu_for_hwnd(owner.0 as isize, None);
    }
    ACTIVE_MENU.with_borrow_mut(|active| std::mem::take(active).chosen)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn show_menu(
    _ui: &mut egui::Ui,
    response: &Response,
    entries: &[(Action, bool)],
) -> Option<Action> {
    let mut chosen = None;
    response.context_menu(|ui| {
        for &(action, enabled) in entries {
            if ui
                .add_enabled(enabled, egui::Button::new(label(action)))
                .clicked()
            {
                chosen = Some(action);
                ui.close();
            }
        }
    });
    chosen
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[derive(Clone, Default)]
struct TestMenu {
    next: Option<Action>,
    entries: Vec<(Action, bool)>,
}

#[cfg(test)]
pub(super) fn test_select_next(ctx: &egui::Context, action: Action) {
    ctx.data_mut(|data| {
        data.get_temp_mut_or_default::<TestMenu>(egui::Id::new("test-text-menu"))
            .next = Some(action)
    });
}

#[cfg(test)]
pub(super) fn test_entries(ctx: &egui::Context) -> Vec<(Action, bool)> {
    ctx.data(|data| {
        data.get_temp::<TestMenu>(egui::Id::new("test-text-menu"))
            .unwrap_or_default()
            .entries
    })
}

#[cfg(test)]
fn test_menu(ctx: &egui::Context, entries: &[(Action, bool)]) -> Option<Action> {
    ctx.data_mut(|data| {
        let menu = data.get_temp_mut_or_default::<TestMenu>(egui::Id::new("test-text-menu"));
        menu.entries = entries.to_vec();
        menu.next
            .take()
            .filter(|action| entries.contains(&(*action, true)))
    })
}
