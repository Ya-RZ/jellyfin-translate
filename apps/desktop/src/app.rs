//! Root view for the UI/UX v2 desktop shell.
//!
//! The view owns all UI state. Every database / network call runs on the
//! shared tokio runtime and reports back through an unbounded channel that a
//! GPUI foreground task drains; the UI thread never blocks and never calls
//! `block_on`. A single [`AppService`] is held behind an [`Arc`] and cloned
//! into every background job.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use ::core::{AppService, SyncProgress, SyncReport};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, StyledExt as _, Theme,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::ui;
use crate::views::{connect, library, presets, settings, tasks};

/// Pages reachable from the sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    Library,
    Tasks,
    Presets,
    Settings,
    Connect,
}

impl Page {
    fn crumb(self) -> &'static str {
        match self {
            Page::Library => "媒体库",
            Page::Tasks => "任务中心",
            Page::Presets => "演员预设",
            Page::Settings => "设置",
            Page::Connect => "连接服务器",
        }
    }
}

/// A boxed fallible background job that can be polled by tokio.
pub(crate) type JobFuture<T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'static>>;

/// State of the one-shot "同步媒体库" job. Progress is shown on the connect
/// page and in the library empty state; the job runs on the tokio runtime.
#[derive(Default)]
pub(crate) struct SyncState {
    pub(crate) running: bool,
    pub(crate) progress: Option<SyncProgress>,
    pub(crate) report: Option<SyncReport>,
    pub(crate) error: Option<SharedString>,
}

/// Progress or completion of a library sync, delivered from the tokio task.
enum SyncEvent {
    Progress(SyncProgress),
    Finished(SyncReport),
    Failed(String),
}

/// Root entity: page routing, the shared service handle, page state and the
/// transient drawer/modal/toast overlays.
pub(crate) struct AppState {
    pub(crate) runtime: tokio::runtime::Handle,
    pub(crate) service: Option<Arc<AppService>>,
    pub(crate) service_error: Option<SharedString>,
    pub(crate) page: Page,
    pub(crate) toast: Option<SharedString>,
    started: bool,

    pub(crate) library: library::LibraryState,
    pub(crate) settings: settings::SettingsState,
    pub(crate) presets: presets::PresetsState,
    pub(crate) tasks: tasks::TasksState,
    pub(crate) connect: connect::ConnectState,
    pub(crate) sync: SyncState,
}

impl AppState {
    pub(crate) fn new(
        runtime: tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let library = library::LibraryState::new(window, cx);
        let presets = presets::PresetsState::new(window, cx);

        // Filtering reads the input values during render, so a keystroke must
        // repaint the page rather than only the input itself.
        cx.observe(&library.search, |_, _, cx| cx.notify()).detach();
        cx.observe(&presets.search, |_, _, cx| cx.notify()).detach();

        Self {
            runtime,
            service: None,
            service_error: None,
            page: Page::Library,
            toast: None,
            started: false,
            library,
            settings: settings::SettingsState::new(window, cx),
            presets,
            tasks: tasks::TasksState::default(),
            connect: connect::ConnectState::new(window, cx),
            sync: SyncState::default(),
        }
    }

