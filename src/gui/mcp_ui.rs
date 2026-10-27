//! Native MCP view. Measurements follow the approved Website prototype.
use super::*;
use crate::{mcp, mcp_registry};
use std::sync::mpsc::{self, Receiver, TryRecvError};

pub(super) const PANEL: Color32 = Color32::from_rgb(43, 43, 43);
pub(super) const ROW: Color32 = Color32::from_rgb(32, 32, 32);
pub(super) const ACTIVE: Color32 = Color32::from_rgb(35, 47, 116);
pub(super) const ACCENT: Color32 = Color32::from_rgb(71, 160, 255);
pub(super) const GREEN: Color32 = Color32::from_rgb(0, 214, 117);
pub(super) const TRANSPORT_TEXT: Color32 = Color32::from_rgb(193, 159, 255);
const CARD_TITLE_TEXT: Color32 = Color32::from_rgb(225, 229, 235);

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct Form {
    id: Option<String>,
    original: Option<mcp::Server>,
    name: String,
    description: String,
    json: String,
    targets: Vec<ClientTarget>,
    registry_name: Option<String>,
    installation_name: Option<String>,
    note: String,
    error: String,
    json_height: f32,
}
impl Default for Form {
    fn default() -> Self {
        Self {
            id: None,
            original: None,
            name: String::new(),
            description: String::new(),
            json: "{\n  \"command\": \"npx\",\n  \"args\": [\"-y\", \"your-mcp-server\"]\n}".into(),
            targets: vec![],
            registry_name: None,
            installation_name: None,
            note: String::new(),
            error: String::new(),
            json_height: 180.0,
        }
    }
}
impl Form {
    fn read_only(&self) -> bool {
        self.original
            .as_ref()
            .is_some_and(mcp::Server::is_codex_builtin)
    }

    fn status_message(&self, zh: &'static str, en: &'static str) -> String {
        let name = self.name.trim();
        let prefix = if name.is_empty() {
            self.installation_name.as_deref().unwrap_or("MCP Server")
        } else {
            name
        };
        format!("{prefix} • {}", i18n::tr(zh, en))
    }
}
enum Command {
    Local,
    Market {
        cursor: Option<String>,
        query: String,
        continuation_query: Option<String>,
        searching: bool,
    },
    Save(Box<Form>),
    Toggle(mcp::Server),
    Delete(mcp::Server),
}
struct Reply {
    records: Option<Vec<mcp::Server>>,
    page: Option<mcp_registry::Page>,
    append: bool,
    message: String,
    warning: String,
    saved: bool,
}

#[derive(Default)]
pub struct State {
    pub open: bool,
    market: bool,
    form: Option<Form>,
    records: Vec<mcp::Server>,
    entries: Vec<mcp_registry::Entry>,
    cursor: Option<String>,
    market_loaded: bool,
    search_open: bool,
    search_focus: bool,
    search_text: String,
    search_query: String,
    market_search_query: Option<String>,
    expanded: Option<String>,
    targets: Vec<ClientTarget>,
    delete_confirm: Option<(String, Instant)>,
    job: Option<Receiver<std::result::Result<Reply, String>>>,
    job_is_market: bool,
    job_is_save: bool,
    market_searching: bool,
    job_query: Option<String>,
    writing: bool,
    pub status: String,
    pub warning: bool,
    error: String,
}
impl State {
    fn empty_search_status(&mut self) {
        if self.search_query.is_empty() || !self.error.is_empty() {
            return;
        }
        let no_matches = if self.market {
            self.market_loaded && self.entries.is_empty() && self.cursor.is_none()
        } else {
            !self
                .records
                .iter()
                .any(|server| matches_local_search(server, &self.search_query))
        };
        if no_matches {
            self.warning = true;
            self.status = format!(
                "MCP Server • {} • {}",
                if self.market {
                    i18n::tr("市场", "Market")
                } else {
                    i18n::tr("本地", "Local")
                },
                i18n::tr("未找到匹配的 MCP Server", "No matching MCP Servers"),
            );
        }
    }

    fn list_status(&mut self) {
        self.warning = false;
        self.status = format!(
            "MCP Server • {}",
            if self.market {
                i18n::tr("市场", "Market")
            } else {
                i18n::tr("本地", "Local")
            }
        );
        if self.market && self.job_is_market && self.job.is_some() {
            self.status = market_progress(self.market_searching).into();
        } else {
            self.empty_search_status();
        }
    }

    fn fetch_market(&mut self, ctx: &egui::Context, cursor: Option<String>, searching: bool) {
        let continuation_query = cursor.as_ref().and(self.market_search_query.clone());
        self.start(
            ctx,
            Command::Market {
                cursor,
                query: self.search_query.clone(),
                continuation_query,
                searching,
            },
        );
    }

    fn submit_search(&mut self, ctx: &egui::Context) {
        let query = self.search_text.trim().to_owned();
        if self.search_query == query {
            if self.market && !self.market_loaded {
                self.fetch_market(ctx, None, !query.is_empty());
            } else {
                self.list_status();
            }
            return;
        }
        self.search_query = query;
        self.market_search_query = None;
        self.expanded = None;
        self.targets.clear();
        self.error.clear();
        self.warning = false;
        self.entries.clear();
        self.cursor = None;
        self.market_loaded = false;
        if self.market {
            self.fetch_market(ctx, None, !self.search_query.is_empty());
        } else {
            self.list_status();
        }
    }

