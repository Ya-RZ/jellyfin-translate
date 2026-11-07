//! 视频元数据抓取弹窗（MetaTube）。
//!
//! 抽屉里点「抓取元数据」→ 本弹窗预填原名 → 搜索候选 → 选一条取详情 →
//! 结果直接填进本地草稿并打开编辑弹窗，由用户确认后再写回 Jellyfin。

use ::core::MovieSearchResult;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, Disableable as _, StyledExt as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AppState;
use crate::ui;

pub(crate) struct MetatubeState {
    pub(crate) open: bool,
    /// Item the scrape fills; cleared when the dialog closes.
    pub(crate) item_id: Option<String>,
    pub(crate) query: Entity<InputState>,
    pub(crate) provider: Entity<InputState>,
    pub(crate) searching: bool,
    pub(crate) applying: bool,
    /// A search has completed (used to tell "no results" apart from "not asked").
    pub(crate) searched: bool,
    pub(crate) error: Option<SharedString>,
    pub(crate) results: Vec<MovieSearchResult>,
}

impl MetatubeState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<AppState>) -> Self {
        Self {
            open: false,
            item_id: None,
            query: cx
                .new(|cx| InputState::new(window, cx).placeholder("番号或关键词，例如 IPX-001")),
            provider: cx.new(|cx| InputState::new(window, cx).placeholder("留空=全部来源（较慢）")),
            searching: false,
            applying: false,
            searched: false,
            error: None,
            results: Vec::new(),
        }
    }

    fn busy(&self) -> bool {
        self.searching || self.applying
    }
}

fn release_date_label(raw: &str) -> String {
    let raw = raw.trim();
    // Go's zero time for "unknown date".
    if raw.is_empty() || raw.starts_with("0001-01-01") {
        return "—".to_string();
    }
    raw.split('T').next().unwrap_or(raw).to_string()
}

