//! “选择要同步的媒体库”弹窗：连接成功或点媒体库工具栏时打开。

use std::collections::HashSet;

use ::core::LibraryView;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{h_flex, v_flex, Disableable as _, Sizable as _, StyledExt as _, Theme};
use gpui_kit::*;

use crate::app::{AppState, ErrorAction};
use crate::ui;

/// Reusable library picker state. The same modal is opened from the connect
/// page and from the library toolbar.
#[derive(Default)]
pub(crate) struct SyncPickerState {
    pub(crate) open: bool,
    pub(crate) loading: bool,
    pub(crate) libraries: Vec<LibraryView>,
    pub(crate) selected: HashSet<String>,
    pub(crate) error: Option<SharedString>,
}

impl AppState {
    /// Open the picker and query the server for its media folders.
    pub(crate) fn open_sync_picker(&mut self, cx: &mut Context<Self>) {
        if self.sync.running {
            self.notify_toast("同步正在进行中，请稍候…", cx);
            return;
        }
        if self.connect.connecting {
            self.notify_toast("正在连接服务器，请稍候…", cx);
            return;
        }
        if self.connect.server.is_none() {
            self.show_error_with_action(
                "无法同步",
                "请先在「连接服务器」页连接 Jellyfin，再选择要同步的媒体库。",
                Some(ErrorAction::ConnectServer),
                cx,
            );
            return;
        }

        self.sync_picker.open = true;
        self.sync_picker.loading = true;
        self.sync_picker.error = None;
        self.sync_picker.libraries.clear();
        self.sync_picker.selected.clear();
        cx.notify();
        self.fetch_remote_libraries(cx);
    }

    fn fetch_remote_libraries(&mut self, cx: &mut Context<Self>) {
        if self.service.is_none() {
            self.sync_picker.loading = false;
            self.sync_picker.error = Some("本地数据服务尚未就绪".into());
            cx.notify();
            return;
        }
        self.sync_picker.loading = true;
        self.sync_picker.error = None;
        cx.notify();

        self.spawn_async_job(
            cx,
            |service| {
                Box::pin(async move { service.remote_libraries().await.map_err(ui::error_text) })
            },
            |this, result, cx| {
                this.sync_picker.loading = false;
                match result {
                    Ok(libraries) => {
                        this.sync_picker.selected =
                            libraries.iter().map(|library| library.id.clone()).collect();
                        this.sync_picker.libraries = libraries;
                        this.sync_picker.error = None;
                    }
                    Err(message) => {
                        this.sync_picker.error =
                            Some(format!("读取服务器媒体库失败：{message}").into());
                    }
                }
                cx.notify();
            },
        );
    }

    fn close_sync_picker(&mut self, cx: &mut Context<Self>) {
        if self.sync.running {
            return;
        }
        self.sync_picker.open = false;
        self.sync_picker.error = None;
        cx.notify();
    }

    fn toggle_sync_library(&mut self, id: String, checked: bool, cx: &mut Context<Self>) {
        if checked {
            self.sync_picker.selected.insert(id);
        } else {
            self.sync_picker.selected.remove(&id);
        }
        cx.notify();
    }

    fn select_all_sync_libraries(&mut self, selected: bool, cx: &mut Context<Self>) {
        if selected {
            self.sync_picker.selected = self
                .sync_picker
                .libraries
                .iter()
                .map(|library| library.id.clone())
                .collect();
        } else {
            self.sync_picker.selected.clear();
        }
        cx.notify();
    }