    fn search_controls(&mut self, ui: &mut egui::Ui, heading: Rect, assets: &GuiAssets) {
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
                    .id_salt("mcp-search-input")
                    .hint_text(
                        RichText::new(i18n::tr("搜索 MCP Server", "Search MCP Server"))
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
                let r = ui.interact(clear, ui.id().with("mcp-search-clear"), Sense::click());
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
                    self.submit_search(ui.ctx());
                    response.request_focus();
                }
            }
            if escape {
                self.search_open = false;
                self.search_text.clear();
                self.submit_search(ui.ctx());
            }
        }
        let response = ui.interact(search, ui.id().with("mcp-search"), Sense::click());
        icon_at(ui, &assets.search, search.center(), 13.0);
        show_hover_tooltip(&response, i18n::tr("搜索", "Search"));
        if response.clicked() {
            if self.search_open {
                if self.search_text.trim().is_empty() {
                    self.search_open = false;
                    self.search_focus = false;
                    self.search_text.clear();
                    submit = !self.search_query.is_empty();
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
            self.submit_search(ui.ctx());
        }
    }

    pub fn writing(&self) -> bool {
        self.writing
    }
    fn close_form(&mut self) {
        self.form = None;
        self.error.clear();
        self.warning = false;
        self.status = "MCP Server".into();
    }
    pub fn show(&mut self, ctx: &egui::Context) {
        self.open = true;
        self.search_open = false;
        self.search_text.clear();
        self.market_search_query = None;
        if !self.search_query.is_empty() {
            self.search_query.clear();
            self.entries.clear();
            self.cursor = None;
            self.market_loaded = false;
        }
        self.status = "MCP Server".into();
        self.warning = false;
        if self.job.is_none() {
            self.start(ctx, Command::Local);
        }
    }
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<(String, bool)> {
        let result = match self.job.as_ref()?.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(60));
                return None;
            }
            Err(TryRecvError::Disconnected) => Err(i18n::tr(
                "MCP 后台任务意外结束",
                "MCP background task ended unexpectedly",
            )
            .into()),
        };
        self.job = None;
        let was_writing = std::mem::take(&mut self.writing);
        let market_request = std::mem::take(&mut self.job_is_market);
        let form_request = std::mem::take(&mut self.job_is_save);
        // A result belongs to the submitted query. Never let an older request
        // overwrite a newer search or the unfiltered list after clearing.
        if self
            .job_query
            .take()
            .is_some_and(|query| query != self.search_query)
        {
            self.market_loaded = false;
            if self.open && self.market && self.form.is_none() {
                self.fetch_market(ctx, None, self.market_searching);
            } else if self.open && self.form.is_none() {
                self.list_status();
            }
            return None;
        }
        match result {
            Ok(reply) => {
                if let Some(records) = reply.records {
                    self.records = records;
                }
                if let Some(page) = reply.page {
                    if !reply.append {
                        self.entries.clear();
                    }
                    for entry in page.entries {
                        if !self.entries.iter().any(|e| e.name == entry.name) {
                            self.entries.push(entry);
                        }
                    }
                    self.cursor = page.cursor;
                    self.market_search_query = page.search_query;
                    self.market_loaded = true;
                }
                if reply.saved {
                    self.form = None;
                    self.expanded = None;
                    self.targets.clear();
                }
                self.warning = !reply.warning.is_empty();
                self.error = reply.warning;
                self.status = reply.message;
                if self.form.is_none() && !was_writing {
                    if market_request && !self.market {
                        // A background registry response must not replace the
                        // local view's search feedback.
                        self.list_status();
                    } else {
                        self.empty_search_status();
                    }
                }
                let event = Some((
                    if !market_request && !was_writing && self.error.is_empty() {
                        // A successful local scan updates the status without adding a log section.
                        String::new()
                    } else if self.error.is_empty() {
                        self.status.clone()
                    } else {
                        format!("{}\n{}", self.status, self.error)
                    },
                    self.warning,
                ));
                if self.open && self.market && !self.market_loaded && self.form.is_none() {
                    self.fetch_market(ctx, None, self.market_searching);
                }
                event
            }
            Err(error) => {
                if let Some(form) = &mut self.form {
                    form.error = error.clone();
                }
                // Registry/network failures belong in the runtime log. Keep the
                // MCP list usable instead of rendering a large error block in it.
                self.error = if market_request || form_request {
                    String::new()
                } else {
                    error.clone()
                };
                self.warning = !form_request || self.form.is_some();
                if form_request && self.form.is_none() {
                    self.status = "MCP Server".into();
                    return Some((error, self.warning));
                }
                self.status = i18n::tr(
                    if market_request {
                        "MCP Server • 市场加载失败 • 查看运行日志"
                    } else {
                        "MCP Server • 操作失败 • 查看运行日志"
                    },
                    if market_request {
                        "MCP Server • Registry failed • See runtime log"
                    } else {
                        "MCP Server • Failed • See runtime log"
                    },
                )
                .into();
                Some((error, true))
            }
        }
    }
    fn start(&mut self, ctx: &egui::Context, command: Command) {
        if let Command::Market { searching, .. } = &command {
            self.market_searching = *searching;
            if self.market {
                self.warning = false;
                self.status = market_progress(*searching).into();
            }
        }
        if self.job.is_some() {
            return;
        }
        self.job_is_market = matches!(&command, Command::Market { .. });
        self.job_is_save = matches!(&command, Command::Save(_));
        self.job_query = match &command {
            Command::Market { query, .. } => Some(query.clone()),
            _ => None,
        };
        self.writing = matches!(
            &command,
            Command::Save(_) | Command::Toggle(_) | Command::Delete(_)
        );
        self.warning = false;
        self.error.clear();
        let progress = match &command {
            Command::Local => Some(i18n::tr(
                "MCP Server • 正在读取本地配置…",
                "MCP Server • Reading local configuration…",
            )),
            Command::Market { searching, .. } => Some(market_progress(*searching)),
            _ => None,
        };
        if let Some(progress) = progress {
            self.status = progress.into();
        }
        let (sender, receiver) = mpsc::channel();
        self.job = Some(receiver);
        let context = ctx.clone();
        std::thread::spawn(move || {
            let result: Result<Reply> = (|| {
                if let Command::Market {
                    cursor,
                    query,
                    continuation_query,
                    ..
                } = command
                {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    let page = if query.is_empty() {
                        runtime.block_on(mcp_registry::fetch(cursor.as_deref()))?
                    } else {
                        runtime.block_on(mcp_registry::fetch_search(
                            cursor.as_deref(),
                            &query,
                            continuation_query.as_deref(),
                        ))?
                    };
                    let count = page.entries.len();
                    return Ok(Reply {
                        records: None,
                        page: Some(page),
                        append: cursor.is_some(),
                        message: format!(
                            "MCP Server • {} • {count}",
                            i18n::tr("市场已更新", "Registry updated")
                        ),
                        warning: String::new(),
                        saved: false,
                    });
                }
                let mut repository = mcp::Repository::discover()?;
                let mut saved = false;
                let message = match command {
                    Command::Local => format!(
                        "MCP Server • {} • {}",
                        i18n::tr("本地", "Local"),
                        repository.records.len()
                    ),
                    Command::Save(form) => {
                        let form = *form;
                        if let Some(expected) = &form.original {
                            repository.ensure_unchanged(expected)?;
                        }
                        repository.save(
                            form.id.as_deref(),
                            &form.name,
                            &form.description,
                            serde_json::from_str(&form.json)?,
                            &form.targets,
                            form.registry_name,
                        )?;
                        saved = true;
                        format!(
                            "{} • {}",
                            form.installation_name.as_deref().unwrap_or(&form.name),
                            if form.installation_name.is_some() {
                                i18n::tr(
                                    "已安装 • 请在客户端重新加载",
                                    "Installed • Reload in the client",
                                )
                            } else {
                                i18n::tr(
                                    "已保存 • 请在客户端重新加载",
                                    "Saved • Reload in the client",
                                )
                            }
                        )
                    }
                    Command::Toggle(server) => {
                        repository.ensure_unchanged(&server)?;
                        repository.toggle(&server.id)?;
                        format!(
                            "{} • {} • {}",
                            server.name,
                            if server.enabled() {
                                i18n::tr("已停用", "Disabled")
                            } else {
                                i18n::tr("已启用", "Enabled")
                            },
                            i18n::tr("请在客户端重新加载", "Reload in the client"),
                        )
                    }
                    Command::Delete(server) => {
                        repository.ensure_unchanged(&server)?;
                        repository.delete(&server.id)?;
                        i18n::tr(
                            "MCP Server • 已删除 • 请在客户端重新加载",
                            "MCP Server • Removed • Reload in the client",
                        )
                        .into()
                    }
                    Command::Market { .. } => unreachable!(),
                };
                Ok(Reply {
                    records: Some(repository.records),
                    page: None,
                    append: false,
                    message,
                    warning: repository.warnings.join("\n"),
                    saved,
                })
            })();
            let _ = sender.send(result.map_err(|e| format!("{e:#}")));
            context.request_repaint();
        });
        ctx.request_repaint();
    }
    pub fn render(
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
        if self.form.is_some() || !self.search_open {
            section_title(
                ui,
                heading,
                if let Some(form) = &self.form {
                    if form.read_only() {
                        i18n::tr("查看 MCP Server", "View MCP Server")
                    } else if form.id.is_some() {
                        i18n::tr("编辑 MCP Server", "Edit MCP Server")
                    } else {
                        i18n::tr("添加 MCP Server", "Add MCP Server")
                    }
                } else {
                    "MCP Server"
                },
                12.0,
                false,
            );
        }
        let back = Rect::from_center_size(
            egui::pos2(heading.left() + 8.0, heading.center().y),
            Vec2::splat(layout::PANEL_BACK_SIZE),
        );
        if back_button(ui, back, "mcp-back") {
            if self.form.is_some() {
                self.close_form();
            } else {
                self.open = false;
            }
        }
        let busy = self.job.is_some();
        let blocked = busy || clients_busy;
        if self.form.is_none() {
            self.search_controls(ui, heading, assets);
            let refresh_rect = Rect::from_center_size(
                egui::pos2(heading.right() - 8.0, heading.center().y),
                Vec2::splat(22.0),
            );
            if refresh_button(
                ui,
                refresh_rect,
                &assets.refresh,
                busy,
                busy && !self.writing,
            ) {
                self.expanded = None;
                self.targets.clear();
                if self.market {
                    self.fetch_market(ui.ctx(), None, false);
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
            let tab_width = (tabs.width() - 10.0) / 2.0;
            let amount = ui.ctx().animate_bool_with_time(
                ui.id().with("mcp-market-slide"),
                self.market,
                0.18,
            );
            ui.painter().rect_filled(
                Rect::from_min_size(
                    tabs.min + egui::vec2(4.0 + amount * (tab_width + 2.0), 4.0),
                    egui::vec2(tab_width, 22.0),
                ),
                6,
                ACTIVE,
            );
            for (market, label) in [
                (false, i18n::tr("本地", "Local")),
                (true, i18n::tr("市场", "Market")),
            ] {
                let tab = Rect::from_min_size(
                    tabs.min + egui::vec2(4.0 + f32::from(market) * (tab_width + 2.0), 4.0),
                    egui::vec2(tab_width, 22.0),
                );
                let r = ui.interact(tab, ui.id().with(("mcp-tab", market)), Sense::click());
                text(
                    ui,
                    tab,
                    label,
                    11.0,
                    if self.market == market {
                        ACCENT
                    } else {
                        TEXT_MUTED
                    },
                );
                if r.clicked() {
                    self.market = market;
                    self.expanded = None;
                    self.targets.clear();
                    self.list_status();
                    if market && !self.market_loaded && !busy {
                        if self.search_query.is_empty()
                            && let Some(page) = mcp_registry::load_cache()
                        {
                            self.entries = page.entries;
                            self.cursor = page.cursor;
                        }
                        self.fetch_market(ui.ctx(), None, false);
                    }
                }
            }
            if self.market {
                let picker = Rect::from_min_size(
                    egui::pos2(toolbar.right() - 118.0, toolbar.top()),
                    egui::vec2(118.0, 30.0),
                );
                registry_picker(ui, picker, assets);
            } else {
                let add = Rect::from_min_size(
                    egui::pos2(
                        toolbar.right()
                            - if i18n::language() == i18n::Language::ZhCn {
                                52.0
                            } else {
                                56.0
                            },
                        toolbar.top(),
                    ),
                    egui::vec2(
                        if i18n::language() == i18n::Language::ZhCn {
                            52.0
                        } else {
                            56.0
                        },
                        30.0,
                    ),
                );
                // Configuration writes keep the toolbar steady; clicks still
                // wait for the current operation to finish.
                if add_button(ui, add, !clients_busy && (!busy || self.writing), busy) {
                    self.form = Some(Form::default());
                    self.status = i18n::tr("MCP Server • 添加", "MCP Server • Add").into();
                }
            }
            let body =
                Rect::from_min_max(egui::pos2(inner.left(), toolbar.bottom() + 10.0), inner.max);
            let mut child = scroll_body(ui, body, "mcp-list-body");
            let mut command = None;
            egui::ScrollArea::vertical()
                .id_salt(("mcp-list", self.market))
                .auto_shrink([false, false])
                .max_height(body.height())
                .show(&mut child, |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 6.0);
                    if !self.error.is_empty() && !self.market {
                        ui.label(
                            RichText::new(&self.error)
                                .size(10.0)
                                .color(Color32::from_rgb(255, 179, 107)),
                        );
                    }
                    if self.market {
                        self.market_cards(ui, assets, blocked);
                    } else {
                        command = self.local_cards(ui, assets, blocked);
                    }
                });
            if let Some(command) = command {
                self.start(ui.ctx(), command);
            }
        } else {
            let body =
                Rect::from_min_max(egui::pos2(inner.left(), heading.bottom() + 8.0), inner.max);
            let mut child = scroll_body(ui, body, "mcp-form-body");
            let mut action = 0;
            egui::ScrollArea::vertical()
                .id_salt("mcp-form-scroll")
                .auto_shrink([false, false])
                .max_height(body.height())
                .show(&mut child, |ui| {
                    let form = self.form.as_mut().unwrap();
                    let read_only = form.read_only();
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 6.0);
                    if read_only {
                        ui.add(
                            egui::Label::new(RichText::new(&form.name).size(11.0).color(TEXT_MAIN))
                                .selectable(true),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(&form.description)
                                    .size(10.0)
                                    .color(TEXT_MUTED),
                            )
                            .wrap(),
                        );
                    } else {
                        input(
                            ui,
                            &mut form.name,
                            i18n::tr("名称/ID : My-MCP-Server", "Name/ID : My-MCP-Server"),
                            "mcp-name",
                        );
                        input(
                            ui,
                            &mut form.description,
                            i18n::tr(
                                "描述 (选填) : 简要描述此 MCP Server 的用途",
                                "Description : Briefly describe this MCP Server",
                            ),
                            "mcp-description",
                        );
                    }
                    field_title(ui, i18n::tr("JSON 配置", "JSON configuration"));
                    if json_editor(ui, form) {
                        form.error.clear();
                        self.warning = false;
                    }
                    field_title(ui, i18n::tr("目标客户端", "Target clients"));
                    targets(
                        ui,
                        &mut form.targets,
                        assets,
                        !blocked && !read_only,
                        false,
                        "form-targets",
                    );
                    if !read_only {
                        form_help(ui, form);
                    }
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 25.0),
                        Sense::hover(),
                    );
                    let save = Rect::from_min_size(
                        egui::pos2(r.right() - 48.0, r.top()),
                        egui::vec2(48.0, 25.0),
                    );
                    let cancel = if read_only {
                        save
                    } else {
                        save.translate(egui::vec2(-54.0, 0.0))
                    };
                    if button(
                        ui,
                        cancel,
                        if read_only {
                            i18n::tr("返回", "Back")
                        } else {
                            i18n::tr("取消", "Cancel")
                        },
                        "mcp-cancel",
                        !busy,
                        false,
                        11.0,
                    ) {
                        action = 1;
                    }
                    if !read_only
                        && button(
                            ui,
                            save,
                            i18n::tr("保存", "Save"),
                            "mcp-save",
                            !blocked,
                            true,
                            11.0,
                        )
                    {
                        action = 2;
                    }
                });
            if action == 1 {
                self.close_form();
            }
            if action == 2 {
                let form = self.form.as_mut().unwrap();
                let check = (|| -> Result<()> {
                    let v = serde_json::from_str(&form.json)
                        .context(i18n::tr("JSON 格式无效", "Invalid JSON"))?;
                    mcp::validate_config(v)?;
                    if form.targets.is_empty() {
                        bail!(i18n::tr(
                            "请选择至少一个目标客户端",
                            "Select at least one target client"
                        ));
                    }
                    if form.name.trim().is_empty() {
                        bail!(i18n::tr("请填写名称/ID", "Enter a name/ID"));
                    }
                    Ok(())
                })();
                if let Err(error) = check {
                    form.error = format!("{error:#}");
                    self.warning = true;
                    self.status = form.status_message("请检查输入内容", "Check your input");
                } else {
                    let form = form.clone();
                    self.start(ui.ctx(), Command::Save(Box::new(form)));
                }
            }
        }
        if clients_busy && !busy {
            show_hover_tooltip(
                &ui.interact(heading, ui.id().with("mcp-client-busy"), Sense::hover()),
                i18n::tr(
                    "客户端正在切换配置，请完成后再修改 MCP",
                    "Wait for the client configuration change before editing MCP",
                ),
            );
        }
    }

    fn local_cards(
        &mut self,
        ui: &mut egui::Ui,
        assets: &GuiAssets,
        blocked: bool,
    ) -> Option<Command> {
        if self.records.is_empty() && self.search_query.is_empty() {
            ui.label(
                RichText::new(i18n::tr(
                    "未发现本地 MCP Server，点击添加或从市场安装。",
                    "No local MCP Servers. Add one or install from the registry.",
                ))
                .size(11.0)
                .color(TEXT_MUTED),
            );
        }
        let mut command = None;
        let mut edit = None;
        for server in &self.records {
            if !matches_local_search(server, &self.search_query) {
                continue;
            }
            ui.push_id(&server.id, |ui| {
                let read_only = server.is_codex_builtin();
                let description = server.display_description();
                let client_targets = server.targets();
                let card_height = local_card_height(&client_targets);
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), card_height),
                    Sense::hover(),
                );
                card_surface(ui, rect);
                let copy = Rect::from_min_max(
                    rect.min + egui::vec2(8.0 + 28.0 + layout::MCP_CARD_TEXT_GAP, 7.0),
                    rect.max - egui::vec2(41.0, 7.0),
                );
                mark(
                    ui,
                    Rect::from_center_size(
                        egui::pos2(rect.left() + 22.0, rect.center().y),
                        Vec2::splat(28.0),
                    ),
                    &server.id,
                    &server.name,
                );
                let hovered = response.hovered()
                    || ui.input(|i| i.pointer.latest_pos().is_some_and(|p| rect.contains(p)));
                let title = Rect::from_min_size(copy.min, egui::vec2(copy.width(), 16.0));
                let delete = Rect::from_min_size(
                    egui::pos2(title.right() - 16.0, title.top()),
                    Vec2::splat(16.0),
                );
                let editing = delete.translate(egui::vec2(-18.0, 0.0));
                card_title(
                    ui,
                    title,
                    &server.name,
                    server.transport(),
                    TRANSPORT_TEXT,
                    if hovered && !read_only { 36.0 } else { 0.0 },
                );
                ellipsis(
                    ui,
                    Rect::from_min_size(
                        egui::pos2(copy.left(), copy.top() + 16.0),
                        egui::vec2(copy.width(), 14.0),
                    ),
                    if description.is_empty() {
                        i18n::tr("未填写描述", "No description")
                    } else {
                        description
                    },
                    9.0,
                    Color32::from_rgb(134, 142, 154),
                );
                if !client_targets.is_empty() {
                    client_target_tags(ui, copy.left(), copy.top() + 32.0, &client_targets);
                }
                let toggle = Rect::from_center_size(
                    egui::pos2(rect.right() - 20.0, rect.center().y),
                    egui::vec2(24.0, 14.0),
                );
                if read_only {
                    if small_icon(
                        ui,
                        Rect::from_center_size(toggle.center(), Vec2::splat(18.0)),
                        &assets.eye_show,
                        "view",
                        i18n::tr("查看配置（只读）", "View configuration (read-only)"),
                        true,
                    ) {
                        edit = Some(server.clone());
                    }
                } else if toggle_button(ui, toggle, server.enabled(), !blocked) {
                    command = Some(Command::Toggle(server.clone()));
                }
                if hovered && !read_only {
                    if small_icon(
                        ui,
                        editing,
                        &assets.history_edit,
                        "edit",
                        i18n::tr("编辑", "Edit"),
                        !blocked,
                    ) {
                        edit = Some(server.clone());
                    }
                    let confirm = self.delete_confirm.as_ref().is_some_and(|(id, time)| {
                        id == &server.id && time.elapsed() < Duration::from_secs(4)
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
                        !blocked,
                    ) {
                        if confirm {
                            command = Some(Command::Delete(server.clone()));
                            self.delete_confirm = None;
                        } else {
                            self.delete_confirm = Some((server.id.clone(), Instant::now()));
                            self.status = delete_confirmation_status("MCP Server");
                            self.warning = false;
                        }
                    }
                }
                let pointer_over_action = ui.input(|input| {
                    input
                        .pointer
                        .latest_pos()
                        .is_some_and(|pos| editing.contains(pos) || delete.contains(pos))
                });
                if read_only || !pointer_over_action {
                    description_tooltip(&response, description);
                }
            });
        }
        if let Some(server) = edit {
            let targets = server.targets();
            let read_only = server.is_codex_builtin();
            let description = server.display_description().to_owned();
            self.form = Some(Form {
                original: Some(server.clone()),
                id: Some(server.id),
                name: server.name,
                description,
                json: serde_json::to_string_pretty(&server.config).unwrap_or_default(),
                targets,
                registry_name: server.registry_name,
                installation_name: None,
                note: String::new(),
                error: String::new(),
                json_height: 180.0,
            });
            self.status = if read_only {
                i18n::tr(
                    "MCP Server • 查看配置（只读）",
                    "MCP Server • View configuration (read-only)",
                )
            } else {
                i18n::tr("MCP Server • 编辑", "MCP Server • Edit")
            }
            .into();
        }
        command
    }
    fn market_cards(&mut self, ui: &mut egui::Ui, assets: &GuiAssets, blocked: bool) {
        let mut install = None;
        for entry in &self.entries {
            let installed = self
                .records
                .iter()
                .any(|r| r.registry_name.as_deref() == Some(&entry.name));
            let expanded = !installed && self.expanded.as_deref() == Some(&entry.name);
            ui.push_id(&entry.name, |ui| {
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
                let install_size = install_button_size(ui);
                let summary = Rect::from_min_max(
                    rect.min,
                    egui::pos2(
                        rect.right()
                            - if expanded {
                                install_size.x + 17.0
                            } else {
                                14.0
                            },
                        rect.top() + layout::MCP_CARD_HEIGHT,
                    ),
                );
                let summary_response = ui
                    .interact(summary, ui.id().with("summary"), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                let mut clicked = summary_response.clicked();
                // The clickable summary owns hover, rather than the card's
                // background response. Keep the tooltip anchored to the card.
                let mut description_response = summary_response;
                description_response.rect = rect;
                description_response.interact_rect = rect;
                let install_rect = expanded.then(|| {
                    Rect::from_center_size(
                        egui::pos2(
                            rect.right() - layout::MCP_CARD_PADDING - install_size.x / 2.0,
                            rect.top() + layout::MCP_CARD_HEIGHT / 2.0,
                        ),
                        install_size,
                    )
                });
                mark(
                    ui,
                    Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), Vec2::splat(28.0)),
                    &entry.name,
                    &entry.title,
                );
                let copy_left = 8.0 + 28.0 + layout::MCP_CARD_TEXT_GAP;
                let copy = Rect::from_min_size(
                    rect.min + egui::vec2(copy_left, 7.0),
                    egui::vec2((summary.width() - copy_left - 8.0).max(30.0), 32.0),
                );
                let title_rect = Rect::from_min_size(copy.min, egui::vec2(copy.width(), 16.0));
                let website_url = entry.website_url.as_deref();
                let badge = card_title(
                    ui,
                    title_rect,
                    &entry.title,
                    entry.transport(),
                    TRANSPORT_TEXT,
                    if website_url.is_some() { 18.0 } else { 0.0 },
                );
                let mut website_hovered = false;
                if let Some(url) = website_url {
                    let link_rect = Rect::from_center_size(
                        egui::pos2(badge.right() + 11.0, title_rect.center().y),
                        Vec2::splat(14.0),
                    );
                    let link_response = ui
                        .interact(link_rect, ui.id().with("mcp-website-link"), Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    icon_at(ui, &assets.link, link_rect.center(), 12.0);
                    show_hover_tooltip(&link_response, url);
                    website_hovered = link_response.hovered();
                    if link_response.clicked() {
                        clicked = false;
                        open_external_link(ui.ctx(), url);
                    }
                }
                ellipsis(
                    ui,
                    Rect::from_min_size(
                        egui::pos2(copy.left(), copy.top() + 16.0),
                        egui::vec2(copy.width(), 14.0),
                    ),
                    &entry.description,
                    9.0,
                    Color32::from_rgb(134, 142, 154),
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
                if clicked {
                    let was_expanded = self.expanded.as_deref() == Some(&entry.name);
                    self.expanded = None;
                    self.targets.clear();
                    if installed {
                        self.status =
                            format!("{} • {}", entry.title, i18n::tr("已安装", "Installed"));
                    } else if !was_expanded {
                        self.expanded = Some(entry.name.clone());
                        self.status = format!(
                            "{} • {}",
                            entry.title,
                            i18n::tr(
                                "选择1个或多个目标客户端后安装",
                                "Select one or more target clients to install",
                            )
                        );
                    }
                }
                if expanded {
                    if install_button(
                        ui,
                        install_rect.expect("expanded cards have an install button"),
                        &assets.install,
                        !blocked && !self.targets.is_empty() && entry.config.is_some(),
                    ) {
                        install = Some(entry.clone());
                    }
                    let body = Rect::from_min_max(
                        rect.min + egui::vec2(8.0, layout::MCP_CARD_HEIGHT + 1.0),
                        rect.max - egui::vec2(8.0, 8.0),
                    );
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt("market-targets")
                            .max_rect(body)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    child.spacing_mut().item_spacing = egui::vec2(0.0, 3.0);
                    field_title(&mut child, i18n::tr("目标客户端", "Target clients"));
                    targets(
                        &mut child,
                        &mut self.targets,
                        assets,
                        !blocked,
                        true,
                        &entry.name,
                    );
                }
                let pointer_over_install = install_rect.is_some_and(|button| {
                    ui.input(|input| {
                        input
                            .pointer
                            .latest_pos()
                            .is_some_and(|pos| button.contains(pos))
                    })
                });
                if !pointer_over_install && !website_hovered {
                    description_tooltip(&description_response, &entry.description);
                }
            });
        }
        if let Some(entry) = install {
            let form = Form {
                name: entry.local_name(),
                description: entry.description.clone(),
                json: serde_json::to_string_pretty(entry.config.as_ref().unwrap())
                    .unwrap_or_default(),
                targets: self.targets.clone(),
                registry_name: Some(entry.name),
                installation_name: Some(entry.title),
                note: entry.setup_note,
                ..Default::default()
            };
            if entry.requires_setup {
                self.status =
                    form.status_message("请补齐配置后保存", "Complete configuration and save");
                self.warning = false;
                self.form = Some(form);
            } else {
                self.start(ui.ctx(), Command::Save(Box::new(form)));
            }
        }
        if self.cursor.is_some() {
            let (r, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::hover());
            if button(
                ui,
                r,
                i18n::tr("加载更多...", "Load more..."),
                "mcp-more",
                !blocked,
                false,
                11.0,
            ) {
                self.fetch_market(ui.ctx(), self.cursor.clone(), false);
            }
        }
    }
}

