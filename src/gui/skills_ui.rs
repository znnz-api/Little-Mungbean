//! Skills share the approved MCP visual components, with directory-based data.
use super::mcp_ui::{
    ACCENT, ACTIVE, GREEN, PANEL, ROW, TRANSPORT_TEXT, add_button, back_button, button,
    card_surface, card_title, client_target_tags, description_tooltip, ellipsis,
    expanded_card_height, field_title, icon_at, input, install_button, mark, refresh_button,
    scroll_body, section_title, small_icon, targets, text, toggle_button,
};
use super::*;
use crate::{skill_registry as registry, skills};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[cfg(test)]
mod tests;

#[derive(Clone)]
struct Form {
    original: Option<skills::Record>,
    bundle: skills::Bundle,
    document: String,
    path: String,
    targets: Vec<ClientTarget>,
    origin: Option<skills::Origin>,
    source: Option<String>,
    height: f32,
    error: String,
}
impl Form {
    fn new(bundle: skills::Bundle) -> Self {
        let document = bundle.document();
        Self {
            original: None,
            bundle,
            document,
            path: String::new(),
            targets: vec![],
            origin: None,
            source: None,
            height: 180.0,
            error: String::new(),
        }
    }
}
enum Command {
    Local,
    Market {
        query: String,
        cursor: Option<String>,
    },
    Import(String),
    Pick(bool),
    Edit(skills::Record),
    Download(registry::Entry, Vec<ClientTarget>),
    Save(Box<Form>),
    Toggle(skills::Record),
    Delete(skills::Record),
}
struct Reply {
    records: Option<Vec<skills::Record>>,
    page: Option<registry::Page>,
    form: Option<Form>,
    message: String,
    saved: bool,
}
#[derive(Default)]
pub struct State {
    pub open: bool,
    pub status: String,
    pub warning: bool,
    market: bool,
    records: Vec<skills::Record>,
    entries: Vec<registry::Entry>,
    cursor: Option<String>,
    loaded: bool,
    loaded_query: Option<String>,
    search_open: bool,
    search_text: String,
    query: String,
    search_focus: bool,
    expanded: Option<String>,
    targets: Vec<ClientTarget>,
    form: Option<Form>,
    confirm: Option<(String, Instant)>,
    job: Option<Receiver<std::result::Result<Reply, String>>>,
    writing: bool,
    job_market: bool,
    job_query: String,
    job_append: bool,
}
impl State {
    pub fn writing(&self) -> bool {
        self.writing
    }
    pub fn show(&mut self, ctx: &egui::Context) {
        self.open = true;
        self.list_status();
        if self.job.is_none() {
            self.start(ctx, Command::Local);
        }
    }
    fn list_status(&mut self) {
        self.warning = false;
        self.status = format!(
            "Skills • {}",
            if self.market {
                i18n::tr("市场", "Market")
            } else {
                i18n::tr("本地", "Local")
            }
        );
        let empty = if self.market {
            self.loaded && self.entries.is_empty()
        } else {
            !self.records.iter().any(|r| local_match(r, &self.query))
        };
        if !self.query.is_empty() && empty {
            self.warning = true;
            self.status = format!(
                "Skills • {} • {}",
                if self.market {
                    i18n::tr("市场", "Market")
                } else {
                    i18n::tr("本地", "Local")
                },
                i18n::tr("未找到匹配的 Skill", "No matching Skills")
            );
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<(String, bool)> {
        let result = match self.job.as_ref()?.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(80));
                return None;
            }
            Err(TryRecvError::Disconnected) => Err("Skill worker disconnected".into()),
        };
        self.job = None;
        self.writing = false;
        match result {
            Ok(reply) => {
                if let Some(records) = reply.records {
                    self.records = records;
                }
                let stale = self.job_market && self.job_query != self.query;
                if let Some(page) = reply.page
                    && !stale
                {
                    if self.job_append {
                        for entry in page.entries {
                            if !self.entries.iter().any(|e| e.key() == entry.key()) {
                                self.entries.push(entry);
                            }
                        }
                    } else {
                        self.entries = page.entries;
                    }
                    self.cursor = page.cursor;
                    self.loaded = true;
                    self.loaded_query = Some(self.job_query.clone());
                }
                if let Some(form) = reply.form {
                    self.form = Some(form);
                }
                let export_status = if reply.saved {
                    self.form.as_ref().and_then(|form| {
                        form.targets
                            .contains(&ClientTarget::ClaudeDesktop)
                            .then(|| {
                                format!(
                                    "{} • {}",
                                    form.bundle.metadata.name,
                                    i18n::tr(
                                        "已保存 • 已导出 ZIP • 请查看运行日志",
                                        "Saved • ZIP exported • See runtime log"
                                    )
                                )
                            })
                    })
                } else {
                    None
                };
                let log_message = reply.message.clone();
                if reply.saved {
                    self.form = None;
                    self.expanded = None;
                }
                self.warning = false;
                self.status = export_status.unwrap_or(reply.message);
                if self.form.is_none() && self.status.is_empty() {
                    self.list_status();
                }
                if self.open
                    && self.market
                    && self.form.is_none()
                    && (stale
                        || (!self.job_market && self.loaded_query.as_deref() != Some(&self.query)))
                {
                    self.fetch(ctx, None);
                }
                Some((log_message, false))
            }
            Err(error) => {
                if let Some(form) = &mut self.form {
                    form.error = error.clone();
                }
                self.warning = true;
                self.status = i18n::tr(
                    "Skills • 操作失败 • 查看运行日志",
                    "Skills • Failed • See runtime log",
                )
                .into();
                Some((error, true))
            }
        }
    }
    fn fetch(&mut self, ctx: &egui::Context, cursor: Option<String>) {
        self.start(
            ctx,
            Command::Market {
                query: self.query.clone(),
                cursor,
            },
        );
    }
    fn start(&mut self, ctx: &egui::Context, command: Command) {
        if self.job.is_some() {
            return;
        }
        self.writing = matches!(
            &command,
            Command::Save(_) | Command::Toggle(_) | Command::Delete(_)
        );
        self.job_market = matches!(&command, Command::Market { .. });
        self.job_query = self.query.clone();
        self.job_append = matches!(
            &command,
            Command::Market {
                cursor: Some(_),
                ..
            }
        );
        if self.job_market {
            self.status = i18n::tr(
                "Skills • 正在拉取 & 搜索市场...",
                "Skills • Fetching & searching the registry...",
            )
            .into();
        } else if matches!(
            &command,
            Command::Download(..) | Command::Import(_) | Command::Pick(_) | Command::Edit(_)
        ) {
            self.status = i18n::tr(
                "Skills • 正在读取技能文件...",
                "Skills • Reading skill files...",
            )
            .into();
        }
        self.warning = false;
        let (send, recv) = mpsc::channel();
        self.job = Some(recv);
        let context = ctx.clone();
        std::thread::spawn(move || {
            let result: Result<Reply> = (|| {
                let runtime = || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                };
                let mut reply = Reply {
                    records: None,
                    page: None,
                    form: None,
                    message: String::new(),
                    saved: false,
                };
                match command {
                    Command::Pick(folder) => {
                        if let Some(path) = skills::pick_import(folder)? {
                            let mut form = Form::new(skills::Bundle::import(&path)?);
                            form.path = path.display().to_string();
                            reply.form = Some(form);
                            reply.message = i18n::tr(
                                "Skills • 请选择目标客户端后保存",
                                "Skills • Select target clients and save",
                            )
                            .into();
                        } else {
                            reply.message = i18n::tr("Skills • 添加", "Skills • Add").into();
                        }
                    }
                    Command::Market { query, cursor } => {
                        reply.page =
                            Some(runtime()?.block_on(registry::fetch(&query, cursor.as_deref()))?);
                    }
                    Command::Import(path) => {
                        let mut form = Form::new(skills::Bundle::import(Path::new(
                            path.trim().trim_matches('"'),
                        ))?);
                        form.path = path;
                        reply.message = format!(
                            "{} • {}",
                            form.bundle.metadata.name,
                            i18n::tr("请选择目标客户端后保存", "Select target clients and save")
                        );
                        reply.form = Some(form);
                    }
                    Command::Edit(record) => {
                        let mut form = Form::new(record.bundle()?);
                        form.targets = record.targets();
                        form.origin = record.origin.clone();
                        form.original = Some(record);
                        reply.form = Some(form);
                        reply.message =
                            i18n::tr("Skills • 编辑 / 查看", "Skills • Edit / View").into();
                    }
                    Command::Download(entry, targets) => {
                        let (bundle, origin) = runtime()?.block_on(registry::download(&entry))?;
                        let mut form = Form::new(bundle);
                        form.origin = Some(origin);
                        form.targets = targets;
                        form.source = Some(entry.source_url());
                        reply.message = format!(
                            "{} • {}",
                            entry.title,
                            i18n::tr("请检查技能内容后保存", "Review the skill contents and save")
                        );
                        reply.form = Some(form);
                    }
                    other => {
                        let repository = skills::Repository::discover()?;
                        reply.message = match other {
                            Command::Local => String::new(),
                            Command::Save(form) => {
                                reply.saved = true;
                                repository.save(
                                    form.original.as_ref(),
                                    &form.bundle.with_document(&form.document)?,
                                    &form.targets,
                                    form.origin.clone(),
                                )?
                            }
                            Command::Toggle(record) => repository.toggle(&record)?,
                            Command::Delete(record) => repository.delete(&record)?,
                            _ => unreachable!(),
                        };
                        reply.records = Some(repository.list()?);
                    }
                }
                Ok(reply)
            })();
            let _ = send.send(result.map_err(|e| format!("{e:#}")));
            context.request_repaint();
        });
    }
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        assets: &GuiAssets,
        clients_busy: bool,
    ) {
        ui.push_id("skills-panel", |ui| {
            self.render_inner(ui, rect, assets, clients_busy)
        });
    }
    fn render_inner(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        assets: &GuiAssets,
        clients_busy: bool,
    ) {
        ui.painter()
            .rect_filled(rect, layout::MCP_PANEL_RADIUS, PANEL);
        paint_surface_outline(ui.painter(), rect, layout::MCP_PANEL_RADIUS);
        let inner = rect.shrink(layout::MCP_PANEL_PADDING);
        let heading = Rect::from_min_size(inner.min, egui::vec2(inner.width(), 18.0));
        if !self.search_open || self.form.is_some() {
            section_title(
                ui,
                heading,
                if self.form.as_ref().is_some_and(|f| f.original.is_some()) {
                    i18n::tr("编辑 Skill", "Edit Skill")
                } else if self.form.is_some() {
                    i18n::tr("添加 Skill", "Add Skill")
                } else {
                    "Skills"
                },
                12.0,
                false,
            );
        }
        let busy = self.job.is_some();
        let blocked = busy || clients_busy;
        if back_button(
            ui,
            Rect::from_center_size(
                egui::pos2(heading.left() + 8.0, heading.center().y),
                Vec2::splat(layout::PANEL_BACK_SIZE),
            ),
            "skills-back",
        ) && !busy
        {
            if self.form.is_some() {
                self.form = None;
                self.list_status();
            } else {
                self.open = false;
            }
        }
        if self.form.is_some() {
            self.form_view(
                ui,
                Rect::from_min_max(egui::pos2(inner.left(), heading.bottom() + 8.0), inner.max),
                assets,
                blocked,
            );
            return;
        }
        self.search(ui, heading, assets);
        let refresh = Rect::from_center_size(
            egui::pos2(heading.right() - 8.0, heading.center().y),
            Vec2::splat(22.0),
        );
        if refresh_button(ui, refresh, &assets.refresh, busy, busy && !self.writing) {
            if self.market {
                self.fetch(ui.ctx(), None);
            } else {
                self.start(ui.ctx(), Command::Local);
            }
        }
        let toolbar = Rect::from_min_size(
            egui::pos2(inner.left(), heading.bottom() + 8.0),
            egui::vec2(inner.width(), 30.0),
        );
        let tabs = Rect::from_min_size(
            toolbar.min,
            egui::vec2(
                if i18n::language() == i18n::Language::ZhCn {
                    87.0
                } else {
                    99.0
                },
                30.0,
            ),
        );
        ui.painter().rect_filled(tabs, 9, ROW);
        let w = (tabs.width() - 10.0) / 2.0;
        let amount =
            ui.ctx()
                .animate_bool_with_time(ui.id().with("skills-market-slide"), self.market, 0.18);
        ui.painter().rect_filled(
            Rect::from_min_size(
                tabs.min + egui::vec2(4.0 + amount * (w + 2.0), 4.0),
                egui::vec2(w, 22.0),
            ),
            6,
            ACTIVE,
        );
        for (market, label) in [
            (false, i18n::tr("本地", "Local")),
            (true, i18n::tr("市场", "Market")),
        ] {
            let tab = Rect::from_min_size(
                tabs.min + egui::vec2(4.0 + f32::from(market) * (w + 2.0), 4.0),
                egui::vec2(w, 22.0),
            );
            let response = ui.interact(tab, ui.id().with(("skills-tab", market)), Sense::click());
            text(
                ui,
                tab,
                label,
                11.0,
                if market == self.market {
                    ACCENT
                } else {
                    TEXT_MUTED
                },
            );
            if response.clicked() && !self.writing {
                self.market = market;
                self.expanded = None;
                self.targets.clear();
                self.list_status();
                if market && self.loaded_query.as_deref() != Some(&self.query) && !busy {
                    if self.query.is_empty()
                        && let Some(page) = registry::cached()
                    {
                        self.entries = page.entries;
                        self.cursor = page.cursor;
                    }
                    self.fetch(ui.ctx(), None);
                }
            }
        }
        if self.market {
            let picker = Rect::from_min_size(
                egui::pos2(toolbar.right() - 118.0, toolbar.top()),
                egui::vec2(118.0, 30.0),
            );
            skill_registry_picker(ui, picker, assets);
        } else if add_button(
            ui,
            Rect::from_min_size(
                egui::pos2(toolbar.right() - 56.0, toolbar.top()),
                egui::vec2(56.0, 30.0),
            ),
            !clients_busy,
            busy,
        ) {
            self.form = Some(Form::new(skills::template()));
            self.status = i18n::tr("Skills • 添加", "Skills • Add").into();
        }
        let body = Rect::from_min_max(egui::pos2(inner.left(), toolbar.bottom() + 10.0), inner.max);
        let mut child = scroll_body(ui, body, "skills-list-body");
        let mut action = None;
        egui::ScrollArea::vertical()
            .id_salt(("skills-list", self.market))
            .auto_shrink([false, false])
            .max_height(body.height())
            .show(&mut child, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 6.0);
                if self.market {
                    action = self.market_cards(ui, assets, blocked);
                } else {
                    action = self.local_cards(ui, assets, blocked);
                }
            });
        if let Some(action) = action {
            self.start(ui.ctx(), action);
        }
    }
    fn search(&mut self, ui: &mut egui::Ui, heading: Rect, assets: &GuiAssets) {
        let search = Rect::from_min_size(
            egui::pos2(
                heading.right() - layout::MCP_SEARCH_RIGHT - 22.0,
                heading.top() - 2.0,
            ),
            Vec2::splat(22.0),
        );
        let mut submit = false;
        if self.search_open {
            let width = layout::MCP_SEARCH_WIDTH.min(heading.width() - 72.0);
            let field = Rect::from_min_size(
                egui::pos2(
                    heading.right() - layout::MCP_SEARCH_RIGHT - width,
                    heading.top() - 2.0,
                ),
                egui::vec2(width, layout::MCP_SEARCH_HEIGHT),
            );
            ui.painter().rect_filled(field, 7, ROW);
            let edit = Rect::from_min_max(field.min, field.max - egui::vec2(44.0, 0.0));
            let response = ui.put(edit, |ui: &mut egui::Ui| {
                let output = egui::TextEdit::singleline(&mut self.search_text)
                    .id_salt("skills-search-input")
                    .hint_text(
                        RichText::new(i18n::tr("搜索 Skill", "Search Skills"))
                            .size(layout::MCP_SEARCH_FONT_SIZE)
                            .color(TEXT_MUTED),
                    )
                    .font(FontId::proportional(layout::MCP_SEARCH_FONT_SIZE))
                    .text_color(TEXT_MAIN)
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(7, 0)))
                    .vertical_align(egui::Align::Center)
                    .min_size(edit.size())
                    .show(ui);
                super::text_menu::attach(
                    ui,
                    output,
                    &mut self.search_text,
                    super::text_menu::Options::default(),
                )
            });
            if std::mem::take(&mut self.search_focus) {
                response.request_focus();
            }
            submit = response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            let escape = (response.has_focus() || response.lost_focus())
                && ui
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            let clear = search.translate(egui::vec2(-22.0, 0.0));
            if !self.search_text.is_empty() {
                let r = ui.interact(clear, ui.id().with("skills-search-clear"), Sense::click());
                let c = clear.center();
                let stroke = Stroke::new(1.0, if r.hovered() { TEXT_MAIN } else { TEXT_MUTED });
                for sign in [-1.0, 1.0] {
                    ui.painter().line_segment(
                        [
                            c + egui::vec2(-3.0, sign * -3.0),
                            c + egui::vec2(3.0, sign * 3.0),
                        ],
                        stroke,
                    );
                }
                show_hover_tooltip(&r, i18n::tr("清除搜索", "Clear search"));
                if r.clicked() {
                    self.search_text.clear();
                    self.apply_search(ui.ctx());
                    response.request_focus();
                }
            }
            if escape {
                self.search_open = false;
                self.search_text.clear();
                self.apply_search(ui.ctx());
            }
        }
        let response = ui.interact(search, ui.id().with("skills-search"), Sense::click());
        icon_at(ui, &assets.search, search.center(), 13.0);
        show_hover_tooltip(&response, i18n::tr("搜索", "Search"));
        if response.clicked() {
            if self.search_open {
                if self.search_text.trim().is_empty() {
                    self.search_open = false;
                    self.search_focus = false;
                    self.search_text.clear();
                    submit = !self.query.is_empty();
                    ui.ctx().request_repaint();
                } else {
                    submit = true;
                }
            } else {
                self.search_open = true;
                self.search_focus = true;
                ui.ctx().request_repaint();
            }
        }
        if submit {
            self.apply_search(ui.ctx());
        }
    }

    fn apply_search(&mut self, ctx: &egui::Context) {
        self.query = self.search_text.trim().to_owned();
        self.expanded = None;
        self.targets.clear();
        if self.market {
            self.loaded = false;
            self.entries.clear();
            self.cursor = None;
            self.fetch(ctx, None);
        } else {
            self.list_status();
        }
    }
    fn local_cards(
        &mut self,
        ui: &mut egui::Ui,
        assets: &GuiAssets,
        blocked: bool,
    ) -> Option<Command> {
        let mut action = None;
        if self.records.is_empty() && self.query.is_empty() {
            ui.label(
                RichText::new(i18n::tr(
                    "未发现本地 Skill，点击添加或从市场安装。",
                    "No local Skills. Add one or install from the registry.",
                ))
                .size(11.0)
                .color(TEXT_MUTED),
            );
        }
        for record in &self.records {
            if !local_match(record, &self.query) {
                continue;
            }
            ui.push_id(&record.id, |ui| {
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(
                        ui.available_width(),
                        layout::MCP_CARD_HEIGHT + 14.0 + layout::MCP_LOCAL_CARD_BOTTOM_EXTRA,
                    ),
                    Sense::hover(),
                );
                card_surface(ui, rect);
                mark(
                    ui,
                    Rect::from_center_size(
                        egui::pos2(rect.left() + 22.0, rect.center().y),
                        Vec2::splat(28.0),
                    ),
                    &record.id,
                    &record.name,
                );
                let copy = Rect::from_min_max(
                    rect.min + egui::vec2(8.0 + 28.0 + layout::MCP_CARD_TEXT_GAP, 7.0),
                    rect.max - egui::vec2(41.0, 7.0),
                );
                let hovered = ui.rect_contains_pointer(rect);
                let delete = Rect::from_min_size(
                    egui::pos2(copy.right() - 16.0, copy.top()),
                    Vec2::splat(16.0),
                );
                let edit = delete.translate(egui::vec2(-18.0, 0.0));
                card_title(
                    ui,
                    Rect::from_min_size(copy.min, egui::vec2(copy.width(), 16.0)),
                    &record.name,
                    if record.read_only { "LINK" } else { "SKILL" },
                    TRANSPORT_TEXT,
                    if hovered { 36.0 } else { 0.0 },
                );
                ellipsis(
                    ui,
                    Rect::from_min_size(
                        copy.min + egui::vec2(0.0, 16.0),
                        egui::vec2(copy.width(), 14.0),
                    ),
                    &record.description,
                    9.0,
                    TEXT_MUTED,
                );
                client_target_tags(ui, copy.left(), copy.top() + 32.0, &record.targets());
                if toggle_button(
                    ui,
                    Rect::from_center_size(
                        egui::pos2(rect.right() - 20.0, rect.center().y),
                        egui::vec2(24.0, 14.0),
                    ),
                    record.enabled(),
                    !blocked && !record.read_only,
                ) {
                    action = Some(Command::Toggle(record.clone()));
                }
                if hovered {
                    if small_icon(
                        ui,
                        edit,
                        &assets.history_edit,
                        "edit",
                        if record.read_only {
                            i18n::tr("查看", "View")
                        } else {
                            i18n::tr("编辑", "Edit")
                        },
                        !blocked,
                    ) {
                        action = Some(Command::Edit(record.clone()));
                    }
                    let confirm = self.confirm.as_ref().is_some_and(|(id, t)| {
                        id == &record.id && t.elapsed() < Duration::from_secs(4)
                    });
                    if small_icon(
                        ui,
                        delete,
                        &assets.history_delete,
                        "delete",
                        if confirm {
                            i18n::tr("再次点击确认删除", "Click again to remove")
                        } else {
                            i18n::tr("删除", "Delete")
                        },
                        !blocked && !record.read_only,
                    ) {
                        if confirm {
                            action = Some(Command::Delete(record.clone()));
                            self.confirm = None;
                        } else {
                            self.confirm = Some((record.id.clone(), Instant::now()));
                            self.status = delete_confirmation_status(&record.name);
                            self.warning = false;
                        }
                    }
                }
                if !ui.input(|i| {
                    i.pointer
                        .latest_pos()
                        .is_some_and(|p| delete.contains(p) || edit.contains(p))
                }) {
                    description_tooltip(&response, &record.description);
                }
            });
        }
        action
    }
    fn market_cards(
        &mut self,
        ui: &mut egui::Ui,
        assets: &GuiAssets,
        blocked: bool,
    ) -> Option<Command> {
        let mut action = None;
        for entry in &self.entries {
            let key = entry.key();
            let installed = self
                .records
                .iter()
                .any(|r| r.origin.as_ref().is_some_and(|o| o.key() == key));
            let expanded = !installed && self.expanded.as_deref() == Some(&key);
            ui.push_id(&key, |ui| {
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(
                        ui.available_width(),
                        if expanded {
                            expanded_card_height()
                        } else {
                            layout::MCP_CARD_HEIGHT
                        },
                    ),
                    Sense::hover(),
                );
                card_surface(ui, rect);
                if expanded {
                    ui.painter().rect_stroke(
                        rect,
                        12,
                        Stroke::new(1.0, Color32::from_rgba_unmultiplied(20, 126, 255, 84)),
                        egui::StrokeKind::Inside,
                    );
                }
                let summary = Rect::from_min_max(
                    rect.min,
                    egui::pos2(
                        rect.right() - if expanded { 42.0 } else { 0.0 },
                        rect.top() + layout::MCP_CARD_HEIGHT,
                    ),
                );
                let mut response = ui
                    .interact(summary, ui.id().with("summary"), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if response.clicked() && !blocked {
                    self.targets.clear();
                    if installed {
                        self.expanded = None;
                        self.status =
                            format!("{} • {}", entry.title, i18n::tr("已安装", "Installed"));
                    } else if expanded {
                        self.expanded = None;
                    } else {
                        self.expanded = Some(key.clone());
                        self.status = format!(
                            "{} • {}",
                            entry.title,
                            i18n::tr(
                                "选择1个或多个目标客户端后安装",
                                "Select one or more target clients to install"
                            )
                        );
                    }
                    self.warning = false;
                }
                mark(
                    ui,
                    Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), Vec2::splat(28.0)),
                    &key,
                    &entry.title,
                );
                let copy_left = 8.0 + 28.0 + layout::MCP_CARD_TEXT_GAP;
                let copy = Rect::from_min_size(
                    rect.min + egui::vec2(copy_left, 7.0),
                    egui::vec2((summary.width() - copy_left - 8.0).max(20.0), 32.0),
                );
                let title_rect = Rect::from_min_size(copy.min, egui::vec2(copy.width(), 16.0));
                let badge = card_title(ui, title_rect, &entry.title, "SKILL", TRANSPORT_TEXT, 18.0);
                let link_rect = Rect::from_center_size(
                    egui::pos2(badge.right() + 11.0, title_rect.center().y),
                    Vec2::splat(14.0),
                );
                let link_response = ui
                    .interact(link_rect, ui.id().with("skill-source-link"), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                icon_at(ui, &assets.link, link_rect.center(), 12.0);
                show_hover_tooltip(&link_response, entry.source_url());
                if link_response.clicked() {
                    open_external_link(ui.ctx(), &entry.source_url());
                }
                ellipsis(
                    ui,
                    Rect::from_min_size(
                        copy.min + egui::vec2(0.0, 16.0),
                        egui::vec2(copy.width(), 14.0),
                    ),
                    &entry.description,
                    9.0,
                    TEXT_MUTED,
                );
                if installed {
                    ui.painter().circle_filled(
                        egui::pos2(
                            rect.right() - 14.0,
                            rect.top() + layout::MCP_CARD_HEIGHT / 2.0,
                        ),
                        3.0,
                        GREEN,
                    );
                }
                if expanded {
                    let install = Rect::from_center_size(
                        egui::pos2(
                            rect.right() - 21.0,
                            rect.top() + layout::MCP_CARD_HEIGHT / 2.0,
                        ),
                        Vec2::splat(26.0),
                    );
                    if install_button(
                        ui,
                        install,
                        &assets.install,
                        !blocked && !self.targets.is_empty(),
                    ) {
                        action = Some(Command::Download(entry.clone(), self.targets.clone()));
                    }
                    let mut child =
                        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(
                            egui::pos2(
                                rect.left() + 8.0,
                                rect.top() + layout::MCP_CARD_HEIGHT + 1.0,
                            ),
                            rect.max - egui::vec2(8.0, 8.0),
                        )));
                    child.spacing_mut().item_spacing = egui::vec2(0.0, 3.0);
                    field_title(&mut child, i18n::tr("目标客户端", "Target clients"));
                    targets(
                        &mut child,
                        &mut self.targets,
                        assets,
                        !blocked,
                        true,
                        "skill-market-targets",
                    );
                    response.rect = rect;
                    response.interact_rect = rect;
                    if !ui.rect_contains_pointer(install) && !ui.rect_contains_pointer(link_rect) {
                        description_tooltip(&response, &entry.description);
                    }
                } else {
                    response.rect = rect;
                    response.interact_rect = rect;
                    if !ui.rect_contains_pointer(link_rect) {
                        description_tooltip(&response, &entry.description);
                    }
                }
            });
        }
        if self.cursor.is_some() && !blocked {
            let (r, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), Sense::hover());
            if button(
                ui,
                r,
                i18n::tr("加载更多", "Load more"),
                "skill-more",
                true,
                false,
                11.0,
            ) {
                action = Some(Command::Market {
                    query: self.query.clone(),
                    cursor: self.cursor.clone(),
                });
            }
        }
        action
    }
    fn form_view(&mut self, ui: &mut egui::Ui, body: Rect, assets: &GuiAssets, blocked: bool) {
        let mut child = scroll_body(ui, body, "skill-form-body");
        let mut action = None;
        let mut cancel = false;
        egui::ScrollArea::vertical().id_salt("skill-form-scroll").auto_shrink([false,false]).max_height(body.height()).show(&mut child,|ui|{
            ui.spacing_mut().item_spacing=egui::vec2(0.0,6.0);
            let form=self.form.as_mut().unwrap();
            if form.original.is_none() && form.origin.is_none() {
                let hint_color = Color32::from_rgb(47, 123, 255);
                let hint_width = ui.available_width();
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(0, 85, 254, 31))
                    .inner_margin(egui::Margin::symmetric(9, 7))
                    .corner_radius(9)
                    .show(ui, |ui| {
                        ui.set_width(hint_width - 18.0);
                        notice_text(ui, i18n::tr(
                                "请选择一个包含 SKILL.md 的目录 / 一个 ZIP 包，或编辑模板里的 SKILL.md 内容来新建 Skill",
                                "Choose a folder containing SKILL.md, a ZIP package, or edit the SKILL.md template to create a Skill",
                            ), hint_color);
                    });
                input(ui,&mut form.path,i18n::tr("本地 Skill 目录或 ZIP 路径", "Local Skill directory or ZIP path"),"skill-import-path");
                let (r,_)=ui.allocate_exact_size(egui::vec2(ui.available_width(),25.0),Sense::hover());
                let browse_width=(r.width()-12.0)/3.0;
                for (index,folder,label) in [(0,true,i18n::tr("选择目录", "Directory")),(1,false,i18n::tr("选择 ZIP", "ZIP file"))] {
                    let rect=Rect::from_min_size(r.min+egui::vec2(index as f32*(browse_width+6.0),0.0),egui::vec2(browse_width,25.0));
                    if button(ui,rect,label,if folder{"skill-folder"}else{"skill-zip"},!blocked,false,11.0){action=Some(Command::Pick(folder));}
                }
                if button(ui,Rect::from_min_size(r.min+egui::vec2(2.0*(browse_width+6.0),0.0),egui::vec2(browse_width,25.0)),i18n::tr("读取路径", "Read path"),"skill-read",!blocked&&!form.path.trim().is_empty(),false,11.0){action=Some(Command::Import(form.path.clone()));}
            }
            if let Some(source)=&form.source {ui.hyperlink_to(source,source);}
            field_title(ui,"SKILL.md");
            let read_only=form.original.as_ref().is_some_and(|r|r.read_only);
            document_editor(ui,&mut form.document,&mut form.height,!read_only);
            if !form.bundle.metadata.compatibility.is_empty(){ui.label(RichText::new(&form.bundle.metadata.compatibility).size(10.0).color(TEXT_MUTED));}
            field_title(ui,i18n::tr("目标客户端", "Target clients"));
            let previous_targets=form.targets.clone();
            targets(ui,&mut form.targets,assets,!blocked&&!read_only,false,"skill-form-targets");
            sync_shared_targets(form.original.as_ref(),&previous_targets,&mut form.targets);
            if form.original.as_ref().is_some_and(|r|r.locations.iter().any(|l|l.targets.len()>1)) {
                ui.label(RichText::new(i18n::tr("共享目录中的 Skill 会同时应用于所列客户端。", "Skills in a shared directory apply to all listed clients.")).size(10.0).color(TEXT_MUTED));
            }
            let claude_desktop_selected = form.targets.contains(&ClientTarget::ClaudeDesktop);
            if read_only || claude_desktop_selected || !form.error.is_empty() {
                let note_width = ui.available_width();
                egui::Frame::new().fill(Color32::from_rgb(58,37,30)).inner_margin(10).corner_radius(10).show(ui,|ui|{
                    ui.set_width(note_width - 20.0);
                    let note_color = Color32::from_rgb(255, 180, 84);
                    if read_only {notice_text(ui,i18n::tr("链接目录为只读；如需修改，请从原始目录重新导入。", "Linked directories are read-only; import the original directory to make changes."),note_color);}
                    if claude_desktop_selected {notice_text(ui,i18n::tr("Claude Desktop：ZIP 导出至下载文件夹的 Little-Mungbean-Skills，请在 Customize > Skills 手动导入。技能的依赖与账号要求请阅读 SKILL.md。", "Claude Desktop: exports a ZIP to Downloads/Little-Mungbean-Skills for manual import in Customize > Skills. Read SKILL.md for dependencies and account requirements."),note_color);}
                    if !form.error.is_empty(){notice_text(ui,&form.error,note_color);}
                });
            }
            let (r,_)=ui.allocate_exact_size(egui::vec2(ui.available_width(),25.0),Sense::hover());
            let save=Rect::from_min_size(egui::pos2(r.right()-48.0,r.top()),egui::vec2(48.0,25.0));
            if button(ui,save.translate(egui::vec2(-54.0,0.0)),i18n::tr("取消", "Cancel"),"skill-cancel",!blocked,false,11.0){cancel=true;}
            else if button(ui,save,i18n::tr("保存", "Save"),"skill-save",!blocked&&!read_only,true,11.0){action=Some(Command::Save(Box::new(form.clone())));}
        });
        if cancel {
            self.form = None;
        }
        if let Some(action) = action {
            self.start(ui.ctx(), action);
        }
        if self.form.is_none() {
            self.list_status();
        }
    }
}
fn notice_text(ui: &mut egui::Ui, value: &str, color: Color32) {
    let mut job = egui::text::LayoutJob::simple(
        value.to_owned(),
        FontId::proportional(10.0),
        color,
        ui.available_width(),
    );
    for section in &mut job.sections {
        section.format.line_height = Some(15.0);
    }
    ui.label(job);
}
fn local_match(record: &skills::Record, query: &str) -> bool {
    let query = query.to_lowercase();
    query.split_whitespace().all(|word| {
        format!("{} {}", record.name, record.description)
            .to_lowercase()
            .contains(word)
    })
}

