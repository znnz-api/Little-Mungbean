//! About page. The Chinese copy follows the current Website prototype.
use super::mcp_ui::{PANEL, back_button, scroll_body};
use super::*;

const ABOUT_ACCENT: Color32 = Color32::from_rgb(22, 135, 255);

mod selection;

#[cfg(test)]
mod tests;

#[derive(Default)]
pub struct State {
    pub open: bool,
}

impl State {
    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        providers: &[crate::gui_providers::Provider],
        provider_textures: &[TextureHandle],
    ) {
        self.render_language(ui, rect, i18n::language(), providers, provider_textures);
    }

    fn render_language(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        language: i18n::Language,
        providers: &[crate::gui_providers::Provider],
        provider_textures: &[TextureHandle],
    ) {
        selection::begin(ui.ctx());
        ui.painter()
            .rect_filled(rect, layout::MCP_PANEL_RADIUS, PANEL);
        paint_surface_outline(ui.painter(), rect, layout::MCP_PANEL_RADIUS);
        let inner = rect.shrink(layout::MCP_PANEL_PADDING);
        let title_rect = Rect::from_min_size(inner.min, egui::vec2(inner.width(), 18.0));
        selectable_section_title(
            ui,
            title_rect,
            language.text("关于", "About"),
            12.0,
            TEXT_MUTED,
        );
        let back = Rect::from_center_size(
            egui::pos2(title_rect.left() + 8.0, title_rect.center().y),
            Vec2::splat(layout::PANEL_BACK_SIZE),
        );
        if back_button(ui, back, "about-back")
            || ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.open = false;
        }
        let body = Rect::from_min_max(
            egui::pos2(inner.left(), title_rect.bottom() + 8.0),
            inner.max,
        );
        let mut child = scroll_body(ui, body, "about-body");
        egui::ScrollArea::vertical()
            .id_salt("about-scroll")
            .auto_shrink([false, false])
            .max_height(body.height())
            .show(&mut child, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 7.0);
                paragraph(ui, language.text(
                    "小绿豆 • Little Mungbean • 是一个免费、开源、简单、轻量的 AI 网关管理器，让 Codex 与 Claude 客户端共享一个可切换、可观测的本地网关。支持多供应商管理、本地协议转换、模型注入、MCP Server 与 Skills 管理、用量统计等功能...",
                    "Little Mungbean is a free, open-source, simple, lightweight AI gateway manager that lets Codex and Claude clients share a switchable, observable local gateway. It supports multiple providers, local protocol conversion, model injection, MCP Server and Skills management, usage statistics, and more."
                ), 11.0, Color32::from_rgb(217, 222, 232));
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(103, 58, 183, 115))
                    .inner_margin(8)
                    .corner_radius(12)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        paragraph(ui, language.text(
                            "如果小绿豆帮到了你，可在 GitHub 点个 Star，这对我们意义重大。作者博客\n您也可以成为赞助商，信息将出现在下方、或供应商预设里，底部可见联系邮箱。",
                            "If Little Mungbean helps you, please give us a Star on GitHub. It means a lot to us. Or buy the author a coffee .\nYou may also become a sponsor. Your info will be shown below or in vendor presets. Email is available at the bottom."
                        ), 11.0, Color32::from_rgb(193, 159, 255));
                    });
                heading(ui, language.text("赞助商", "Sponsors"));
                render_sponsors(ui, providers, provider_textures, language);
                // Keep the section heading vertically centered with the tighter website gap.
                ui.add_space(4.0);
                ui.spacing_mut().item_spacing.y = 11.0;
                heading(ui, language.text("主要功能", "Main features"));
                ui.spacing_mut().item_spacing.y = 7.0;
                for (zh, en) in FEATURES {
                    status_light(ui, BLUE_BRIGHT, language.text(zh, en));
                }
                heading(ui, language.text("客户端状态灯", "Client status lights"));
                for (color, zh, en) in CLIENT_LIGHTS { status_light(ui, *color, language.text(zh, en)); }
                heading(ui, language.text("流动光点状态", "Flowing Dot Status"));
                status_light(ui, Color32::BLACK, language.text("黑色 • 未连接", "Black • Disconnected"));
                status_light(ui, GREEN, language.text("绿色 • 已连接；流动光点不代表实时数据传输", "Green • Connected; flowing dots do not indicate real-time data transfer"));
                heading(ui, language.text("常见问题", "Frequently asked questions"));
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    for (zh_question, en_question, zh_answer, en_answer) in FAQ {
                        paragraph(ui, language.text(zh_question, en_question), 10.0, Color32::from_rgb(217, 222, 232));
                        faq_answer(ui, language.text(zh_answer, en_answer));
                        ui.add_space(1.0);
                    }
                });
                heading(ui, language.text("开源协议", "License"));
                paragraph(ui, language.text("本项目使用 Apache License 2.0 开源。", "This project is open source under the Apache License 2.0."), 10.0, TEXT_MUTED);
                ui.add_space(7.0);
            });
        selection::finish(ui);
    }
}

