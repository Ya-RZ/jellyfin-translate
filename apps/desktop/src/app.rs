//! M0 desktop shell: connection form, library sync and a status bar.
//!
//! The view owns every piece of UI state. All network and database work runs
//! on a background tokio runtime; results come back to the view through an
//! unbounded mpsc channel consumed by a GPUI foreground task, so the UI
//! thread never blocks and never calls `block_on`.

use ::core::{AppConfig, SyncProgress, SyncReport, SyncService};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{
    h_flex, v_flex, ActiveTheme as _, Disableable as _, StyledExt as _, Theme,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// What the shell is currently doing. Only one background operation may run at
/// a time; both buttons are disabled while this is not `Idle`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Connecting,
    Syncing,
}

/// Result of a connection attempt, delivered from the tokio task to the view.
enum ConnectOutcome {
    Connected {
        client: jellyfin::Client,
        info: jellyfin::SystemInfo,
    },
    Failed(String),
}

/// Progress or completion of a library sync, delivered from the tokio task.
enum SyncEvent {
    Progress(SyncProgress),
    Finished(SyncReport),
    Failed(String),
}

/// Server identity shown once `/System/Info` succeeded.
struct ServerDetails {
    name: String,
    version: String,
}

/// Root view: inputs, connection/sync state and the status bar.
pub(crate) struct AppState {
    /// Handle to the background runtime that performs blocking/async work.
    runtime: tokio::runtime::Handle,
    url_input: Entity<InputState>,
    api_key_input: Entity<InputState>,
    phase: Phase,
    error: Option<SharedString>,
    server: Option<ServerDetails>,
    client: Option<jellyfin::Client>,
    progress: Option<SyncProgress>,
    report: Option<SyncReport>,
}