    fn start_selected_sync(&mut self, cx: &mut Context<Self>) {
        if self.sync.running || self.sync_picker.loading {
            return;
        }
        let ids = self
            .sync_picker
            .libraries
            .iter()
            .filter(|library| self.sync_picker.selected.contains(&library.id))
            .map(|library| library.id.clone())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            self.sync_picker.error = Some("请至少选择一个媒体库".into());
            cx.notify();
            return;
        }
        self.sync_picker.error = None;
        self.start_library_sync(ids, cx);
    }

    pub(crate) fn render_sync_picker(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if !self.sync_picker.open {
            return div().into_any_element();
        }

        let running = self.sync.running;
        let loading = self.sync_picker.loading;
        let selected_count = self.sync_picker.selected.len();
        let total = self.sync_picker.libraries.len();
        let all_selected = total > 0 && selected_count == total;

        let overlay = div()
            .id("sync-picker-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0.0, 0.0, 0.0, 0.52))
            .on_click(cx.listener(|this, _, _, cx| this.close_sync_picker(cx)));

        let header = h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .px(px(18.))
            .py(px(14.))
            .gap_2()
            .items_center()
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
                            .child("选择要同步的媒体库"),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("只同步勾选的库；未勾选的库保留本地旧数据。"),
                    ),
            )
            .child(
                Button::new("sync-picker-close")
                    .ghost()
                    .compact()
                    .label("✕")
                    .disabled(running)
                    .on_click(cx.listener(|this, _, _, cx| this.close_sync_picker(cx))),
            );

        let selection_tools = h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(12.))
                    .text_color(ui::muted(theme))
                    .child(format!("已选 {selected_count} / {total}")),
            )
            .child(
                Button::new("sync-picker-all")
                    .small()
                    .ghost()
                    .label("全选")
                    .disabled(loading || running || all_selected)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.select_all_sync_libraries(true, cx)),
                    ),
            )
            .child(
                Button::new("sync-picker-none")
                    .small()
                    .ghost()
                    .label("全不选")
                    .disabled(loading || running || selected_count == 0)
                    .on_click(
                        cx.listener(|this, _, _, cx| this.select_all_sync_libraries(false, cx)),
                    ),
            );

        let mut body = v_flex().w_full().flex_1().min_h_0().gap(px(10.)).p(px(16.));

        if running {
            let text = self
                .sync
                .progress
                .as_ref()
                .map(ui::sync_progress_text)
                .unwrap_or_else(|| "正在准备同步…".to_string());
            body = body.child(
                div()
                    .w_full()
                    .min_w_0()
                    .whitespace_normal()
                    .p(px(10.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(ui::accent_soft(0.4))
                    .bg(ui::accent_soft(0.10))
                    .text_size(px(12.))
                    .text_color(ui::accent())
                    .child(text),
            );
        }

        if let Some(error) = self.sync_picker.error.as_ref() {
            body = body.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .p(px(10.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(ui::red(theme))
                    .bg(ui::red(theme).opacity(0.10))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(px(12.))
                            .text_color(ui::red(theme))
                            .child(error.clone()),
                    )
                    .child(
                        Button::new("sync-picker-retry")
                            .small()
                            .label("重试")
                            .disabled(running || loading)
                            .on_click(
                                cx.listener(|this, _, _, cx| this.fetch_remote_libraries(cx)),
                            ),
                    ),
            );
        }

        body = body.child(selection_tools);

        let list: AnyElement = if loading {
            div()
                .w_full()
                .p(px(24.))
                .text_center()
                .text_size(px(12.5))
                .text_color(ui::muted(theme))
                .child("正在读取服务器媒体库…")
                .into_any_element()
        } else if self.sync_picker.libraries.is_empty() {
            div()
                .w_full()
                .p(px(24.))
                .text_center()
                .text_size(px(12.5))
                .text_color(ui::muted(theme))
                .child("服务器上没有可同步的媒体库")
                .into_any_element()
        } else {
            v_flex()
                .w_full()
                .min_w_0()
                .children(self.sync_picker.libraries.iter().map(|library| {
                    let selected = self.sync_picker.selected.contains(&library.id);
                    let id = library.id.clone();
                    let checkbox_id = format!("sync-lib-{}", library.id);
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .h(px(42.))
                        .px(px(10.))
                        .gap_2()
                        .items_center()
                        .rounded(px(8.))
                        .hover(|style| style.bg(theme.muted))
                        .child(
                            Checkbox::new(checkbox_id)
                                .checked(selected)
                                .disabled(loading || running)
                                .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                                    this.toggle_sync_library(id.clone(), *checked, cx)
                                })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(12.5))
                                .child(library.name.clone()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .text_color(ui::muted(theme))
                                .child(format!("本地 {} 条", library.item_count)),
                        )
                }))
                .into_any_element()
        };

        body = body.child(
            div()
                .id("sync-picker-list")
                .w_full()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(list),
        );

        let footer = h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .justify_between()
            .gap_2()
            .px(px(18.))
            .py(px(14.))
            .border_t_1()
            .border_color(ui::border(theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(11.5))
                    .text_color(ui::muted(theme))
                    .child(format!("已选 {selected_count} 个媒体库")),
            )
            .child(
                Button::new("sync-picker-cancel")
                    .ghost()
                    .label("取消")
                    .disabled(running)
                    .on_click(cx.listener(|this, _, _, cx| this.close_sync_picker(cx))),
            )
            .child(
                Button::new("sync-picker-start")
                    .primary()
                    .label(format!("同步选中（{selected_count}）"))
                    .loading(running)
                    .disabled(running || loading || selected_count == 0)
                    .on_click(cx.listener(|this, _, _, cx| this.start_selected_sync(cx))),
            );

        let card = v_flex()
            .id("sync-picker-card")
            .occlude()
            .w_full()
            .max_w(px(560.))
            .h_full()
            .max_h(px(620.))
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
}