fn faq_answer(ui: &mut egui::Ui, value: &str) {
    // Match Website .Answer and the collapsed 8px paragraph/list margins.
    egui::Frame::new()
        .fill(Color32::from_black_alpha(64))
        .inner_margin(egui::Margin {
            left: 9,
            right: 9,
            top: 1,
            bottom: 1,
        })
        .corner_radius(12)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.add_space(8.0);
            let mut lines = value.lines().filter(|line| !line.is_empty()).peekable();
            let mut first_block = true;
            while let Some(&line) = lines.peek() {
                if !first_block {
                    ui.add_space(8.0);
                }
                first_block = false;
                if line.starts_with("• ") {
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        while let Some(item) = lines.peek().and_then(|line| line.strip_prefix("• "))
                        {
                            ui.horizontal_top(|ui| {
                                let (marker, _) =
                                    ui.allocate_exact_size(egui::vec2(16.0, 15.5), Sense::hover());
                                ui.painter().circle_filled(
                                    egui::pos2(marker.left() + 8.0, marker.center().y),
                                    1.5,
                                    TEXT_MUTED,
                                );
                                paragraph(ui, item, 10.0, TEXT_MUTED);
                            });
                            lines.next();
                        }
                    });
                } else {
                    paragraph(ui, line, 10.0, TEXT_MUTED);
                    lines.next();
                }
            }
            ui.add_space(8.0);
        });
}