    /// Open the local service once, then load the initial page data.
    ///
    /// `AppService::open()` is synchronous and may still be a stub while the
    /// backend lands, so it runs on the tokio runtime: a panic or error leaves
    /// the shell usable with an inline banner instead of aborting the process.
    fn start_service(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<AppService, String>>();
        self.runtime.spawn(async move {
            let result = AppService::open().map_err(ui::error_text);
            let _ = tx.send(result);
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = rx.recv().await;
            this.update_in(cx, |this, window, cx| {
                match result {
                    Some(Ok(service)) => {
                        this.service = Some(Arc::new(service));
                        this.service_error = None;
                        this.load_libraries(cx);
                        this.load_settings(window, cx);
                        this.load_presets(cx);
                    }
                    Some(Err(message)) => this.service_error = Some(message.into()),
                    None => this.service_error = Some("本地数据服务启动失败".into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Navigate from another page module.
    pub(crate) fn navigate_external(
        &mut self,
        page: Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate(page, window, cx);
    }

    /// Route to a page, lazily loading data the page needs.
    fn navigate(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if self.page == page {
            return;
        }
        self.page = page;
        match page {
            Page::Settings if !self.settings.loaded && !self.settings.loading => {
                self.load_settings(window, cx);
            }
            Page::Presets if !self.presets.loaded && !self.presets.loading => {
                self.load_presets(cx);
            }
            _ => {}
        }
        cx.notify();
    }

    fn selected_item(&self) -> Option<::core::ItemView> {
        self.library.drawer_item()
    }

    // ---------------------------------------------------------------- helpers

    /// Run a synchronous service call on the tokio runtime, then apply the
    /// result on the UI thread. Panics in the worker become an error message.
    pub(crate) fn spawn_job<T, F>(
        &mut self,
        cx: &mut Context<Self>,
        task: F,
        then: impl FnOnce(&mut Self, Result<T, String>, &mut Context<Self>) + 'static,
    ) where
        T: Send + 'static,
        F: FnOnce(Arc<AppService>) -> Result<T, String> + Send + 'static,
    {
        let Some(service) = self.service.clone() else {
            self.service_error = Some("本地数据服务尚未就绪".into());
            cx.notify();
            return;
        };

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<T, String>>();
        self.runtime.spawn(async move {
            let _ = tx.send(task(service));
        });
        cx.spawn(async move |this, cx| {
            let result = match rx.recv().await {
                Some(result) => result,
                None => Err("后台任务意外结束".to_string()),
            };
            this.update(cx, |this, cx| {
                then(this, result, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Window-aware variant of [`Self::spawn_job`], used when the result must
    /// also write into input state (which needs a `Window`).
    pub(crate) fn spawn_job_in<T, F>(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        task: F,
        then: impl FnOnce(&mut Self, Result<T, String>, &mut Window, &mut Context<Self>) + 'static,
    ) where
        T: Send + 'static,
        F: FnOnce(Arc<AppService>) -> Result<T, String> + Send + 'static,
    {
        let Some(service) = self.service.clone() else {
            self.service_error = Some("本地数据服务尚未就绪".into());
            cx.notify();
            return;
        };

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<T, String>>();
        self.runtime.spawn(async move {
            let _ = tx.send(task(service));
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match rx.recv().await {
                Some(result) => result,
                None => Err("后台任务意外结束".to_string()),
            };
            this.update_in(cx, |this, window, cx| {
                then(this, result, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run an async service call on the tokio runtime.
    pub(crate) fn spawn_async_job_in<T>(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        task: impl FnOnce(Arc<AppService>) -> JobFuture<T> + Send + 'static,
        then: impl FnOnce(&mut Self, Result<T, String>, &mut Window, &mut Context<Self>) + 'static,
    ) where
        T: Send + 'static,
    {
        let Some(service) = self.service.clone() else {
            self.service_error = Some("本地数据服务尚未就绪".into());
            cx.notify();
            return;
        };

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<T, String>>();
        self.runtime.spawn(async move {
            let result = task(service).await;
            let _ = tx.send(result);
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = match rx.recv().await {
                Some(result) => result,
                None => Err("后台任务意外结束".to_string()),
            };
            this.update_in(cx, |this, window, cx| {
                then(this, result, window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Show a toast for a few seconds without blocking the UI thread.
    pub(crate) fn notify_toast(
        &mut self,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let message = message.into();
        self.toast = Some(message.clone());
        cx.notify();

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(3)).await;
            this.update(cx, |this, cx| {
                if this.toast.as_ref() == Some(&message) {
                    this.toast = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Record the outcome of a translate/write/connect job for 任务中心.
    pub(crate) fn record_task(
        &mut self,
        kind: &str,
        detail: impl Into<SharedString>,
        ok: bool,
        cx: &mut Context<Self>,
    ) {
        self.tasks.last = Some(tasks::TaskRecord {
            kind: kind.to_string(),
            detail: detail.into(),
            ok,
        });
        cx.notify();
    }

    /// Mirror every Jellyfin library into the local store.
    ///
    /// `sync_all` reports progress through a callback; the callback forwards
    /// each update into an unbounded channel whose receiver is polled by a
    /// GPUI foreground task, so neither the UI thread nor the progress channel
    /// ever blocks.
    pub(crate) fn start_sync(&mut self, cx: &mut Context<Self>) {
        if self.sync.running {
            return;
        }
        let Some(service) = self.service.clone() else {
            self.service_error = Some("本地数据服务尚未就绪".into());
            cx.notify();
            return;
        };

        self.sync.running = true;
        self.sync.progress = None;
        self.sync.report = None;
        self.sync.error = None;
        cx.notify();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SyncEvent>();
        self.runtime.spawn(async move {
            let progress_tx = tx.clone();
            let result = service
                .sync_all(move |progress| {
                    let _ = progress_tx.send(SyncEvent::Progress(progress));
                })
                .await;
            let event = match result {
                Ok(report) => SyncEvent::Finished(report),
                Err(error) => SyncEvent::Failed(ui::error_text(error)),
            };
            let _ = tx.send(event);
        });

        cx.spawn(async move |this, cx| {
            let mut saw_terminal = false;
            while let Some(event) = rx.recv().await {
                if matches!(&event, SyncEvent::Finished(_) | SyncEvent::Failed(_)) {
                    saw_terminal = true;
                }
                let updated = this
                    .update(cx, |this, cx| {
                        match event {
                            SyncEvent::Progress(progress) => {
                                this.sync.progress = Some(progress);
                            }
                            SyncEvent::Finished(report) => {
                                let message = format!(
                                    "同步完成：{} 个媒体库，{} 个条目，用时 {} ms",
                                    report.libraries, report.items, report.elapsed_ms
                                );
                                this.sync.running = false;
                                this.sync.progress = None;
                                this.sync.report = Some(report);
                                this.sync.error = None;
                                this.record_task("同步", message.clone(), true, cx);
                                this.notify_toast(message, cx);
                                // The mirror changed underneath us: drop the
                                // cached view before re-reading libraries.
                                this.library.items.clear();
                                this.library.selected.clear();
                                this.library.drawer = None;
                                this.library.active_lib = None;
                                this.load_libraries(cx);
                            }
                            SyncEvent::Failed(message) => {
                                this.sync.running = false;
                                this.sync.progress = None;
                                this.sync.error = Some(message.clone().into());
                                this.record_task("同步", message.clone(), false, cx);
                                this.notify_toast(message, cx);
                            }
                        }
                        cx.notify();
                    })
                    .is_ok();
                if saw_terminal || !updated {
                    break;
                }
            }

            // The channel closed without a terminal event (worker panic or a
            // cancelled task): never leave the UI stuck in "syncing".
            if !saw_terminal {
                this.update(cx, |this, cx| {
                    if this.sync.running {
                        this.sync.running = false;
                        this.sync.progress = None;
                        this.sync.error = Some("同步任务意外结束".into());
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    // ----------------------------------------------------------------- render

    fn render_sidebar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let nav_item =
            |page: Page, icon: &'static str, label: &'static str, badge: Option<usize>| {
                let active = self.page == page;
                let mut row = h_flex()
                    .id(("nav", page as usize))
                    .w_full()
                    .h(px(38.))
                    .px(px(10.))
                    .gap_2()
                    .rounded(px(9.))
                    .cursor_pointer()
                    .bg(if active {
                        ui::accent_soft(0.16)
                    } else {
                        ui::transparent()
                    })
                    .hover(move |style| style.bg(theme.muted))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.navigate(page, window, cx)
                    }))
                    .child(
                        div()
                            .w(px(18.))
                            .flex_shrink_0()
                            .text_center()
                            .text_size(px(14.))
                            .text_color(if active {
                                ui::accent()
                            } else {
                                ui::muted(theme)
                            })
                            .child(icon),
                    )
                    .child(
                        div()
                            .text_size(px(13.5))
                            .text_color(if active {
                                theme.foreground
                            } else {
                                ui::muted(theme)
                            })
                            .child(label),
                    );

                if let Some(badge) = badge.filter(|count| *count > 0) {
                    row = row.child(
                        div()
                            .ml_auto()
                            .px(px(7.))
                            .py(px(1.))
                            .rounded_full()
                            .bg(theme.muted)
                            .text_size(px(11.))
                            .text_color(ui::muted(theme))
                            .child(badge.to_string()),
                    );
                }
                row
            };

        let connection = match self.connect.server.as_ref() {
            Some(server) => format!("{} · {}", server.name, server.version),
            None => "未连接".to_string(),
        };
        let connected = self.connect.server.is_some();

        v_flex()
            .w(px(232.))
            .h_full()
            .flex_shrink_0()
            .px(px(12.))
            .py(px(16.))
            .gap(px(4.))
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(ui::border(theme))
            .child(
                h_flex()
                    .gap(px(10.))
                    .px(px(8.))
                    .pb(px(18.))
                    .child(
                        div()
                            .size(px(32.))
                            .rounded(px(9.))
                            .bg(ui::brand_background())
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(14.))
                            .font_semibold()
                            .text_color(hsla(0.0, 0.0, 1.0, 1.0))
                            .child("JT"),
                    )
                    .child(
                        v_flex()
                            .gap(px(1.))
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .font_semibold()
                                    .child("Jellyfin Translate"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(ui::muted(theme))
                                    .child("元数据汉化工作台"),
                            ),
                    ),
            )
            .child(
                div()
                    .px(px(10.))
                    .pt(px(14.))
                    .pb(px(6.))
                    .text_size(px(11.))
                    .text_color(ui::muted(theme))
                    .child("工作区"),
            )
            .child(nav_item(Page::Library, "▦", "媒体库", None))
            .child(nav_item(
                Page::Tasks,
                "◷",
                "任务中心",
                if self.tasks.last.is_some() {
                    Some(1)
                } else {
                    None
                },
            ))
            .child(nav_item(
                Page::Presets,
                "☺",
                "演员预设",
                Some(self.presets.rows.len()),
            ))
            .child(
                div()
                    .px(px(10.))
                    .pt(px(14.))
                    .pb(px(6.))
                    .text_size(px(11.))
                    .text_color(ui::muted(theme))
                    .child("系统"),
            )
            .child(nav_item(Page::Settings, "⚙", "设置", None))
            .child(nav_item(Page::Connect, "⏻", "连接服务器", None))
            .child(div().flex_1())
            .child(
                h_flex()
                    .px(px(8.))
                    .pt(px(10.))
                    .gap_2()
                    .border_t_1()
                    .border_color(ui::border(theme))
                    .child(div().size(px(8.)).rounded_full().bg(if connected {
                        ui::green(theme)
                    } else {
                        ui::muted(theme)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(ui::muted(theme))
                            .child(connection),
                    ),
            )
    }

    fn render_topbar(&self, theme: &Theme) -> impl IntoElement {
        let connection = match self.connect.server.as_ref() {
            Some(server) => format!("{} · v{}", server.name, server.version),
            None => "未连接服务器".to_string(),
        };

        let (parent, leaf) = match self.page {
            Page::Library => (
                Some("媒体库"),
                self.library
                    .active_library()
                    .map(|library| library.name.clone())
                    .unwrap_or_else(|| "全部".to_string()),
            ),
            page => (None, page.crumb().to_string()),
        };

        h_flex()
            .h(px(58.))
            .flex_shrink_0()
            .w_full()
            .px(px(20.))
            .gap_3()
            .border_b_1()
            .border_color(ui::border(theme))
            .bg(theme.group_box)
            .when_some(parent, |row, parent| {
                row.child(
                    div()
                        .text_size(px(13.))
                        .text_color(ui::muted(theme))
                        .child(format!("{parent} / ")),
                )
            })
            .child(div().text_size(px(13.)).font_semibold().child(leaf))
            .child(div().flex_1())
            .when(self.page == Page::Library, |row| {
                row.child(Input::new(&self.library.search).w(px(260.)).h(px(34.)))
            })
            .child(
                h_flex()
                    .gap_2()
                    .px(px(9.))
                    .h(px(34.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(ui::border(theme))
                    .bg(theme.muted)
                    .child(
                        div()
                            .size(px(8.))
                            .rounded_full()
                            .bg(if self.connect.server.is_some() {
                                ui::green(theme)
                            } else {
                                ui::muted(theme)
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(ui::muted(theme))
                            .child(connection),
                    ),
            )
            .child(
                div()
                    .size(px(34.))
                    .rounded_full()
                    .bg(ui::brand_background())
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.))
                    .font_semibold()
                    .text_color(hsla(0.0, 0.0, 1.0, 1.0))
                    .child("JT"),
            )
    }

    fn render_service_banner(&self, theme: &Theme) -> impl IntoElement {
        let (text, color) = if let Some(error) = self.service_error.as_ref() {
            (format!("本地数据服务不可用：{error}"), ui::red(theme))
        } else if self.service.is_none() {
            ("正在初始化本地数据…".to_string(), ui::muted(theme))
        } else {
            ("本地数据服务已就绪".to_string(), ui::green(theme))
        };
        let error = self.service_error.is_some();

        h_flex()
            .w_full()
            .px(px(20.))
            .py(px(7.))
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(ui::border(theme))
            .bg(if error {
                ui::red(theme).opacity(0.10)
            } else {
                theme.group_box
            })
            .child(div().size(px(6.)).rounded_full().bg(color))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(if error { color } else { ui::muted(theme) })
                    .child(text),
            )
    }

    fn render_drawer(
        &self,
        item: &::core::ItemView,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let item_id = item.id.clone();
        let locked = item.locked;

        let status_line = if locked {
            (
                "🔒 已锁定 — 不会再次翻译；解锁后才能重译。",
                ui::purple(),
                ui::purple(),
            )
        } else {
            match item.status {
                ::core::TranslationStatus::Draft => (
                    "✎ 草稿 — 已翻译，尚未写回 Jellyfin。",
                    ui::amber(theme),
                    ui::amber(theme),
                ),
                ::core::TranslationStatus::Written => (
                    "✔ 已写回 — 服务器上的值已更新。",
                    ui::green(theme),
                    ui::panel2(theme),
                ),
                ::core::TranslationStatus::Untranslated => (
                    "○ 未翻译 — 点“翻译此视频”生成草稿。",
                    ui::muted(theme),
                    ui::panel2(theme),
                ),
            }
        };

        let tags = if item.tags.is_empty() {
            div()
                .text_color(ui::muted(theme))
                .child("—")
                .into_any_element()
        } else {
            h_flex()
                .flex_wrap()
                .gap(px(6.))
                .children(item.tags.iter().map(|tag| {
                    div()
                        .px(px(8.))
                        .py(px(2.))
                        .rounded_full()
                        .bg(theme.muted)
                        .text_size(px(11.5))
                        .text_color(ui::muted(theme))
                        .child(tag.clone())
                }))
                .into_any_element()
        };

        let people = if item.people.is_empty() {
            div()
                .text_color(ui::muted(theme))
                .child("—")
                .into_any_element()
        } else {
            v_flex()
                .gap(px(4.))
                .children(item.people.iter().map(|person| {
                    let name = person.name.clone();
                    let text = match person.role.as_ref() {
                        Some(role) if !role.is_empty() => format!("{name} · {role}"),
                        _ => name,
                    };
                    div()
                        .text_size(px(12.5))
                        .text_color(theme.foreground)
                        .child(text)
                }))
                .into_any_element()
        };

        let id_edit = item_id.clone();
        let id_translate = item_id.clone();
        let id_write = item_id.clone();
        let id_lock = item_id.clone();
        let write_enabled = !locked && item.status == ::core::TranslationStatus::Draft;
        let lock_label = if locked { "解锁" } else { "锁定" };

        h_flex()
            .absolute()
            .top_0()
            .right_0()
            .h_full()
            .w(px(400.))
            .flex_col()
            .bg(theme.sidebar)
            .border_l_1()
            .border_color(ui::border(theme))
            .child(
                h_flex()
                    .items_start()
                    .gap_3()
                    .p(px(16.))
                    .border_b_1()
                    .border_color(ui::border(theme))
                    .child(
                        div()
                            .w(px(72.))
                            .h(px(104.))
                            .flex_shrink_0()
                            .rounded(px(8.))
                            .bg(ui::poster_background(&item.id)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(3.))
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .font_semibold()
                                    .truncate()
                                    .child(item.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(ui::muted(theme))
                                    .truncate()
                                    .child(format!("{} · {}", item.original_title, item.item_type)),
                            ),
                    )
                    .child(
                        Button::new("drawer-close")
                            .ghost()
                            .compact()
                            .label("✕")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.library.drawer = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .id("drawer-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(16.))
                    .child(
                        h_flex()
                            .gap_2()
                            .p(px(10.))
                            .mb(px(14.))
                            .rounded(px(9.))
                            .border_1()
                            .border_color(status_line.2)
                            .bg(status_line.1.opacity(0.10))
                            .text_size(px(12.))
                            .text_color(status_line.1)
                            .child(status_line.0),
                    )
                    .child(ui::kv_row(
                        "标题",
                        item.title.clone().into_any_element(),
                        theme,
                    ))
                    .child(ui::kv_row(
                        "原名",
                        item.original_title.clone().into_any_element(),
                        theme,
                    ))
                    .child(ui::kv_row(
                        "简介",
                        div()
                            .whitespace_normal()
                            .child(if item.overview.is_empty() {
                                "—".to_string()
                            } else {
                                item.overview.clone()
                            })
                            .into_any_element(),
                        theme,
                    ))
                    .child(ui::kv_row("标签", tags, theme))
                    .child(ui::kv_row("演职员", people, theme))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .pt(px(12.))
                            .child(format!("条目 ID：{}", item.id)),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .p(px(12.))
                    .border_t_1()
                    .border_color(ui::border(theme))
                    .child(
                        Button::new("drawer-edit")
                            .label("编辑")
                            .disabled(locked)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_edit(id_edit.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new("drawer-translate")
                            .primary()
                            .label("翻译此视频")
                            .disabled(locked)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.translate_items(vec![id_translate.clone()], window, cx)
                            })),
                    )
                    .child(
                        Button::new("drawer-write")
                            .label("写回服务器")
                            .disabled(!write_enabled)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.write_back_items(vec![id_write.clone()], window, cx)
                            })),
                    )
                    .child(
                        Button::new("drawer-lock")
                            .label(lock_label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_items_locked(vec![id_lock.clone()], !locked, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_edit_modal(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let title = self
            .library
            .editing_item()
            .map(|item| format!("编辑 · {}", item.title))
            .unwrap_or_else(|| "编辑视频".to_string());

        let overlay = div()
            .id("modal-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0.0, 0.0, 0.0, 0.50))
            .on_click(cx.listener(|this, _, _, cx| {
                this.library.editing = None;
                cx.notify();
            }));

        let body = v_flex()
            .absolute()
            .inset_0()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .w(px(680.))
                    .max_h(px(680.))
                    .rounded(px(14.))
                    .border_1()
                    .border_color(ui::border(theme))
                    .bg(theme.sidebar)
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .w_full()
                            .px(px(18.))
                            .py(px(14.))
                            .border_b_1()
                            .border_color(ui::border(theme))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(15.))
                                    .font_semibold()
                                    .child(title),
                            )
                            .child(
                                Button::new("modal-close")
                                    .ghost()
                                    .compact()
                                    .label("✕")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.library.editing = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .id("modal-body")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .p(px(18.))
                            .child(
                                v_flex()
                                    .gap(px(14.))
                                    .child(
                                        v_flex()
                                            .gap(px(6.))
                                            .child(ui::field_label("标题", theme))
                                            .child(Input::new(&self.library.edit_title).w_full()),
                                    )
                                    .child(
                                        v_flex()
                                            .gap(px(6.))
                                            .child(ui::field_label("原名", theme))
                                            .child(Input::new(&self.library.edit_orig).w_full()),
                                    )
                                    .child(
                                        v_flex()
                                            .gap(px(6.))
                                            .child(ui::field_label("简介", theme))
                                            .child(
                                                gpui_kit::component::input::Textarea::new(
                                                    &self.library.edit_overview,
                                                )
                                                .h(px(110.))
                                                .w_full(),
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .gap(px(6.))
                                            .child(ui::field_label("标签", theme))
                                            .child(Input::new(&self.library.edit_tags).w_full())
                                            .child(
                                                div()
                                                    .text_size(px(11.5))
                                                    .text_color(ui::muted(theme))
                                                    .child("逗号分隔；写回时转成 Jellyfin Tags 数组。"),
                                            ),
                                    )
                                    .child(
                                        v_flex()
                                            .gap(px(6.))
                                            .child(ui::field_label("演职员（姓名 / 角色）", theme))
                                            .child(
                                                gpui_kit::component::input::Textarea::new(
                                                    &self.library.edit_people,
                                                )
                                                .h(px(110.))
                                                .w_full(),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.5))
                                                    .text_color(ui::muted(theme))
                                                    .child("每行一位，格式：姓名 / 角色；没有角色只写姓名。"),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .p(px(10.))
                                            .rounded(px(9.))
                                            .border_1()
                                            .border_color(ui::border(theme))
                                            .bg(theme.muted)
                                            .text_size(px(12.))
                                            .text_color(ui::muted(theme))
                                            .child(
                                                "编辑后先保存为草稿；确认无误再写回服务器。",
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap(px(8.))
                            .px(px(18.))
                            .py(px(14.))
                            .border_t_1()
                            .border_color(ui::border(theme))
                            .child(
                                Button::new("modal-cancel")
                                    .ghost()
                                    .label("取消")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.library.editing = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("modal-save-draft")
                                    .label("保存草稿")
                                    .disabled(self.library.edit_saving)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_edit(false, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("modal-save-write")
                                    .primary()
                                    .label("保存并写回")
                                    .loading(self.library.edit_saving)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_edit(true, window, cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element();

        v_flex()
            .absolute()
            .inset_0()
            .child(overlay)
            .child(body)
            .into_any_element()
    }

    fn render_toast(&self, theme: &Theme) -> impl IntoElement {
        h_flex()
            .absolute()
            .right(px(22.))
            .bottom(px(22.))
            .px(px(14.))
            .py(px(11.))
            .rounded(px(10.))
            .border_1()
            .border_color(ui::border(theme))
            .bg(theme.sidebar)
            .shadow_lg()
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(theme.foreground)
                    .child(self.toast.clone().unwrap_or_default()),
            )
    }
}

impl Render for AppState {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.started {
            self.started = true;
            self.start_service(window, cx);
        }

        let theme = cx.theme().clone();
        let content: AnyElement = match self.page {
            Page::Library => self.render_library(&theme, cx),
            Page::Tasks => self.render_tasks(&theme, cx),
            Page::Presets => self.render_presets(&theme, cx),
            Page::Settings => self.render_settings(&theme, cx),
            Page::Connect => self.render_connect(&theme, cx),
        };

        let mut root = div()
            .size_full()
            .relative()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                h_flex()
                    .size_full()
                    .child(self.render_sidebar(&theme, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(self.render_topbar(&theme))
                            .when(
                                self.service.is_none() || self.service_error.is_some(),
                                |column| column.child(self.render_service_banner(&theme)),
                            )
                            .child(
                                div()
                                    .id("page-scroll")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .child(content),
                            ),
                    ),
            );

        if let Some(item) = self.selected_item() {
            root = root
                .child(
                    div()
                        .id("drawer-overlay")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .bg(hsla(0.0, 0.0, 0.0, 0.45))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.library.drawer = None;
                            cx.notify();
                        })),
                )
                .child(self.render_drawer(&item, &theme, cx));
        }

        if self.library.editing.is_some() {
            root = root.child(self.render_edit_modal(&theme, cx));
        }

        if self.toast.is_some() {
            root = root.child(self.render_toast(&theme));
        }

        root
    }
}
