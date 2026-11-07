//! 演员预设页：表格、搜索、添加与删除。

use ::core::{Preset, PresetKind};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::{h_flex, v_flex, Disableable as _, Sizable as _, StyledExt as _, Theme};
use gpui_kit::*;

use crate::app::AppState;
use crate::ui;

pub(crate) fn preset_kind_label(kind: PresetKind) -> &'static str {
    match kind {
        PresetKind::Person => "演员姓名",
        PresetKind::Tag => "标签",
        PresetKind::Genre => "类型",
    }
}

/// Parse the paste-import format: one `原名=译名` per line. Blank lines are
/// ignored; non-empty lines without `=` (or with an empty side) are skipped.
fn parse_preset_import(raw: &str) -> (Vec<(String, String)>, usize) {
    let mut entries = Vec::new();
    let mut skipped = 0usize;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match line.split_once('=') {
            Some((source, target)) => {
                let source = source.trim();
                let target = target.trim();
                if source.is_empty() || target.is_empty() {
                    skipped += 1;
                } else {
                    entries.push((source.to_string(), target.to_string()));
                }
            }
            None => skipped += 1,
        }
    }
    (entries, skipped)
}

pub(crate) struct PresetsState {
    pub(crate) rows: Vec<Preset>,
    pub(crate) active_kind: PresetKind,
    pub(crate) search: Entity<InputState>,
    pub(crate) new_source: Entity<InputState>,
    pub(crate) new_target: Entity<InputState>,
    pub(crate) import_open: bool,
    pub(crate) importing: bool,
    pub(crate) import_error: Option<SharedString>,
    pub(crate) import_text: Entity<TextareaState>,
    pub(crate) loaded: bool,
    pub(crate) loading: bool,
    pub(crate) error: Option<SharedString>,
}

impl PresetsState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<AppState>) -> Self {
        Self {
            rows: Vec::new(),
            active_kind: PresetKind::Person,
            search: cx.new(|cx| InputState::new(window, cx).placeholder("搜索预设…")),
            new_source: cx.new(|cx| {
                InputState::new(window, cx).placeholder("原文（演员姓名 / 标签 / 类型）")
            }),
            new_target: cx.new(|cx| InputState::new(window, cx).placeholder("预设译名")),
            import_open: false,
            importing: false,
            import_error: None,
            import_text: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("每行一条：原名=译名\nDrama=剧情\nComedy=喜剧")
            }),
            loaded: false,
            loading: false,
            error: None,
        }
    }
}

impl AppState {
    pub(crate) fn load_presets(&mut self, cx: &mut Context<Self>) {
        if self.service.is_none() || self.presets.loading {
            return;
        }
        self.presets.loading = true;
        self.presets.error = None;
        cx.notify();

        self.spawn_job(
            cx,
            |service| service.presets().map_err(ui::error_text),
            |this, result, cx| {
                this.presets.loading = false;
                match result {
                    Ok(rows) => {
                        this.presets.rows = rows;
                        this.presets.loaded = true;
                    }
                    Err(message) => {
                        this.presets.loaded = true;
                        this.presets.error = Some(message.into());
                    }
                }
                cx.notify();
            },
        );
    }

    fn add_preset(&mut self, window: &Window, cx: &mut Context<Self>) {
        let source = self.presets.new_source.read(cx).value().trim().to_string();
        let target = self.presets.new_target.read(cx).value().trim().to_string();
        if source.is_empty() || target.is_empty() {
            self.notify_toast("请填写原文与预设译名", cx);
            return;
        }
        let kind = self.presets.active_kind;

        self.spawn_job_in(
            window,
            cx,
            move |service| {
                service
                    .upsert_preset(kind, &source, &target)
                    .map_err(ui::error_text)
            },
            |this, result, window, cx| {
                match result {
                    Ok(()) => {
                        this.presets
                            .new_source
                            .update(cx, |state, cx| state.set_value("", window, cx));
                        this.presets
                            .new_target
                            .update(cx, |state, cx| state.set_value("", window, cx));
                        this.notify_toast("已添加预设", cx);
                        this.load_presets(cx);
                    }
                    Err(message) => this.notify_toast(message, cx),
                }
                cx.notify();
            },
        );
    }