fn render_sponsors(
    ui: &mut egui::Ui,
    providers: &[crate::gui_providers::Provider],
    provider_textures: &[TextureHandle],
    language: i18n::Language,
) {
    let sponsors: Vec<_> = providers
        .iter()
        .enumerate()
        .filter(|(_, provider)| provider.show_in.about())
        .collect();
    if sponsors.is_empty() {
        paragraph(
            ui,
            language.text("暂无赞助商", "No sponsors yet"),
            10.0,
            TEXT_MUTED,
        );
        return;
    }

    ui.spacing_mut().item_spacing.y = layout::PROVIDER_ROW_GAP;
    for row in sponsors.chunks(layout::ABOUT_SPONSOR_MAX_COLUMNS) {
        let width = ui.available_width();
        let card_size = layout::PROVIDER_BUTTON_SIZE;
        let gap = layout::PROVIDER_BUTTON_GAP;
        let (row_rect, _) = ui.allocate_exact_size(Vec2::new(width, card_size), Sense::hover());
        let left = row_rect.left();

        for (column, (provider_index, provider)) in row.iter().enumerate() {
            let rect = Rect::from_min_size(
                egui::pos2(left + column as f32 * (card_size + gap), row_rect.top()),
                Vec2::splat(card_size),
            );
            let mut response = ui.interact(
                rect,
                ui.id().with(("about-sponsor", provider.id.as_str())),
                if provider.website.is_some() {
                    Sense::click()
                } else {
                    Sense::hover()
                },
            );
            if provider.website.is_some() {
                response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
            }
            let [r, g, b] = layout::PROVIDER_BACKGROUND;
            let background = if response.hovered() {
                let [r, g, b, a] = layout::PROVIDER_HOVER_BACKGROUND;
                Color32::from_rgba_unmultiplied(r, g, b, a)
            } else {
                Color32::from_rgba_unmultiplied(r, g, b, layout::PROVIDER_BACKGROUND_ALPHA)
            };
            ui.painter()
                .rect_filled(rect, layout::PROVIDER_CORNER_RADIUS, background);
            if let Some(texture) = provider_textures.get(*provider_index) {
                paint_svg_texture(
                    ui,
                    texture,
                    Rect::from_center_size(rect.center(), Vec2::splat(layout::PROVIDER_ICON_SIZE)),
                    Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            let outline = if response.hovered() {
                let [r, g, b, a] = layout::PROVIDER_HOVER_OUTLINE;
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(r, g, b, a))
            } else {
                surface_outline_stroke()
            };
            ui.painter().rect_stroke(
                rect,
                layout::PROVIDER_CORNER_RADIUS,
                outline,
                egui::StrokeKind::Inside,
            );
            show_hover_tooltip(&response, provider.name.clone());
            if response.clicked()
                && let Some(website) = provider.website.as_deref()
            {
                open_external_link(ui.ctx(), website);
            }
        }
    }
    ui.spacing_mut().item_spacing.y = 7.0;
}

fn paragraph(ui: &mut egui::Ui, value: &str, size: f32, color: Color32) {
    formatted_paragraph(ui, value, size, color, false);
}

fn formatted_paragraph(
    ui: &mut egui::Ui,
    value: &str,
    size: f32,
    color: Color32,
    emphasize_prefix: bool,
) {
    let mut links = Vec::new();
    for (label, url) in [
        (
            "GitHub",
            "https://r.networkpe.top/Little-Mungbean-GitHub-EN",
        ),
        ("作者博客", "https://r.networkpe.top/Little-Mungbean-blog"),
        ("coffee", "https://r.networkpe.top/Little-Mungbean-coffee"),
        (
            "Apache License 2.0",
            "https://www.apache.org/licenses/LICENSE-2.0",
        ),
        ("heimen115@gmail.com", "mailto:heimen115@gmail.com"),
    ] {
        if let Some(start) = value.find(label) {
            links.push((start..start + label.len(), url));
        }
    }
    let mut accents: Vec<_> = links.iter().map(|(range, _)| range.clone()).collect();
    for label in ["Star", "heimen115@gmail.com"] {
        if let Some(start) = value.find(label)
            && (label != "Star" || value.contains("GitHub"))
        {
            accents.push(start..start + label.len());
        }
    }
    let mut boundaries = vec![0, value.len()];
    for range in &accents {
        boundaries.extend([range.start, range.end]);
    }
    let strong_end = value.find(" • ").filter(|_| emphasize_prefix);
    if let Some(end) = strong_end {
        boundaries.push(end);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    for pair in boundaries.windows(2) {
        let range = pair[0]..pair[1];
        let accent = accents.iter().any(|range_| range_.contains(&range.start));
        let strong = strong_end.is_some_and(|end| range.end <= end);
        let mut format = egui::TextFormat::simple(
            FontId::proportional(size),
            if accent {
                ABOUT_ACCENT
            } else if strong {
                Color32::from_rgb(217, 222, 232)
            } else {
                color
            },
        );
        format.line_height = Some(size * if size == 11.0 { 1.6 } else { 1.55 });
        job.append(&value[range], 0.0, format);
    }
    let galley = ui.painter().layout_job(job);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click_and_drag());
    selection::paint(ui, &response, rect.min, galley.clone(), color);
    // egui's built-in underline follows the expanded line height. Draw near the baseline instead.
    let mut row_start = 0;
    for row in &galley.rows {
        let row_end = row_start + row.glyphs.len();
        for (range, _) in &links {
            let start = value[..range.start].chars().count().max(row_start);
            let end = value[..range.end].chars().count().min(row_end);
            if start < end {
                let first = &row.glyphs[start - row_start];
                let last = &row.glyphs[end - row_start - 1];
                let origin = rect.min + row.pos.to_vec2();
                let y = origin.y + last.pos.y + 1.0;
                ui.painter().line_segment(
                    [
                        egui::pos2(origin.x + first.pos.x, y),
                        egui::pos2(origin.x + last.max_x(), y),
                    ],
                    Stroke::new(0.6, ABOUT_ACCENT),
                );
            }
        }
        row_start = row_end + usize::from(row.ends_with_newline);
    }
    if response.hovered()
        && let Some(pos) = response.hover_pos()
    {
        let index = galley.cursor_from_pos(pos - rect.min).index.0;
        for (range, url) in links {
            let chars = value[..range.start].chars().count()..value[..range.end].chars().count();
            if chars.contains(&index) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                if response.clicked() {
                    open_external_link(ui.ctx(), url);
                }
            }
        }
    }
}