fn skill_registry_picker(ui: &mut egui::Ui, rect: Rect, assets: &GuiAssets) {
    let response = ui
        .interact(rect, ui.id().with("skill-registry"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let popup = egui::Popup::menu(&response)
        .align(egui::RectAlign::BOTTOM_END)
        .gap(6.0)
        .width(124.0)
        .frame(
            egui::Frame::new()
                .fill(Color32::from_rgb(48, 48, 48))
                .inner_margin(5)
                .corner_radius(12)
                .stroke(Stroke::NONE),
        );
    let open = popup.is_open();
    // Keep the full toolbar hit area stable while the visible surface is 26px high.
    ui.painter().rect_filled(rect, 9, Color32::TRANSPARENT);
    let background = Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width(), layout::MCP_TOOLBAR_CONTROL_HEIGHT),
    );
    ui.painter().rect_filled(background, 9, ROW);
    ellipsis(
        ui,
        Rect::from_min_max(
            rect.min + egui::vec2(8.0, 0.0),
            rect.max - egui::vec2(24.0, 0.0),
        ),
        "ClawHub",
        11.0,
        TEXT_MUTED,
    );
    let center = egui::pos2(rect.right() - 14.0, rect.center().y);
    let mut arrow = egui::epaint::Mesh::with_texture(assets.history_expand.id());
    arrow.add_rect_with_uv(
        Rect::from_center_size(center, Vec2::splat(12.0)),
        Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    if open {
        arrow.rotate(egui::emath::Rot2::from_angle(std::f32::consts::PI), center);
    }
    ui.painter().add(egui::Shape::mesh(arrow));
    popup.show(|ui| {
        ui.set_width(114.0);
        let (option, option_response) =
            ui.allocate_exact_size(egui::vec2(114.0, 26.0), Sense::click());
        ui.painter().rect_filled(
            option,
            9,
            if option_response.hovered() {
                ORANGE
            } else {
                ACTIVE
            },
        );
        ellipsis(
            ui,
            option.shrink2(egui::vec2(8.0, 0.0)),
            "ClawHub",
            11.0,
            if option_response.hovered() {
                TEXT_MAIN
            } else {
                ACCENT
            },
        );
        paint_surface_outline(ui.painter(), option.expand(5.0), 12);
        if option_response.clicked() {
            ui.close();
        }
    });
}
fn sync_shared_targets(
    original: Option<&skills::Record>,
    before: &[ClientTarget],
    selected: &mut Vec<ClientTarget>,
) {
    let Some(record) = original else {
        return;
    };
    let Some(changed) = ClientTarget::ALL
        .into_iter()
        .find(|t| before.contains(t) != selected.contains(t))
    else {
        return;
    };
    let enabled = selected.contains(&changed);
    for location in record
        .locations
        .iter()
        .filter(|l| l.targets.len() > 1 && l.targets.contains(&changed))
    {
        for target in &location.targets {
            if enabled {
                if !selected.contains(target) {
                    selected.push(*target);
                }
            } else {
                selected.retain(|t| t != target);
            }
        }
    }
}
fn document_editor(ui: &mut egui::Ui, document: &mut String, height: &mut f32, enabled: bool) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), *height), Sense::hover());
    ui.painter().rect_filled(rect, 10, ROW);
    let inner = Rect::from_min_max(
        rect.min + egui::vec2(8.0, 5.0),
        rect.max - egui::vec2(8.0, 17.0),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(inner.intersect(ui.clip_rect()));
    let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap_width: f32| {
        ui.painter()
            .layout_no_wrap(text.as_str().to_owned(), FontId::monospace(12.0), TEXT_MAIN)
    };
    let text_width = layouter(&child, document, f32::INFINITY).size().x;
    let viewport_width = inner.width() - child.spacing().scroll.content_margin.sum().x;
    egui::ScrollArea::both()
        .hscroll(text_width > viewport_width)
        .id_salt("skill-document-scroll")
        .max_height(inner.height())
        .auto_shrink([false, false])
        .show(&mut child, |ui| {
            let output = egui::TextEdit::multiline(&mut *document)
                .interactive(enabled)
                .id_salt("skill-document")
                .code_editor()
                .font(FontId::monospace(12.0))
                .layouter(&mut layouter)
                .desired_width(f32::INFINITY)
                .desired_rows(1)
                .frame(egui::Frame::NONE)
                .text_color(TEXT_MAIN)
                .show(ui);
            if enabled {
                super::text_menu::attach(
                    ui,
                    output,
                    document,
                    super::text_menu::Options {
                        multiline: true,
                        password: false,
                    },
                );
            }
        });
    let handle = Rect::from_min_size(rect.right_bottom() - Vec2::splat(20.0), Vec2::splat(20.0));
    let response = ui
        .interact(handle, ui.id().with("skill-document-resize"), Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if response.dragged() {
        *height = (*height + ui.input(|i| i.pointer.delta().y)).clamp(120.0, 1200.0);
    }
    for d in [0.0, 4.0] {
        ui.painter().line_segment(
            [
                rect.right_bottom() + egui::vec2(-12.0 + d, -5.0),
                rect.right_bottom() + egui::vec2(-5.0, -12.0 + d),
            ],
            Stroke::new(1.0, TEXT_MUTED),
        );
    }
}