    fn delete_preset(&mut self, kind: PresetKind, source: String, cx: &mut Context<Self>) {
        let label = source.clone();
        self.spawn_job(
            cx,
            move |service| service.delete_preset(kind, &source).map_err(ui::error_text),
            move |this, result, cx| {
                match result {
                    Ok(()) => {
                        this.notify_toast(format!("已删除预设：{label}"), cx);
                        this.load_presets(cx);
                    }
                    Err(message) => this.notify_toast(message, cx),
                }
                cx.notify();
            },
        );
    }

    fn open_preset_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.presets.importing {
            return;
        }
        self.presets.import_open = true;
        self.presets.import_error = None;
        self.presets
            .import_text
            .update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    fn close_preset_import(&mut self, cx: &mut Context<Self>) {
        if self.presets.importing {
            return;
        }
        self.presets.import_open = false;
        self.presets.import_error = None;
        cx.notify();
    }

    fn run_preset_import(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.presets.importing || self.service.is_none() {
            return;
        }
        let raw = self.presets.import_text.read(cx).value().to_string();
        let (entries, skipped) = parse_preset_import(&raw);
        if entries.is_empty() {
            self.presets.import_error = Some("没有可导入的行（格式：原名=译名，每行一条）".into());
            cx.notify();
            return;
        }
        let kind = self.presets.active_kind;
        self.presets.importing = true;
        self.presets.import_error = None;
        cx.notify();

        self.spawn_job_in(
            window,
            cx,
            move |service| {
                let mut imported = 0usize;
                let mut failed = 0usize;
                let mut first_error = None;
                for (source, target) in entries {
                    match service.upsert_preset(kind, &source, &target) {
                        Ok(()) => imported += 1,
                        Err(error) => {
                            failed += 1;
                            if first_error.is_none() {
                                first_error = Some(ui::error_text(error));
                            }
                        }
                    }
                }
                Ok((imported, failed, skipped, first_error))
            },
            |this, result, _window, cx| {
                this.presets.importing = false;
                match result {
                    Ok((imported, failed, skipped, first_error)) => {
                        this.notify_toast(format!("导入 {imported} 条，跳过 {skipped} 条"), cx);
                        if failed > 0 {
                            this.presets.import_error = Some(
                                format!(
                                    "{failed} 条导入失败：{}",
                                    first_error.unwrap_or_else(|| "未知错误".to_string())
                                )
                                .into(),
                            );
                        }
                        this.load_presets(cx);
                    }
                    Err(message) => {
                        this.presets.import_error = Some(format!("导入失败：{message}").into());
                    }
                }
                cx.notify();
            },
        );
    }

    pub(crate) fn render_preset_import(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if !self.presets.import_open {
            return div().into_any_element();
        }
        let importing = self.presets.importing;
        let kind_label = preset_kind_label(self.presets.active_kind);

        let overlay = div()
            .id("preset-import-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0.0, 0.0, 0.0, 0.52))
            .on_click(cx.listener(|this, _, _, cx| this.close_preset_import(cx)));

        let header = h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .gap_2()
            .items_center()
            .px(px(18.))
            .py(px(14.))
            .border_b_1()
            .border_color(ui::border(theme))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_semibold()
                            .child(format!("批量导入 · {kind_label}预设")),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("每行一条：原名=译名，例如 Drama=剧情"),
                    ),
            )
            .child(
                Button::new("preset-import-close-x")
                    .ghost()
                    .compact()
                    .label("✕")
                    .disabled(importing)
                    .on_click(cx.listener(|this, _, _, cx| this.close_preset_import(cx))),
            );