fn heading(ui: &mut egui::Ui, value: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), Sense::hover());
    selectable_section_title(ui, rect, value, 11.0, ABOUT_ACCENT);
}

fn selectable_section_title(ui: &mut egui::Ui, rect: Rect, value: &str, size: f32, color: Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(value.into(), FontId::proportional(size), color);
    let visible_center = if galley.mesh_bounds.is_positive() {
        galley.mesh_bounds.center().y
    } else {
        galley.size().y / 2.0
    };
    let pos = egui::pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.center().y - visible_center,
    );
    let text_rect = Rect::from_min_size(pos, galley.size());
    let gap = layout::SECTION_TITLE_LINE_GAP;
    let length = layout::SECTION_TITLE_LINE_LENGTH
        .min(((rect.width() - text_rect.width()) / 2.0 - gap).max(0.0));
    let [r, g, b, a] = layout::SECTION_TITLE_LINE_COLOR;
    let stroke = Stroke::new(
        layout::SECTION_TITLE_LINE_WIDTH,
        Color32::from_rgba_unmultiplied(r, g, b, a),
    );
    for (left, right) in [
        (text_rect.left() - gap - length, text_rect.left() - gap),
        (text_rect.right() + gap, text_rect.right() + gap + length),
    ] {
        ui.painter().line_segment(
            [
                egui::pos2(left, rect.center().y),
                egui::pos2(right, rect.center().y),
            ],
            stroke,
        );
    }
    let response = ui.interact(
        text_rect,
        ui.id().with(("about-heading", value)),
        Sense::click_and_drag(),
    );
    selection::paint(ui, &response, pos, galley, color);
}

fn bullet(ui: &mut egui::Ui, value: &str) {
    formatted_paragraph(ui, value, 10.0, TEXT_MUTED, true);
}

fn status_light(ui: &mut egui::Ui, color: Color32, value: &str) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(7.0, 15.5), Sense::hover());
        ui.painter().circle_filled(rect.center(), 3.0, color);
        bullet(ui, value);
    });
}

