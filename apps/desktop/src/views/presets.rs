//! 演员预设页：表格、搜索、添加与删除。

use ::core::Preset;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, Sizable as _, Theme};
use gpui_kit::*;

use crate::app::AppState;
use crate::ui;

pub(crate) struct PresetsState {
    pub(crate) rows: Vec<Preset>,
    pub(crate) search: Entity<InputState>,
    pub(crate) new_source: Entity<InputState>,
    pub(crate) new_target: Entity<InputState>,
    pub(crate) loaded: bool,
    pub(crate) loading: bool,
    pub(crate) error: Option<SharedString>,
}

impl PresetsState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<AppState>) -> Self {
        Self {
            rows: Vec::new(),
            search: cx.new(|cx| InputState::new(window, cx).placeholder("搜索演员…")),
            new_source: cx
                .new(|cx| InputState::new(window, cx).placeholder("演员原文名，例如 Tim Robbins")),
            new_target: cx
                .new(|cx| InputState::new(window, cx).placeholder("预设译名，例如 蒂姆·罗宾斯")),
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
            self.notify_toast("请填写演员原文名与预设译名", cx);
            return;
        }

        self.spawn_job_in(
            window,
            cx,
            move |service| {
                service
                    .upsert_preset(&source, &target)
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

    fn delete_preset(&mut self, source: String, cx: &mut Context<Self>) {
        let label = source.clone();
        self.spawn_job(
            cx,
            move |service| service.delete_preset(&source).map_err(ui::error_text),
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

    pub(crate) fn render_presets(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let search = self.presets.search.read(cx).value().to_lowercase();
        let needle = search.trim();
        let visible = self
            .presets
            .rows
            .iter()
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
            .child(header_cell("原文名", None))
            .child(header_cell("预设译名", None))
            .child(header_cell("使用次数", Some(px(120.))))
            .child(div().w(px(96.)).flex_shrink_0().child(div()));

        let rows = visible.iter().map(|preset| {
            let source = preset.source.clone();
            let delete_source = preset.source.clone();
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
                        Button::new(format!("preset-delete-{source}"))
                            .small()
                            .ghost()
                            .label("删除")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_preset(delete_source.clone(), cx)
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
                    "还没有演员预设".to_string()
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
                        "演员预设",
                        "演员姓名完全匹配时直接采用预设译名；角色名不套用",
                        theme,
                    ))
                    .child(div().flex_1())
                    .child(Input::new(&self.presets.search).w(px(220.))),
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
                    .child("翻译演职员时先查本表；命中直接用预设值，未命中才交给翻译服务。"),
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