impl AppState {
    pub(crate) fn new(
        runtime: tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("http://192.168.1.10:8096"));
        let api_key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Jellyfin API Key")
        });

        Self {
            runtime,
            url_input,
            api_key_input,
            phase: Phase::Idle,
            error: None,
            server: None,
            client: None,
            progress: None,
            report: None,
        }
    }

    fn set_error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }

    /// Normalize what the user typed into a URL the HTTP client accepts.
    /// A bare host:port is the common case, so default to plain HTTP.
    fn normalize_url(raw: &str) -> String {
        let trimmed = raw.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            trimmed.to_string()
        } else {
            format!("http://{trimmed}")
        }
    }

    fn begin_connect(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.phase != Phase::Idle {
            return;
        }

        let raw_url = self.url_input.read(cx).value().to_string();
        let api_key = self.api_key_input.read(cx).value().to_string();

        if raw_url.trim().is_empty() {
            self.set_error("请填写 Jellyfin 服务器地址", cx);
            return;
        }
        if api_key.trim().is_empty() {
            self.set_error("请填写 API Key", cx);
            return;
        }

        let url = Self::normalize_url(&raw_url);
        let api_key = api_key.trim().to_string();

        self.phase = Phase::Connecting;
        self.error = None;
        self.server = None;
        self.progress = None;
        self.report = None;
        cx.notify();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ConnectOutcome>();
        self.runtime.spawn(async move {
            let outcome = match jellyfin::Client::new(&url, &api_key) {
                Ok(client) => match client.system_info().await {
                    Ok(info) => ConnectOutcome::Connected { client, info },
                    Err(error) => ConnectOutcome::Failed(format!("{error:#}")),
                },
                Err(error) => ConnectOutcome::Failed(format!("{error:#}")),
            };
            let _ = tx.send(outcome);
        });

        // Foreground task: awaits the background result without blocking the
        // UI thread, then updates the entity and asks for a repaint.
        cx.spawn(async move |this, cx| {
            let outcome = rx.recv().await;
            this.update(cx, |this, cx| {
                this.phase = Phase::Idle;
                match outcome {
                    Some(ConnectOutcome::Connected { client, info }) => {
                        this.client = Some(client);
                        this.server = Some(ServerDetails {
                            name: info.server_name,
                            version: info.version,
                        });
                        this.error = None;
                    }
                    Some(ConnectOutcome::Failed(message)) => {
                        this.client = None;
                        this.server = None;
                        this.error = Some(message.into());
                    }
                    // The background task ended without reporting: don't leave
                    // the shell stuck in the "connecting" state.
                    None => {
                        this.error = Some("连接任务意外结束".into());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn begin_sync(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.phase != Phase::Idle {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.set_error("请先连接 Jellyfin 服务器", cx);
            return;
        };

        self.phase = Phase::Syncing;
        self.error = None;
        self.progress = None;
        self.report = None;
        cx.notify();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SyncEvent>();
        let db_path = AppConfig::default_db_path();

        self.runtime.spawn(async move {
            let progress_tx = tx.clone();
            let outcome: anyhow::Result<SyncReport> = async move {
                let store = store::Store::open(&db_path)?;
                let mut service = SyncService::new(client, store);
                service
                    .sync_all(move |progress| {
                        let _ = progress_tx.send(SyncEvent::Progress(progress));
                    })
                    .await
            }
            .await;

            let event = match outcome {
                Ok(report) => SyncEvent::Finished(report),
                Err(error) => SyncEvent::Failed(format!("{error:#}")),
            };
            let _ = tx.send(event);
        });

        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let terminal = matches!(&event, SyncEvent::Finished(_) | SyncEvent::Failed(_));
                let updated = this
                    .update(cx, |this, cx| {
                        match event {
                            SyncEvent::Progress(progress) => {
                                this.progress = Some(progress);
                            }
                            SyncEvent::Finished(report) => {
                                this.report = Some(report);
                                this.phase = Phase::Idle;
                            }
                            SyncEvent::Failed(message) => {
                                this.error = Some(message.into());
                                this.phase = Phase::Idle;
                            }
                        }
                        cx.notify();
                    })
                    .is_ok();
                if terminal || !updated {
                    break;
                }
            }

            // The channel closed without a terminal event (background task
            // panicked or was cancelled): clear the busy state.
            this.update(cx, |this, cx| {
                if this.phase == Phase::Syncing {
                    this.phase = Phase::Idle;
                    if this.error.is_none() {
                        this.error = Some("同步任务意外结束".into());
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
}

fn field_label(text: &'static str, theme: &Theme) -> Div {
    div()
        .text_sm()
        .text_color(theme.muted_foreground)
        .child(text)
}

impl Render for AppState {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let busy = self.phase != Phase::Idle;

        let status_text = match self.phase {
            Phase::Idle => "就绪",
            Phase::Connecting => "连接中…",
            Phase::Syncing => "同步中…",
        };
        let status_color = if busy { theme.warning } else { theme.success };

        let connection_card =
            v_flex()
                .w_full()
                .gap_3()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.group_box)
                .child(div().text_lg().font_semibold().child("连接"))
                .child(
                    v_flex()
                        .gap_1()
                        .child(field_label("Jellyfin 服务器地址", theme))
                        .child(Input::new(&self.url_input).w_full()),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(field_label("API Key（密码式输入）", theme))
                        .child(Input::new(&self.api_key_input).w_full()),
                )
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            Button::new("connect")
                                .primary()
                                .label("连接")
                                .loading(self.phase == Phase::Connecting)
                                .disabled(busy)
                                .on_click(cx.listener(|this, _event, window, cx| {
                                    this.begin_connect(window, cx)
                                })),
                        )
                        .when_some(self.server.as_ref(), |row, server| {
                            row.child(div().text_sm().text_color(theme.success).child(format!(
                                "已连接：{} · 版本 {}",
                                server.name, server.version
                            )))
                        }),
                );

        let sync_card = v_flex()
            .w_full()
            .gap_3()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.group_box)
            .child(div().text_lg().font_semibold().child("同步"))
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("sync")
                            .primary()
                            .label("同步媒体库")
                            .loading(self.phase == Phase::Syncing)
                            .disabled(busy)
                            .on_click(
                                cx.listener(|this, _event, window, cx| this.begin_sync(window, cx)),
                            ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("将 Jellyfin 媒体库镜像到本地数据库"),
                    ),
            )
            .when_some(self.progress.as_ref(), |card, progress| {
                card.child(div().text_sm().text_color(theme.foreground).child(format!(
                    "已同步 {}/{} 个媒体库 · {} 个条目 · {}",
                    progress.libraries_done,
                    progress.libraries_total,
                    progress.items,
                    progress.message
                )))
            })
            .when_some(self.report.as_ref(), |card, report| {
                card.child(div().text_sm().text_color(theme.success).child(format!(
                    "同步完成：{} 个媒体库，{} 个条目，用时 {} ms",
                    report.libraries, report.items, report.elapsed_ms
                )))
            });

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div().flex_1().w_full().child(
                    v_flex()
                        .w_full()
                        .max_w(px(780.))
                        .mx_auto()
                        .p_6()
                        .gap_5()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_2xl().font_semibold().child("Jellyfin Translate"))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("连接 Jellyfin 服务器，把媒体库同步到本地。"),
                                ),
                        )
                        .child(connection_card)
                        .when(self.client.is_some(), |column| column.child(sync_card)),
                ),
            )
            .child(
                h_flex()
                    .w_full()
                    .px_4()
                    .py_2()
                    .gap_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .bg(theme.group_box)
                    .child(div().size_2().rounded_full().bg(status_color))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(status_text),
                    )
                    .when_some(self.error.as_ref(), |bar, error| {
                        bar.child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(theme.danger)
                                .truncate()
                                .child(error.clone()),
                        )
                    }),
            )
    }
}
