//! 任务中心页：静态任务队列占位 + 最近一次翻译/写回结果。

use gpui_kit::component::{h_flex, v_flex, StyledExt as _, Theme};
use gpui_kit::*;

use crate::app::AppState;
use crate::ui;

/// Result of the most recent translate/write/connect operation.
pub(crate) struct TaskRecord {
    pub(crate) kind: String,
    pub(crate) detail: SharedString,
    pub(crate) ok: bool,
}

#[derive(Default)]
pub(crate) struct TasksState {
    pub(crate) last: Option<TaskRecord>,
}

impl AppState {
    pub(crate) fn render_tasks(&self, theme: &Theme, _cx: &mut Context<Self>) -> AnyElement {
        let status_pill = |text: &'static str, color: Hsla, background: Hsla| {
            div()
                .px(px(8.))
                .py(px(2.))
                .rounded_full()
                .bg(background)
                .text_size(px(11.))
                .text_color(color)
                .child(text)
        };

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

        let row = |title: &'static str, scope: &'static str, status: &'static str, color: Hsla| {
            h_flex()
                .w_full()
                .px(px(12.))
                .h(px(48.))
                .items_center()
                .border_b_1()
                .border_color(ui::border(theme))
                .child(div().flex_1().min_w_0().text_size(px(12.5)).child(title))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.))
                        .text_color(ui::muted(theme))
                        .child(scope),
                )
                .child(div().w(px(120.)).flex_shrink_0().child(status_pill(
                    status,
                    color,
                    color.opacity(0.14),
                )))
                .child(
                    div()
                        .w(px(160.))
                        .flex_shrink_0()
                        .text_size(px(11.5))
                        .text_color(ui::muted(theme))
                        .child("—"),
                )
        };

        let last_card: AnyElement = match self.tasks.last.as_ref() {
            Some(last) => {
                let color = if last.ok {
                    ui::green(theme)
                } else {
                    ui::red(theme)
                };
                ui::card(theme)
                    .p(px(16.))
                    .mb(px(14.))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .child(
                                div()
                                    .px(px(8.))
                                    .py(px(2.))
                                    .rounded_full()
                                    .bg(color.opacity(0.14))
                                    .text_size(px(11.))
                                    .text_color(color)
                                    .child(if last.kind.is_empty() {
                                        "操作".to_string()
                                    } else {
                                        last.kind.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(12.5))
                                    .child(last.detail.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(ui::muted(theme))
                                    .child("最近一次结果"),
                            ),
                    )
                    .into_any_element()
            }
            None => ui::card(theme)
                .p(px(16.))
                .mb(px(14.))
                .child(
                    div()
                        .text_size(px(12.5))
                        .text_color(ui::muted(theme))
                        .child("还没有执行过翻译或写回。到媒体库选中条目后开始。"),
                )
                .into_any_element(),
        };

        v_flex()
            .w_full()
            .px(px(24.))
            .pb(px(40.))
            .child(h_flex().pt(px(22.)).pb(px(16.)).child(ui::page_head(
                "任务中心",
                "翻译与写回都在这里排队；锁定条目会被跳过",
                theme,
            )))
            .child(last_card)
            .child(
                div()
                    .mb(px(8.))
                    .text_size(px(13.5))
                    .font_semibold()
                    .child("任务队列"),
            )
            .child(
                ui::card(theme)
                    .child(
                        h_flex()
                            .w_full()
                            .px(px(12.))
                            .h(px(38.))
                            .items_center()
                            .border_b_1()
                            .border_color(ui::border(theme))
                            .child(header_cell("任务", None))
                            .child(header_cell("范围", None))
                            .child(header_cell("状态", Some(px(120.))))
                            .child(header_cell("进度", Some(px(160.)))),
                    )
                    .child(row(
                        "翻译任务",
                        "由媒体库的「翻译选中 / 翻译此视频」触发",
                        "空闲",
                        ui::muted(theme),
                    ))
                    .child(row(
                        "写回任务",
                        "由媒体库的「写回选中 / 写回服务器」触发",
                        "空闲",
                        ui::muted(theme),
                    ))
                    .child(row(
                        "同步任务",
                        "本地索引同步（后续版本接入任务中心）",
                        "未接入",
                        ui::amber(theme),
                    )),
            )
            .child(
                div()
                    .mt(px(10.))
                    .text_size(px(11.5))
                    .text_color(ui::muted(theme))
                    .child("当前版本任务中心只展示最近一次结果；完整队列将在后续迭代接入。"),
            )
            .into_any_element()
    }
}