fn market_progress(searching: bool) -> &'static str {
    if searching {
        i18n::tr(
            "MCP Server • 正在拉取 & 搜索市场...",
            "MCP Server • Fetching & searching registry...",
        )
    } else {
        i18n::tr(
            "MCP Server • 正在拉取市场...",
            "MCP Server • Fetching registry...",
        )
    }
}

fn matches_local_search(server: &mcp::Server, query: &str) -> bool {
    let query = query.to_lowercase();
    query.is_empty()
        || [server.name.as_str(), server.display_description()]
            .iter()
            .any(|value| value.to_lowercase().contains(&query))
}

pub fn panel_rect(content: Rect, status: Rect) -> Rect {
    let status_inner = Rect::from_min_max(
        status.min + egui::vec2(layout::STATUS_PADDING_LEFT, layout::STATUS_PADDING_TOP),
        status.max - egui::vec2(layout::STATUS_PADDING_RIGHT, layout::STATUS_PADDING_BOTTOM),
    );
    Rect::from_min_max(
        content.min
            + egui::vec2(
                layout::CONTENT_PADDING_LEFT,
                layout::CONTENT_PADDING_TOP + layout::MCP_PANEL_TOP_OFFSET,
            ),
        egui::pos2(
            content.right() - layout::CONTENT_PADDING_RIGHT,
            status_inner.center().y
                - layout::STATUS_LOG_BUTTON_SIZE / 2.0
                - layout::MCP_STATUS_BUTTON_GAP,
        ),
    )
}