        let mut body = v_flex().w_full().min_w_0().gap(px(10.)).p(px(18.));
        body = body.child(
            Textarea::new(&self.presets.import_text)
                .h(px(260.))
                .w_full()
                .disabled(importing),
        );
        body = body.child(
            div()
                .w_full()
                .min_w_0()
                .whitespace_normal()
                .text_size(px(11.5))
                .text_color(ui::muted(theme))
                .child("按第一个 = 分割并 trim；空行与缺少 = 的行会被忽略；重复原名会覆盖译名。"),
        );
        if let Some(error) = self.presets.import_error.as_ref() {
            body = body.child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .p(px(10.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(ui::red(theme))
                    .bg(ui::red(theme).opacity(0.10))
                    .text_size(px(12.))
                    .text_color(ui::red(theme))
                    .child(error.clone()),
            );
        }

        let body = div()
            .id("preset-import-body")
            .w_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(body);

        let footer = h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .justify_end()
            .gap_2()
            .px(px(18.))
            .py(px(14.))
            .border_t_1()
            .border_color(ui::border(theme))
            .child(
                Button::new("preset-import-cancel")
                    .ghost()
                    .label("关闭")
                    .disabled(importing)
                    .on_click(cx.listener(|this, _, _, cx| this.close_preset_import(cx))),
            )
            .child(
                Button::new("preset-import-run")
                    .primary()
                    .label("导入")
                    .loading(importing)
                    .disabled(importing)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.run_preset_import(window, cx)),
                    ),
            );

        let card = v_flex()
            .id("preset-import-card")
            .occlude()
            .w_full()
            .max_w(px(680.))
            .h_full()
            .max_h(px(560.))
            .rounded(px(14.))
            .border_1()
            .border_color(ui::border(theme))
            .bg(theme.sidebar)
            .overflow_hidden()
            .child(header)
            .child(body)
            .child(footer);

        v_flex()
            .absolute()
            .inset_0()
            .occlude()
            .items_center()
            .justify_center()
            .p(px(24.))
            .child(overlay)
            .child(card)
            .into_any_element()
    }

    pub(crate) fn render_presets(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let active_kind = self.presets.active_kind;
        let search = self.presets.search.read(cx).value().to_lowercase();
        let needle = search.trim();
        let visible = self
            .presets
            .rows
            .iter()
            .filter(|preset| preset.kind == active_kind)
            .filter(|preset| {
                needle.is_empty()
                    || preset.source.to_lowercase().contains(needle)
                    || preset.target.to_lowercase().contains(needle)
            })
            .collect::<Vec<_>>();

        let header_cell = |text: &'static str, width: Option<Pixels>| {
            let cell = div()
                .text_size(px(11.5))
                .text_color(ui::muted(theme))
                .child(text);
            match width {
                Some(width) => cell.w(width).flex_shrink_0().into_any_element(),
                None => cell.flex_1().min_w_0().into_any_element(),
            }
        };

        let table_header = h_flex()
            .w_full()
            .px(px(12.))
            .h(px(38.))
            .items_center()
            .border_b_1()
            .border_color(ui::border(theme))
            .child(header_cell("原文", None))
            .child(header_cell("译文", None))
            .child(header_cell("使用次数", Some(px(120.))))
            .child(div().w(px(96.)).flex_shrink_0().child(div()));

        let rows = visible.iter().map(|preset| {
            let source = preset.source.clone();
            let delete_source = preset.source.clone();
            let kind = active_kind;
            h_flex()
                .w_full()
                .px(px(12.))
                .h(px(44.))
                .items_center()
                .border_b_1()
                .border_color(ui::border(theme))
                .hover(|style| style.bg(ui::panel2(theme)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.5))
                        .child(preset.source.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.5))
                        .child(preset.target.clone()),
                )
                .child(
                    div()
                        .w(px(120.))
                        .flex_shrink_0()
                        .text_size(px(12.))
                        .text_color(ui::muted(theme))
                        .child(format!("{} 次", preset.uses)),
                )
                .child(
                    div().w(px(96.)).flex_shrink_0().child(
                        Button::new(format!("preset-delete-{:?}-{source}", kind))
                            .small()
                            .ghost()
                            .label("删除")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_preset(kind, delete_source.clone(), cx)
                            })),
                    ),
                )
        });

        let table_body: AnyElement = if visible.is_empty() {
            div()
                .p(px(32.))
                .text_center()
                .text_size(px(12.5))
                .text_color(ui::muted(theme))
                .child(if self.presets.loading {
                    "正在读取预设…".to_string()
                } else if needle.is_empty() {
                    format!("还没有{}预设", preset_kind_label(active_kind))
                } else {
                    "没有匹配项".to_string()
                })
                .into_any_element()
        } else {
            v_flex().w_full().children(rows).into_any_element()
        };

        let mut column = v_flex()
            .w_full()
            .px(px(24.))
            .pb(px(40.))
            .child(
                h_flex()
                    .w_full()
                    .items_end()
                    .flex_wrap()
                    .gap_3()
                    .pt(px(22.))
                    .pb(px(16.))
                    .child(ui::page_head(
                        "翻译预设",
                        "演员姓名、标签、类型三类预设：翻译时完全匹配即直接替换；角色名不套用",
                        theme,
                    ))
                    .child(div().flex_1())
                    .child(
                        Button::new("preset-import-open")
                            .small()
                            .label("导入")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_preset_import(window, cx)
                            })),
                    )
                    .child(Input::new(&self.presets.search).w(px(220.))),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap(px(4.))
                    .border_b_1()
                    .border_color(ui::border(theme))
                    .mb(px(14.))
                    .children([PresetKind::Person, PresetKind::Tag, PresetKind::Genre].map(
                        |kind| {
                            let active = active_kind == kind;
                            let count = self
                                .presets
                                .rows
                                .iter()
                                .filter(|preset| preset.kind == kind)
                                .count();
                            h_flex()
                                .id(format!("preset-tab-{:?}", kind))
                                .gap(px(6.))
                                .px(px(14.))
                                .py(px(9.))
                                .border_b_2()
                                .border_color(if active {
                                    ui::accent()
                                } else {
                                    ui::transparent()
                                })
                                .cursor_pointer()
                                .text_size(px(13.))
                                .text_color(if active {
                                    theme.foreground
                                } else {
                                    ui::muted(theme)
                                })
                                .font_weight(if active {
                                    gpui_kit::FontWeight::SEMIBOLD
                                } else {
                                    gpui_kit::FontWeight::NORMAL
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.presets.active_kind = kind;
                                    cx.notify();
                                }))
                                .child(preset_kind_label(kind))
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(ui::muted(theme))
                                        .child(count.to_string()),
                                )
                        },
                    )),
            )
            .child(
                ui::card(theme).p(px(14.)).mb(px(14.)).child(
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(220.))
                                .child(Input::new(&self.presets.new_source).w_full()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(220.))
                                .child(Input::new(&self.presets.new_target).w_full()),
                        )
                        .child(
                            Button::new("preset-add")
                                .primary()
                                .label("+ 添加")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.add_preset(window, cx)),
                                ),
                        ),
                ),
            )
            .child(ui::card(theme).child(table_header).child(table_body))
            .child(
                div()
                    .mt(px(10.))
                    .text_size(px(11.5))
                    .text_color(ui::muted(theme))
                    .child("演员姓名 / 标签 / 类型分别使用各自的预设；完全匹配（忽略大小写与首尾空格）直接采用译文，未命中才交给翻译服务。"),
            );

        if let Some(error) = self.presets.error.as_ref() {
            column = column.child(
                div()
                    .mt(px(12.))
                    .p(px(10.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(ui::red(theme))
                    .bg(ui::red(theme).opacity(0.10))
                    .text_size(px(12.))
                    .text_color(ui::red(theme))
                    .child(format!("读取预设失败：{error}")),
            );
        }

        column.into_any_element()
    }
}