impl AppState {
    pub(crate) fn open_metatube_picker(
        &mut self,
        item_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.library.item_by_id(&item_id).cloned() else {
            return;
        };
        if item.locked {
            self.notify_toast("已锁定，先解锁再抓取", cx);
            return;
        }

        let keyword = if item.original_title.trim().is_empty() {
            item.title.trim().to_string()
        } else {
            item.original_title.trim().to_string()
        };

        self.metatube.item_id = Some(item_id);
        self.metatube.results.clear();
        self.metatube.searched = false;
        self.metatube.searching = false;
        self.metatube.applying = false;
        self.metatube.error = None;
        self.metatube.open = true;
        self.metatube
            .query
            .update(cx, |state, cx| state.set_value(keyword, window, cx));
        self.metatube
            .provider
            .update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    pub(crate) fn close_metatube_picker(&mut self, cx: &mut Context<Self>) {
        if self.metatube.busy() {
            return;
        }
        self.metatube.open = false;
        self.metatube.item_id = None;
        self.metatube.results.clear();
        self.metatube.searched = false;
        self.metatube.error = None;
        cx.notify();
    }

    fn run_metatube_search(&mut self, cx: &mut Context<Self>) {
        if self.metatube.busy() {
            return;
        }
        if self.service.is_none() {
            self.metatube.error = Some("本地数据服务尚未就绪".into());
            cx.notify();
            return;
        }
        let query = self.metatube.query.read(cx).value().trim().to_string();
        if query.is_empty() {
            self.metatube.error = Some("请输入搜索关键词".into());
            cx.notify();
            return;
        }
        let provider = self.metatube.provider.read(cx).value().trim().to_string();
        let provider = (!provider.is_empty()).then_some(provider);

        self.metatube.searching = true;
        self.metatube.searched = false;
        self.metatube.error = None;
        self.metatube.results.clear();
        cx.notify();

        self.spawn_async_job(
            cx,
            move |service| {
                Box::pin(async move {
                    service
                        .metatube_search(query, provider)
                        .await
                        .map_err(ui::error_text)
                })
            },
            |this, result, cx| {
                this.metatube.searching = false;
                this.metatube.searched = true;
                match result {
                    Ok(results) => this.metatube.results = results,
                    Err(message) => this.metatube.error = Some(message.into()),
                }
                cx.notify();
            },
        );
    }

    fn apply_metatube_candidate(
        &mut self,
        provider: String,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.metatube.busy() {
            return;
        }
        if self.service.is_none() {
            self.metatube.error = Some("本地数据服务尚未就绪".into());
            cx.notify();
            return;
        }
        let Some(item_id) = self.metatube.item_id.clone() else {
            return;
        };

        self.metatube.applying = true;
        self.metatube.error = None;
        cx.notify();

        self.spawn_async_job_in(
            window,
            cx,
            move |service| {
                Box::pin(async move {
                    let info = service
                        .metatube_detail(provider, id, true)
                        .await
                        .map_err(ui::error_text)?;
                    service
                        .apply_metatube_metadata(&item_id, &info)
                        .map_err(ui::error_text)
                })
            },
            move |this, result, window, cx| {
                this.metatube.applying = false;
                match result {
                    Ok(item) => {
                        let item_id = item.id.clone();
                        this.cache_item(item);
                        this.metatube.open = false;
                        this.metatube.item_id = None;
                        this.metatube.results.clear();
                        this.metatube.searched = false;
                        this.open_edit(item_id, window, cx);
                        this.notify_toast("已填入草稿，核对后点写回", cx);
                    }
                    Err(message) => this.metatube.error = Some(message.into()),
                }
                cx.notify();
            },
        );
    }

    pub(crate) fn render_metatube_picker(
        &self,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.metatube.open {
            return div().into_any_element();
        }
        let busy = self.metatube.busy();
        let searching = self.metatube.searching;
        let applying = self.metatube.applying;

        let overlay = div()
            .id("metatube-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0.0, 0.0, 0.0, 0.52))
            .on_click(cx.listener(|this, _, _, cx| this.close_metatube_picker(cx)));

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
                            .child("抓取视频元数据"),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("来源：MetaTube 服务（地址与 Token 在设置页配置）。结果只填入本地草稿，不会自动写回 Jellyfin。"),
                    ),
            )
            .child(
                Button::new("metatube-close-x")
                    .ghost()
                    .compact()
                    .label("✕")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.close_metatube_picker(cx))),
            );

        let search_bar = h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .items_end()
            .px(px(18.))
            .py(px(14.))
            .border_b_1()
            .border_color(ui::border(theme))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(220.))
                    .gap(px(6.))
                    .child(ui::field_label("关键词", theme))
                    .child(Input::new(&self.metatube.query).w_full()),
            )
            .child(
                v_flex()
                    .w(px(200.))
                    .flex_shrink_0()
                    .gap(px(6.))
                    .child(ui::field_label("Provider（可选）", theme))
                    .child(Input::new(&self.metatube.provider).w_full()),
            )
            .child(
                Button::new("metatube-search")
                    .primary()
                    .label("搜索")
                    .loading(searching)
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.run_metatube_search(cx))),
            );

        let mut list = v_flex().w_full().min_w_0().gap(px(8.));

        if let Some(error) = self.metatube.error.as_ref() {
            list = list.child(
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

        if searching {
            list = list.child(
                div()
                    .w_full()
                    .text_size(px(12.))
                    .text_color(ui::muted(theme))
                    .child("正在搜索，全部来源可能耗时十几秒…"),
            );
        } else if applying {
            list = list.child(
                div()
                    .w_full()
                    .text_size(px(12.))
                    .text_color(ui::accent())
                    .child("正在抓取详情并写入草稿…"),
            );
        } else if self.metatube.searched && self.metatube.results.is_empty() {
            list = list.child(
                div()
                    .w_full()
                    .text_size(px(12.))
                    .text_color(ui::muted(theme))
                    .child("没有找到匹配的影片。换个番号，或指定 Provider 再试。"),
            );
        } else if !self.metatube.searched {
            list = list.child(
                div()
                    .w_full()
                    .text_size(px(12.))
                    .text_color(ui::muted(theme))
                    .child("输入关键词后点「搜索」。默认用条目的原名；原名通常就是番号。"),
            );
        }

        for (index, result) in self.metatube.results.iter().enumerate() {
            let provider = result.provider.clone();
            let id = result.id.clone();

            let actors = if result.actors.is_empty() {
                String::new()
            } else {
                result.actors.join(" / ")
            };
            let meta = [
                result.number.trim().to_string(),
                result.provider.trim().to_string(),
                release_date_label(&result.release_date),
            ]
            .into_iter()
            .filter(|part| !part.is_empty() && part != "—")
            .collect::<Vec<_>>()
            .join(" · ");

            list = list.child(
                h_flex()
                    .id(("metatube-result", index))
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .items_center()
                    .p(px(10.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(ui::border(theme))
                    .bg(ui::panel(theme))
                    .cursor_pointer()
                    .when(busy, |row| row.opacity(0.5))
                    .hover(|style| style.border_color(ui::accent_soft(0.6)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.apply_metatube_candidate(provider.clone(), id.clone(), window, cx)
                    }))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(3.))
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.5))
                                    .font_semibold()
                                    .child(if result.title.trim().is_empty() {
                                        result.id.clone()
                                    } else {
                                        result.title.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(ui::muted(theme))
                                    .child(meta),
                            )
                            .when(!actors.is_empty(), |column| {
                                column.child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(11.))
                                        .text_color(ui::muted(theme))
                                        .child(actors),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.5))
                            .text_color(ui::accent())
                            .child("选择 ›"),
                    ),
            );
        }

        let count = self.metatube.results.len();
        let body = div()
            .id("metatube-body")
            .w_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(18.))
            .child(list);

        let footer = h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .items_center()
            .justify_end()
            .gap_2()
            .px(px(18.))
            .py(px(14.))
            .border_t_1()
            .border_color(ui::border(theme))
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.5))
                    .text_color(ui::muted(theme))
                    .child(if self.metatube.searched {
                        format!("{count} 条结果")
                    } else {
                        String::new()
                    }),
            )
            .child(
                Button::new("metatube-close")
                    .ghost()
                    .label("关闭")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.close_metatube_picker(cx))),
            );

        let card = v_flex()
            .id("metatube-card")
            .occlude()
            .w_full()
            .max_w(px(760.))
            .h_full()
            .max_h(px(620.))
            .rounded(px(14.))
            .border_1()
            .border_color(ui::border(theme))
            .bg(theme.sidebar)
            .overflow_hidden()
            .child(header)
            .child(search_bar)
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