pub(super) fn scroll_body(ui: &mut egui::Ui, body: Rect, id: &str) -> egui::Ui {
    // As in provider menus, extend only the viewport into the right padding.
    // The shared scrollbar gutter leaves content at the original 10px inset.
    let viewport = Rect::from_min_max(
        body.min,
        body.max + egui::vec2(f32::from(layout::SCROLLBAR_GUTTER), 0.0),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(viewport)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(viewport.intersect(ui.clip_rect()));
    child.spacing_mut().scroll.fade.strength = 0.0;
    child
}

pub(super) fn expanded_card_height() -> f32 {
    // 8px padding + 28px summary + 9px row gap + 18px legend +
    // 3px legend margin + two 28px target rows with a 6px gap + 8px padding.
    2.0 * layout::MCP_CARD_PADDING
        + 28.0
        + 9.0
        + 18.0
        + 3.0
        + 2.0 * layout::MCP_TARGET_HEIGHT
        + layout::MCP_TARGET_GAP
}

fn install_button_size(ui: &egui::Ui) -> Vec2 {
    let _ = ui;
    Vec2::splat(26.0)
}

fn local_card_height(targets: &[ClientTarget]) -> f32 {
    let height = if targets.is_empty() {
        layout::MCP_CARD_HEIGHT
    } else {
        layout::MCP_CARD_HEIGHT + 14.0
    };
    height + layout::MCP_LOCAL_CARD_BOTTOM_EXTRA
}

pub(super) fn client_target_tags(ui: &mut egui::Ui, left: f32, top: f32, targets: &[ClientTarget]) {
    let mut x = left;
    for target in targets {
        let label = target.title();
        let galley = ui.painter().layout_no_wrap(
            label.to_owned(),
            FontId::proportional(8.0),
            Color32::from_rgb(125, 217, 110),
        );
        let rect = Rect::from_min_size(egui::pos2(x, top), galley.size() + egui::vec2(6.0, 2.0));
        ui.painter()
            .rect_filled(rect, 4, Color32::from_rgba_unmultiplied(98, 197, 85, 38));
        ui.painter().galley(
            rect.min + egui::vec2(3.0, 1.0),
            galley,
            Color32::from_rgb(125, 217, 110),
        );
        x = rect.right() + 5.0;
    }
}

fn registry_picker(ui: &mut egui::Ui, rect: Rect, assets: &GuiAssets) {
    let response = ui
        .interact(rect, ui.id().with("mcp-registry"), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let popup = egui::Popup::menu(&response)
        .align(egui::RectAlign::BOTTOM_END)
        .gap(6.0)
        .width(124.0)
        .frame(log_menu_frame().inner_margin(5));
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
        i18n::tr("MCP 官方市场", "Official Registry"),
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
        let (option, response) = ui.allocate_exact_size(egui::vec2(114.0, 26.0), Sense::click());
        ui.painter()
            .rect_filled(option, 9, if response.hovered() { ORANGE } else { ACTIVE });
        ellipsis(
            ui,
            option.shrink2(egui::vec2(8.0, 0.0)),
            i18n::tr("MCP 官方市场", "Official Registry"),
            11.0,
            if response.hovered() {
                TEXT_MAIN
            } else {
                ACCENT
            },
        );
        paint_surface_outline(ui.painter(), option.expand(5.0), 12);
        if response.clicked() {
            ui.close();
        }
    });
}