const FEATURES: &[(&str, &str)] = &[
    (
        "客户端一键安装 • 支持一键安装 Codex CLI、Codex Desktop、Claude Code、Claude Desktop。",
        "One-click client installation • Install Codex CLI, Codex Desktop, Claude Code, and Claude Desktop.",
    ),
    (
        "多供应商管理 • 保存多个供应商的接口地址和 API Key，支持重命名、拖动排序和快速切换。",
        "Multiple providers • Save multiple provider endpoints and API keys, with renaming, drag sorting, and quick switching.",
    ),
    (
        "供应商切换 • 切换供应商接口地址或 API Key 后，点击 重启 • 连接，重启所选 Agent 客户端并重新加载配置。",
        "Provider switching • After changing the endpoint or API key, click Restart • Connect to restart the selected Agent client and reload its configuration.",
    ),
    (
        "账号模式切换 • 可在本地网关模式与 Agent 客户端官方账号模式之间切换，无需退出账号登录，并自动恢复原始配置。",
        "Account mode switching • Switch between the local gateway and the client's official account mode without signing out, with automatic restoration of the original configuration.",
    ),
    (
        "多客户端独立接入 • 分别管理四类 Agent 客户端的安装、运行、连接状态和网关路由。",
        "Independent client access • Manage installation, running and connection states, and gateway routes separately for all four Agent clients.",
    ),
    (
        "MCP Server 管理与市场 • 支持本地与市场 MCP Server 手动添加、市场安装、编辑、搜索、开关、移除、多 Agent 客户支持、查看 MCP Server 服务网站。",
        "MCP Server management and market • Add local MCP servers, install from the market, edit, search, enable, disable, and remove servers; supports multiple Agent clients and service website links.",
    ),
    (
        "Skills 管理与市场 • 支持本地与市场 Skills 手动添加、市场安装、导入 目录/ZIP、编辑、搜索、开关、移除、多 Agent 客户支持、查看 Skill 服务网站。Claude Desktop 通过 Customize > Skills 导入ZIP。",
        "Skills management and market • Add local Skills, install from the market, import folders or ZIPs, edit, search, enable, disable, and remove Skills; supports multiple Agent clients and service website links. Import ZIP via Customize > Skills in Claude Desktop.",
    ),
    (
        "本地协议转换 • 支持 OpenAI、Anthropic、Gemini 等协议转换；同族模型优先直接转发，并支持热开关。详情见下方常见问题。",
        "Local protocol conversion • Convert between OpenAI, Anthropic, Gemini, and other protocols. Models in the same family prefer direct forwarding, with hot switching. See the FAQ below.",
    ),
    (
        "网关模型拉取管理 • 从供应商拉取模型列表，按供应商隐藏老旧或不常用模型。",
        "Gateway model management • Fetch provider models and hide older or less-used models per provider.",
    ),
    (
        "模型映射注入 • 将供应商模型注入 Agent 客户端的模型列表，方便直接选择使用。",
        "Model injection • Inject provider models into Agent client model lists for direct selection.",
    ),
    (
        "供应商用量统计 • 按供应商独立统计请求数和 Token 用量。",
        "Provider usage statistics • Track request counts and token usage separately for each provider.",
    ),
    (
        "实时状态检测 • 自动检测 Agent 客户端是否安装、运行和连接网关。",
        "Live status detection • Automatically detect whether Agent clients are installed, running, and connected to the gateway.",
    ),
    (
        "安全配置保护 • API Key 使用本地加密保存，网关仅监听本机回环地址。",
        "Configuration protection • Store API keys with local encryption; the gateway listens only on loopback addresses.",
    ),
    (
        "客户端连接信息 • 右击客户端卡片，可打开配置目录，或复制当前本地连接的 Base URL 与 Token，方便搭配其他工具使用。",
        "Client connection information • Right-click a client card to open its configuration folder or copy the current local Base URL and Token for use with other tools.",
    ),
    (
        "运行日志 • 记录客户端启动、连接、重启、模型注入、网关操作与 MCP/Skills 变更。启动或重启成功后输出本地监听地址，CLI 还显示配置目录与工作目录；连接令牌可从客户端卡片右键菜单复制。",
        "Runtime logs • Record client starts, connections, restarts, model injection, gateway operations, and MCP/Skills changes. Successful starts and restarts log the local listening address; CLI logs also show configuration and working directories. Copy connection tokens from the client card context menu.",
    ),
    (
        "请求日志 • 记录 Agent、供应商、模型、状态码、Token、耗时、首字延迟、请求端点和协议转换等信息。",
        "Request logs • Record the Agent, provider, model, status code, tokens, duration, time to first token, endpoint, protocol conversion, and more.",
    ),
    (
        "中英文界面 • 支持中文和英文界面及日志输出。",
        "Chinese and English • Supports both languages for the interface and logs.",
    ),
];

const CLIENT_LIGHTS: &[(Color32, &str, &str)] = &[
    (TEXT_MUTED, "灰色(×) • 未安装", "Gray(×) • Not installed"),
    (TEXT_MUTED, "灰色 • 未运行", "Gray • Not running"),
    (
        BLUE_BRIGHT,
        "蓝色 • 运行中，未连接本地网关",
        "Blue • Running, disconnected from the local gateway",
    ),
    (
        GREEN,
        "绿色 • 已连接本地网关",
        "Green • Connected to the local gateway",
    ),
];