pub fn action_button(
    ui: &mut egui::Ui,
    rect: Rect,
    icon: &TextureHandle,
    hint: &str,
    enabled: bool,
    background: Color32,
    tooltip: bool,
) -> bool {
    let r = ui
        .interact(
            rect,
            ui.id().with(("action", hint)),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.painter().rect_filled(
        rect,
        12,
        if r.hovered() && enabled {
            ORANGE
        } else if enabled {
            background
        } else {
            Color32::from_gray(59)
        },
    );
    icon_at_tinted(
        ui,
        icon,
        rect.center(),
        16.0,
        if enabled {
            Color32::WHITE
        } else {
            Color32::from_gray(128)
        },
    );
    if tooltip || !enabled {
        show_hover_tooltip(&r, hint);
    }
    enabled && r.clicked()
}
pub fn model_source_button(
    ui: &mut egui::Ui,
    rect: Rect,
    mode: ModelListMode,
    enabled: bool,
    assets: &GuiAssets,
) -> Option<ModelListMode> {
    let id = ui.id().with("model-source-menu-open");
    let icon = if mode == ModelListMode::Gateway {
        &assets.gateway_models
    } else {
        &assets.default_models
    };
    let background = if mode == ModelListMode::Gateway {
        let rgb = layout::MODEL_SOURCE_GATEWAY_BG;
        Color32::from_rgb(rgb[0], rgb[1], rgb[2])
    } else {
        install_button_bg()
    };
    let clicked = action_button(
        ui,
        rect,
        icon,
        model_mode_label(mode),
        enabled,
        background,
        false,
    );
    let label_width = [ModelListMode::BuiltIn, ModelListMode::Gateway]
        .into_iter()
        .map(|mode| {
            ui.painter()
                .layout_no_wrap(
                    model_mode_label(mode).into(),
                    FontId::proportional(11.0),
                    TEXT_MAIN,
                )
                .size()
                .x
        })
        .fold(0.0_f32, f32::max);
    let popup_width = (label_width + 27.0 + 8.0 + 12.0).max(layout::MODEL_SOURCE_MENU_WIDTH);
    let popup = model_source_popup_rect(rect, popup_width);
    let pointer = ui.input(|i| i.pointer.latest_pos());
    let was_open = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let hover_button = ui.rect_contains_pointer(rect);
    let hover_menu = pointer.is_some_and(|p| model_source_menu_hovered(p, popup, rect));
    let show = enabled && (hover_button || clicked || (was_open && hover_menu));
    ui.ctx().data_mut(|d| d.insert_temp(id, show));
    let mut choice = None;
    if show {
        egui::Area::new(ui.id().with("model-source-popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(popup.min)
            .show(ui.ctx(), |ui| {
                let frame = log_menu_frame().show(ui, |ui| {
                    ui.set_width(popup_width - 12.0);
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 4.0);
                    for selected in [ModelListMode::BuiltIn, ModelListMode::Gateway] {
                        let (r, response) = ui.allocate_exact_size(
                            egui::vec2(popup_width - 12.0, 28.0),
                            Sense::click(),
                        );
                        ui.painter().rect_filled(
                            r,
                            8,
                            if response.hovered() {
                                ORANGE
                            } else if selected == mode {
                                ACTIVE
                            } else {
                                Color32::from_gray(59)
                            },
                        );
                        icon_at(
                            ui,
                            if selected == ModelListMode::Gateway {
                                &assets.gateway_models
                            } else {
                                &assets.default_models
                            },
                            egui::pos2(r.left() + 14.0, r.center().y),
                            14.0,
                        );
                        ellipsis(
                            ui,
                            Rect::from_min_max(
                                egui::pos2(r.left() + 27.0, r.top()),
                                r.max - egui::vec2(8.0, 0.0),
                            ),
                            model_mode_label(selected),
                            11.0,
                            if selected == mode && !response.hovered() {
                                ACCENT
                            } else {
                                TEXT_MAIN
                            },
                        );
                        if response.clicked() {
                            choice = Some(selected);
                        }
                    }
                });
                paint_surface_outline(
                    ui.painter(),
                    frame.response.rect,
                    layout::LOG_MENU_CORNER_RADIUS,
                );
            });
    }
    if choice.is_some() {
        ui.ctx().data_mut(|d| d.insert_temp(id, false));
    }
    choice
}

fn model_source_popup_rect(button: Rect, width: f32) -> Rect {
    let size = egui::vec2(width, 72.0);
    Rect::from_center_size(
        egui::pos2(button.center().x, button.top() - size.y / 2.0 - 6.0),
        size,
    )
}

fn model_source_menu_hovered(pointer: egui::Pos2, popup: Rect, button: Rect) -> bool {
    popup.expand(6.0).contains(pointer) || button.contains(pointer)
}

pub fn back_button(ui: &mut egui::Ui, rect: Rect, id: &str) -> bool {
    let response = ui
        .interact(rect, ui.id().with(id), Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.painter().rect_filled(
        rect,
        7,
        if response.hovered() {
            ORANGE
        } else {
            Color32::TRANSPARENT
        },
    );
    let c = rect.center();
    let stroke = Stroke::new(
        1.3,
        if response.hovered() {
            TEXT_MAIN
        } else {
            TEXT_MUTED
        },
    );
    ui.painter().line_segment(
        [c + egui::vec2(-4.95, 0.0), c + egui::vec2(6.05, 0.0)],
        stroke,
    );
    ui.painter().line_segment(
        [c + egui::vec2(-5.3, 0.0), c + egui::vec2(-0.35, -4.95)],
        stroke,
    );
    ui.painter().line_segment(
        [c + egui::vec2(-5.3, 0.0), c + egui::vec2(-0.35, 4.95)],
        stroke,
    );
    show_hover_tooltip(&response, i18n::tr("返回", "Back"));
    response.clicked()
}
pub(super) fn refresh_button(
    ui: &mut egui::Ui,
    rect: Rect,
    icon: &TextureHandle,
    busy: bool,
    loading: bool,
) -> bool {
    let r = ui.interact(
        rect,
        ui.id().with("mcp-refresh"),
        if busy { Sense::hover() } else { Sense::click() },
    );
    ui.painter().rect_filled(
        rect,
        7,
        if r.hovered() {
            ORANGE
        } else {
            Color32::TRANSPARENT
        },
    );
    if loading {
        let angle = -(ui.input(|i| i.time) as f32) * std::f32::consts::TAU / 0.7;
        let mut mesh = egui::epaint::Mesh::with_texture(icon.id());
        mesh.add_rect_with_uv(
            Rect::from_center_size(rect.center(), Vec2::splat(13.0)),
            Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        mesh.rotate(egui::emath::Rot2::from_angle(angle), rect.center());
        ui.painter().add(egui::Shape::mesh(mesh));
        ui.ctx().request_repaint_after(Duration::from_millis(16));
    } else {
        icon_at(ui, icon, rect.center(), 13.0);
    }
    show_hover_tooltip(&r, i18n::tr("刷新", "Refresh"));
    !busy && r.clicked()
}
pub(super) fn section_title(ui: &egui::Ui, rect: Rect, title: &str, size: f32, dashed: bool) {
    paint_section_title_at(
        ui,
        rect,
        title,
        size,
        layout::SECTION_TITLE_LINE_LENGTH,
        dashed,
    );
}
pub(super) fn field_title(ui: &mut egui::Ui, title: &str) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), Sense::hover());
    section_title(ui, r, title, 10.0, false);
}
pub(super) fn text(ui: &egui::Ui, rect: Rect, value: &str, size: f32, color: Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(value.into(), FontId::proportional(size), color);
    let center_y = if galley.mesh_bounds.is_positive() {
        galley.mesh_bounds.center().y
    } else {
        galley.size().y / 2.0
    };
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .galley(
            egui::pos2(
                rect.center().x - galley.size().x / 2.0,
                rect.center().y - center_y,
            ),
            galley,
            color,
        );
}
pub(super) fn ellipsis(ui: &egui::Ui, rect: Rect, value: &str, size: f32, color: Color32) {
    let mut job = egui::text::LayoutJob::simple(
        value.into(),
        FontId::proportional(size),
        color,
        rect.width(),
    );
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.painter().layout_job(job);
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .galley(
            egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
}
pub(super) fn icon_at(ui: &egui::Ui, icon: &TextureHandle, center: egui::Pos2, size: f32) {
    icon_at_tinted(ui, icon, center, size, Color32::WHITE);
}

fn icon_at_tinted(
    ui: &egui::Ui,
    icon: &TextureHandle,
    center: egui::Pos2,
    size: f32,
    tint: Color32,
) {
    paint_svg_texture(
        ui,
        icon,
        Rect::from_center_size(center, Vec2::splat(size)),
        Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
        tint,
    );
}
pub(super) fn button(
    ui: &mut egui::Ui,
    rect: Rect,
    label: &str,
    id: &str,
    enabled: bool,
    primary: bool,
    size: f32,
) -> bool {
    let r = ui.interact(
        rect,
        ui.id().with(id),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    ui.painter().rect_filled(
        rect,
        if id == "install" { 6 } else { 7 },
        if enabled && r.hovered() {
            ORANGE
        } else if primary && enabled {
            ACTIVE
        } else {
            Color32::from_gray(59)
        },
    );
    text(
        ui,
        rect,
        label,
        size,
        if !enabled {
            Color32::from_gray(110)
        } else if r.hovered() {
            TEXT_MAIN
        } else if primary {
            ACCENT
        } else {
            TEXT_MUTED
        },
    );
    enabled && r.clicked()
}

pub(super) fn install_button(
    ui: &mut egui::Ui,
    rect: Rect,
    icon: &TextureHandle,
    enabled: bool,
) -> bool {
    let response = ui.interact(
        rect,
        ui.id().with("mcp-install"),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let icon_color = if enabled {
        ACCENT
    } else {
        Color32::from_rgb(116, 125, 137)
    };
    ui.painter()
        .rect_filled(rect, 9, if enabled { ACTIVE } else { PANEL });
    icon_at_tinted(ui, icon, rect.center(), 12.0, icon_color);
    show_hover_tooltip(&response, i18n::tr("安装", "Install"));
    enabled && response.clicked()
}

pub(super) fn add_button(ui: &mut egui::Ui, rect: Rect, enabled: bool, busy: bool) -> bool {
    let response = ui.interact(
        rect,
        ui.id().with("mcp-add"),
        if enabled && !busy {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let color = if !enabled {
        Color32::from_gray(110)
    } else if response.hovered() {
        TEXT_MAIN
    } else {
        TEXT_MUTED
    };
    let background_rect = Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width(), layout::MCP_TOOLBAR_CONTROL_HEIGHT),
    );
    ui.painter().rect_filled(
        background_rect,
        9,
        if enabled && response.hovered() {
            ORANGE
        } else {
            ROW
        },
    );
    let center = egui::pos2(background_rect.left() + 13.0, background_rect.center().y);
    for size in [egui::vec2(8.0, 1.2), egui::vec2(1.2, 8.0)] {
        ui.painter()
            .rect_filled(Rect::from_center_size(center, size), 1, color);
    }
    text(
        ui,
        Rect::from_min_max(
            background_rect.min + egui::vec2(22.0, 0.0),
            background_rect.max - egui::vec2(8.0, 0.0),
        ),
        i18n::tr("添加", "Add"),
        11.0,
        color,
    );
    enabled && !busy && response.clicked()
}
pub(super) fn input(ui: &mut egui::Ui, value: &mut String, hint: &str, id: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), Sense::hover());
    ui.painter().rect_filled(rect, 10, ROW);
    ui.put(rect, |ui: &mut egui::Ui| {
        let output = egui::TextEdit::singleline(&mut *value)
            .id_salt(id)
            .hint_text(egui::RichText::new(hint).size(layout::MCP_INPUT_FONT_SIZE))
            .font(FontId::proportional(layout::MCP_INPUT_FONT_SIZE))
            .text_color(TEXT_MAIN)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(8, 5)))
            .vertical_align(egui::Align::Center)
            .min_size(rect.size())
            .show(ui);
        super::text_menu::attach(ui, output, value, super::text_menu::Options::default())
    });
}
fn form_help(ui: &mut egui::Ui, form: &Form) {
    let required = serde_json::from_str::<serde_json::Value>(&form.json)
        .map(|value| mcp::required_config_inputs(&value))
        .unwrap_or_default();
    if form.note.is_empty() && required.is_empty() && form.error.is_empty() {
        return;
    }
    let content_width =
        (ui.available_width() - 2.0 * f32::from(layout::MCP_FORM_HELP_PADDING)).max(1.0);
    let [r, g, b] = layout::MCP_FORM_HELP_BG;
    egui::Frame::NONE
        .fill(Color32::from_rgb(r, g, b))
        .inner_margin(egui::Margin::same(layout::MCP_FORM_HELP_PADDING))
        .corner_radius(layout::MCP_FORM_HELP_RADIUS)
        .show(ui, |ui| {
            ui.set_width(content_width);
            ui.spacing_mut().item_spacing.y = 4.0;
            if !form.note.is_empty() {
                ui.add(
                    egui::Label::new(RichText::new(&form.note).size(10.0).color(TEXT_MUTED)).wrap(),
                );
            }
            let warning = Color32::from_rgb(255, 179, 107);
            if !required.is_empty() {
                ui.add(
                    egui::Label::new(
                        RichText::new(i18n::tr(
                            "请在 JSON 中补齐以下必填项后保存：",
                            "Complete these required values in the JSON, then save:",
                        ))
                        .size(10.0)
                        .color(warning),
                    )
                    .wrap(),
                );
                for input in required {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("• {} ({})", input.name, input.path))
                                .size(10.0)
                                .color(warning),
                        )
                        .wrap(),
                    );
                }
            }
            if !form.error.is_empty() {
                ui.add(
                    egui::Label::new(RichText::new(&form.error).size(10.0).color(warning)).wrap(),
                );
            }
        });
}

fn json_editor(ui: &mut egui::Ui, form: &mut Form) -> bool {
    let read_only = form.read_only();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), form.json_height),
        Sense::hover(),
    );
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
        ui.painter().layout_no_wrap(
            text.as_str().to_owned(),
            FontId::monospace(layout::MCP_INPUT_FONT_SIZE),
            TEXT_MAIN,
        )
    };
    let text_width = layouter(&child, &form.json, f32::INFINITY).size().x;
    let viewport_width = inner.width() - child.spacing().scroll.content_margin.sum().x;
    let changed = egui::ScrollArea::both()
        .hscroll(text_width > viewport_width)
        .id_salt("mcp-json-scroll")
        .max_height(inner.height())
        .auto_shrink([false, false])
        .show(&mut child, |ui| {
            if read_only {
                let mut text = form.json.as_str();
                let output = egui::TextEdit::multiline(&mut text)
                    .id_salt("mcp-json")
                    .code_editor()
                    .font(FontId::monospace(layout::MCP_INPUT_FONT_SIZE))
                    .layouter(&mut layouter)
                    .desired_width(f32::INFINITY)
                    .desired_rows(1)
                    .frame(egui::Frame::NONE)
                    .text_color(TEXT_MAIN)
                    .show(ui);
                super::text_menu::attach_read_only(ui, output, text);
                return false;
            }
            let output = egui::TextEdit::multiline(&mut form.json)
                .id_salt("mcp-json")
                .code_editor()
                .font(FontId::monospace(layout::MCP_INPUT_FONT_SIZE))
                .layouter(&mut layouter)
                .desired_width(f32::INFINITY)
                .desired_rows(1)
                .frame(egui::Frame::NONE)
                .text_color(TEXT_MAIN)
                .show(ui);
            super::text_menu::attach(
                ui,
                output,
                &mut form.json,
                super::text_menu::Options {
                    multiline: true,
                    password: false,
                },
            )
            .changed()
        })
        .inner;
    let handle = Rect::from_min_size(rect.right_bottom() - Vec2::splat(20.0), Vec2::splat(20.0));
    let r = ui
        .interact(handle, ui.id().with("mcp-json-resize"), Sense::drag())
        .on_hover_cursor(egui::CursorIcon::ResizeVertical);
    if r.dragged() {
        form.json_height =
            (form.json_height + ui.input(|i| i.pointer.delta().y)).clamp(120.0, 1200.0);
    }
    for offset in [0.0, 4.0] {
        ui.painter().line_segment(
            [
                rect.right_bottom() + egui::vec2(-12.0 + offset, -5.0),
                rect.right_bottom() + egui::vec2(-5.0, -12.0 + offset),
            ],
            Stroke::new(1.0, if r.hovered() { ACCENT } else { TEXT_MUTED }),
        );
    }
    changed
}
pub(super) fn targets(
    ui: &mut egui::Ui,
    selected: &mut Vec<ClientTarget>,
    assets: &GuiAssets,
    enabled: bool,
    market: bool,
    id: &str,
) {
    let width = (ui.available_width() - layout::MCP_TARGET_GAP) / 2.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(
            ui.available_width(),
            2.0 * layout::MCP_TARGET_HEIGHT + layout::MCP_TARGET_GAP,
        ),
        Sense::hover(),
    );
    for (index, target) in ClientTarget::ALL.into_iter().enumerate() {
        let r = Rect::from_min_size(
            rect.min
                + egui::vec2(
                    (index % 2) as f32 * (width + layout::MCP_TARGET_GAP),
                    (index / 2) as f32 * (layout::MCP_TARGET_HEIGHT + layout::MCP_TARGET_GAP),
                ),
            egui::vec2(width, layout::MCP_TARGET_HEIGHT),
        );
        let response = ui.interact(
            r,
            ui.id().with((id, target.id())),
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let on = selected.contains(&target);
        ui.painter().rect_filled(
            r,
            9,
            if on || response.hovered() {
                ACTIVE
            } else if market {
                PANEL
            } else {
                ROW
            },
        );
        icon_at(
            ui,
            assets.client_icon(target),
            egui::pos2(r.left() + 12.5, r.center().y),
            13.0,
        );
        ellipsis(
            ui,
            Rect::from_min_max(
                egui::pos2(r.left() + 24.0, r.top()),
                egui::pos2(r.right() - 22.0, r.bottom()),
            ),
            target.title(),
            10.0,
            if on {
                Color32::from_rgb(196, 216, 255)
            } else {
                TEXT_MUTED
            },
        );
        if on {
            icon_at(
                ui,
                &assets.select_green,
                egui::pos2(r.right() - 12.0, r.center().y),
                13.0,
            );
        }
        if enabled && response.clicked() {
            if on {
                selected.retain(|t| *t != target);
            } else {
                selected.push(target);
            }
        }
    }
}
pub(super) fn card_surface(ui: &egui::Ui, rect: Rect) {
    ui.painter().rect_filled(rect, 12, ROW);
    // Parent responses stop hovering over interactive descendants. The outline
    // follows the full clipped card, just like CSS :hover, including its buttons.
    if ui.rect_contains_pointer(rect) {
        ui.painter().rect_stroke(
            rect,
            12,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(20, 126, 255, 84)),
            egui::StrokeKind::Inside,
        );
    }
}
pub(super) fn mark(ui: &egui::Ui, rect: Rect, key: &str, name: &str) {
    let (background, foreground) = mark_colors(key);
    ui.painter().rect_filled(rect, 8, background);
    let letters: String = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .collect();
    text(ui, rect, &letters.to_uppercase(), 10.0, foreground);
}