const FAQ: &[(&str, &str, &str, &str)] = &[
    (
        "本地网关模式是什么？",
        "What is local gateway mode?",
        "Agent 客户端(例如 Claude Code、Codex Desktop)先连接到 Little-Mungbean 的本地网关，再由网关将请求转发到真正的 AI 模型服务商，包括官方 API 或第三方中转服务。\n\n本地网关负责协议转换、请求路由、供应商切换和密钥管理，让不同客户端能够更方便地调用不同模型。",
        "Agent clients such as Claude Code and Codex Desktop connect to Little-Mungbean's local gateway, which forwards requests to an AI model provider, including official APIs or third-party relays.\n\nThe gateway handles protocol conversion, request routing, provider switching, and key management so clients can access different models more easily.",
    ),
    (
        "账号模式是什么？",
        "What is account mode?",
        "账号模式下，Agent 客户端直接通过已登录的 AI 服务商官方账号连接和对话，不使用 Little-Mungbean 的本地网关路由。",
        "In account mode, the Agent client connects using its signed-in official provider account, without routing through Little-Mungbean's local gateway.",
    ),
    (
        "什么是协议转换？",
        "What is protocol conversion?",
        "协议转换是指在 AI 网关或代理中，将 Agent 客户端使用的一种 API 格式自动翻译成上游模型支持的另一种格式，并对请求和响应进行双向转换，包含流式响应。\n\n当前涉及的本地协议转换主要格式包括：\n• OpenAI Chat Completions\n• OpenAI Responses\n• Anthropic Messages\n• Gemini GenerateContent",
        "Protocol conversion translates the Agent client's API format into one supported by the upstream model, converting requests and responses in both directions, including streaming responses.\n\nThe main formats supported are:\n• OpenAI Chat Completions\n• OpenAI Responses\n• Anthropic Messages\n• Gemini GenerateContent",
    ),
    (
        "什么情况建议开启本地协议转换？",
        "When should local protocol conversion be enabled?",
        "当你需要使用跨族的模型时，建议开启本地协议转换，例如：\n• 在 Codex 中使用 Claude、Gemini 或其他模型族的模型。\n• 在 Claude 中使用 GPT、Grok、Gemini 或其他模型族的模型。",
        "Enable local protocol conversion when using models from another family, for example:\n• Claude, Gemini, or other model families in Codex.\n• GPT, Grok, Gemini, or other model families in Claude.",
    ),
    (
        "什么情况建议关闭本地协议转换？",
        "When should local protocol conversion be disabled?",
        "• 不需要使用跨族模型时。\n• 供应商已经提供服务端协议转换时，建议关闭本地协议转换，避免重复转换。\n• Grok 原生支持 OpenAI Responses，因此在 Codex 中使用 Grok 通常不需要开启本地协议转换。",
        "• When models from another family are not needed.\n• When the provider already offers server-side conversion, to avoid duplicate conversion.\n• Grok natively supports OpenAI Responses, so local conversion is usually unnecessary when using Grok in Codex.",
    ),
    (
        "关闭 Little-Mungbean 窗口后网关会停止吗？",
        "Does closing the Little-Mungbean window stop the gateway?",
        "不会。关闭窗口后网关会继续在系统托盘运行，以保持已连接客户端的网络请求。选择托盘菜单中的 退出 才会完全停止网关；再次使用时，需要重新点击 启动 • 连接。",
        "No. The gateway continues running in the system tray to serve connected clients. Select Exit from the tray menu to stop it completely. Click Start • Connect again when you next use it.",
    ),
    (
        "API Key 会保存在哪里？",
        "Where are API keys stored?",
        "API Key 会在本机加密保存。网关只监听本机回环地址，不对局域网或公网开放监听端口。",
        "API keys are encrypted on this computer. The gateway listens only on loopback addresses and does not expose a listening port to the LAN or Internet.",
    ),
    (
        "如何反馈问题？",
        "How can I send feedback?",
        "BUG与意见反馈邮箱：heimen115@gmail.com",
        "Bug reports and feedback: heimen115@gmail.com",
    ),
];