fn mark_colors(name: &str) -> (Color32, Color32) {
    const PALETTE: [(Color32, Color32); 4] = [
        (
            Color32::from_rgb(41, 49, 68),
            Color32::from_rgb(139, 175, 255),
        ),
        (
            Color32::from_rgb(36, 57, 48),
            Color32::from_rgb(104, 215, 164),
        ),
        (
            Color32::from_rgb(53, 41, 68),
            Color32::from_rgb(198, 162, 255),
        ),
        (
            Color32::from_rgb(64, 51, 39),
            Color32::from_rgb(239, 180, 127),
        ),
    ];
    let hash = name.bytes().fold(0usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as usize)
    });
    PALETTE[hash % PALETTE.len()]
}
pub(super) fn card_title(
    ui: &egui::Ui,
    rect: Rect,
    name: &str,
    transport: &str,
    transport_color: Color32,
    reserved: f32,
) -> Rect {
    let pill_w = ui
        .painter()
        .layout_no_wrap(transport.into(), FontId::proportional(8.0), transport_color)
        .size()
        .x
        + 6.0;
    let name_w = ui
        .painter()
        .layout_no_wrap(name.into(), FontId::proportional(11.0), CARD_TITLE_TEXT)
        .size()
        .x
        .min((rect.width() - pill_w - 6.0 - reserved).max(10.0));
    ellipsis(
        ui,
        Rect::from_min_size(rect.min, egui::vec2(name_w, rect.height())),
        name,
        11.0,
        CARD_TITLE_TEXT,
    );
    let pill = Rect::from_min_size(
        egui::pos2(rect.left() + name_w + 6.0, rect.top() + 2.0),
        egui::vec2(pill_w, 12.0),
    );
    ui.painter()
        .rect_filled(pill, 4, Color32::from_rgba_unmultiplied(103, 58, 183, 115));
    text(ui, pill, transport, 8.0, transport_color);
    pill
}
pub(super) fn toggle_button(ui: &mut egui::Ui, rect: Rect, on: bool, enabled: bool) -> bool {
    let r = ui.interact(
        rect,
        ui.id().with("toggle"),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    ui.painter().rect_filled(
        rect,
        8,
        if on {
            Color32::from_rgb(0, 172, 99)
        } else {
            Color32::from_rgb(80, 85, 99)
        },
    );
    ui.painter().circle_filled(
        egui::pos2(rect.left() + if on { 17.0 } else { 7.0 }, rect.center().y),
        5.0,
        Color32::from_rgb(220, 226, 235),
    );
    show_hover_tooltip(
        &r,
        if on {
            i18n::tr("关闭", "Disable")
        } else {
            i18n::tr("开启", "Enable")
        },
    );
    enabled && r.clicked()
}
pub(super) fn small_icon(
    ui: &mut egui::Ui,
    rect: Rect,
    icon: &TextureHandle,
    id: &str,
    hint: &str,
    enabled: bool,
) -> bool {
    let r = ui.interact(
        rect,
        ui.id().with(id),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if r.hovered() {
        ui.painter()
            .rect_filled(rect, 4, Color32::from_white_alpha(18));
    }
    icon_at_tinted(
        ui,
        icon,
        rect.center(),
        11.0,
        if r.hovered() && id == "delete" {
            Color32::from_rgb(255, 87, 34)
        } else if r.hovered() {
            Color32::WHITE
        } else {
            Color32::from_gray(140)
        },
    );
    show_hover_tooltip(&r, hint);
    enabled && r.clicked()
}
pub(super) fn description_tooltip(response: &egui::Response, description: &str) {
    if description.is_empty() {
        return;
    }
    let width = response.rect.width().max(80.0);
    let mut anchor = response.clone();
    let offset = egui::vec2(0.0, layout::TOOLTIP_GAP - layout::TOOLTIP_ARROW_HEIGHT);
    anchor.rect = response.rect.translate(offset);
    // PopupAnchor reads interact_rect; rect alone only changes the tooltip width.
    anchor.interact_rect = anchor.rect;
    show_hover_tooltip_contents_width(&anchor, egui::RectAlign::TOP, width, |ui| {
        let content_width = (width - 16.0).max(1.0);
        let mut lines = Vec::new();
        for paragraph in description.lines() {
            let mut line = String::new();
            for character in paragraph.chars() {
                let mut candidate = line.clone();
                candidate.push(character);
                let candidate_width = ui
                    .painter()
                    .layout_no_wrap(
                        candidate.clone(),
                        FontId::proportional(11.0),
                        Color32::WHITE,
                    )
                    .size()
                    .x;
                if !line.is_empty() && candidate_width > content_width {
                    lines.push(std::mem::take(&mut line));
                }
                line.push(character);
            }
            lines.push(line);
        }
        let mut job = egui::text::LayoutJob::default();
        for (index, line) in lines.iter().enumerate() {
            let natural_width = ui
                .painter()
                .layout_no_wrap(line.clone(), FontId::proportional(11.0), Color32::WHITE)
                .size()
                .x;
            let mut format = egui::TextFormat::simple(FontId::proportional(11.0), Color32::WHITE);
            format.line_height = Some(16.0);
            let gaps = line.chars().count().saturating_sub(1);
            if index + 1 < lines.len() && gaps > 0 {
                format.extra_letter_spacing =
                    (content_width - natural_width).max(0.0) / gaps as f32;
            }
            job.append(line, 0.0, format.clone());
            if index + 1 < lines.len() {
                job.append("\n", 0.0, format);
            }
        }
        job.wrap.max_width = content_width;
        ui.add(egui::Label::new(job).halign(egui::Align::Center));
    });
}
