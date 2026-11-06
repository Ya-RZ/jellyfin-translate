//! 连接服务器页：地址 + API Key → AppService::connect → 展示服务器名/版本。

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{h_flex, v_flex, Disableable as _, Theme};
use gpui_kit::*;

use crate::app::AppState;
use crate::ui;

/// Server identity returned by a successful connection.
pub(crate) struct ServerDetails {
    pub(crate) name: String,
    pub(crate) version: String,
}

pub(crate) struct ConnectState {
    pub(crate) url: Entity<InputState>,
    pub(crate) api_key: Entity<InputState>,
    pub(crate) server: Option<ServerDetails>,
    pub(crate) connecting: bool,
    pub(crate) error: Option<SharedString>,
    /// Whether the saved credentials were already restored this session.
    pub(crate) restored: bool,
}

impl ConnectState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<AppState>) -> Self {
        Self {
            url: cx.new(|cx| InputState::new(window, cx).placeholder("http://192.168.1.10:8096")),
            api_key: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("Jellyfin API Key")
            }),
            server: None,
            connecting: false,
            error: None,
            restored: false,
        }
    }

    /// Bare `host:port` is the common case, so default it to plain HTTP.
    fn normalize_url(raw: &str) -> String {
        let trimmed = raw.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            trimmed.to_string()
        } else {
            format!("http://{trimmed}")
        }
    }
}

impl AppState {
    /// Restore the Jellyfin URL/API key saved by a previous session and, when
    /// both are present, connect automatically.
    ///
    /// Runs on the tokio runtime because it reads the config file. The values
    /// stay in the inputs even when the automatic connection fails.
    pub(crate) fn restore_saved_config(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.connect.restored || self.service.is_none() {
            return;
        }
        self.connect.restored = true;

        self.spawn_job_in(
            window,
            cx,
            |service| service.jellyfin_config().map_err(ui::error_text),
            |this, result, window, cx| {
                match result {
                    Ok(config) => {
                        let url = config.url.trim().to_string();
                        let api_key = config.api_key.trim().to_string();

                        if !url.is_empty() {
                            let value = url.clone();
                            this.connect
                                .url
                                .update(cx, |state, cx| state.set_value(value, window, cx));
                        }
                        if !api_key.is_empty() {
                            let value = api_key.clone();
                            this.connect
                                .api_key
                                .update(cx, |state, cx| state.set_value(value, window, cx));
                        }

                        if !url.is_empty() && !api_key.is_empty() {
                            this.connect_server(window, cx, true);
                        }
                    }
                    Err(message) => {
                        // A missing or unreadable config is not fatal: keep the
                        // page usable so the user can enter credentials.
                        tracing::debug!(error = %message, "read saved Jellyfin config failed");
                    }
                }
                cx.notify();
            },
        );
    }

    fn connect_server(&mut self, window: &Window, cx: &mut Context<Self>, auto: bool) {
        let raw_url = self.connect.url.read(cx).value().to_string();
        let api_key = self.connect.api_key.read(cx).value().trim().to_string();

        if raw_url.trim().is_empty() {
            self.connect.error = Some("请填写 Jellyfin 服务器地址".into());
            cx.notify();
            return;
        }
        if api_key.is_empty() {
            self.connect.error = Some("请填写 API Key".into());
            cx.notify();
            return;
        }

        let url = ConnectState::normalize_url(&raw_url);
        self.connect.connecting = true;
        self.connect.error = None;
        cx.notify();

        self.spawn_async_job_in(
            window,
            cx,
            move |service| {
                Box::pin(async move {
                    service
                        .connect(&url, &api_key)
                        .await
                        .map_err(ui::error_text)
                })
            },
            move |this, result, _window, cx| {
                this.connect.connecting = false;
                match result {
                    Ok(info) => {
                        let message =
                            format!("已连接：{} · 版本 {}", info.server_name, info.version);
                        this.connect.server = Some(ServerDetails {
                            name: info.server_name,
                            version: info.version,
                        });
                        this.connect.error = None;
                        this.record_task("连接", message.clone(), true, cx);
                        this.notify_toast(message, cx);
                        // The new connection may expose a different server.
                        this.covers.clear();
                        this.library.libraries.clear();
                        this.library.items.clear();
                        this.library.active_lib = None;
                        this.library.selected.clear();
                        this.library.drawer = None;
                        this.load_libraries(cx);
                        // Only a manual connection opens the picker; the
                        // saved-config auto-connect at startup should not pop a
                        // modal over the user's library view. The toolbar
                        // button is always available.
                        if !auto {
                            this.open_sync_picker(cx);
                        }
                    }
                    Err(message) => {
                        this.connect.server = None;
                        let shown = if auto {
                            format!(
                                "自动连接失败：无法连接 Jellyfin，请检查地址、API Key 或网络。（{message}）"
                            )
                        } else {
                            format!("连接失败：{message}")
                        };
                        this.connect.error = Some(shown.clone().into());
                        if auto {
                            this.notify_toast(shown.clone(), cx);
                        }
                        this.record_task("连接", shown, false, cx);
                    }
                }
                cx.notify();
            },
        );
    }

    pub(crate) fn render_connect(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut card = v_flex()
            .w_full()
            .max_w(px(560.))
            .gap(px(14.))
            .p(px(18.))
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(ui::field_label("服务器地址", theme))
                    .child(Input::new(&self.connect.url).w_full())
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("不带协议时自动补 http://"),
                    ),
            )
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(ui::field_label("API Key", theme))
                    .child(Input::new(&self.connect.api_key).w_full())
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("Jellyfin 控制台 → 管理 → API 密钥；写元数据需要管理员权限。"),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .text_size(px(11.5))
                    .text_color(ui::muted(theme))
                    .child(
                        "连接信息保存在程序目录 data/config.json（地址与 API Key 仅本机保存）。",
                    ),
            )
            .child(
                Button::new("connect-server")
                    .primary()
                    .w_full()
                    .label(if self.connect.connecting {
                        "连接中…"
                    } else {
                        "连接并校验"
                    })
                    .loading(self.connect.connecting)
                    .disabled(self.connect.connecting)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.connect_server(window, cx, false)),
                    ),
            );

        if let Some(server) = self.connect.server.as_ref() {
            card = card.child(
                h_flex()
                    .gap_2()
                    .p(px(10.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(ui::green(theme))
                    .bg(ui::green(theme).opacity(0.10))
                    .text_size(px(12.))
                    .text_color(ui::green(theme))
                    .child(format!("已连接：{} · 版本 {}", server.name, server.version)),
            );
        }

        if let Some(error) = self.connect.error.as_ref() {
            card = card.child(
                div()
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

        if self.connect.server.is_some() {
            let running = self.sync.running;
            let progress = self
                .sync
                .progress
                .as_ref()
                .map(ui::sync_progress_text)
                .unwrap_or_else(|| {
                    if running {
                        "正在准备同步…".to_string()
                    } else {
                        "把 Jellyfin 媒体库镜像到本地，完成后自动刷新媒体库。".to_string()
                    }
                });

            let mut sync_block = v_flex()
                .w_full()
                .gap(px(8.))
                .p(px(12.))
                .rounded(px(9.))
                .border_1()
                .border_color(ui::border(theme))
                .bg(theme.muted)
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(
                            Button::new("sync-from-connect")
                                .primary()
                                .label(if running {
                                    "同步中…"
                                } else {
                                    "同步媒体库"
                                })
                                .loading(running)
                                .disabled(running)
                                .on_click(cx.listener(|this, _, _, cx| this.open_sync_picker(cx))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.5))
                                .text_color(ui::muted(theme))
                                .child(progress),
                        ),
                );

            if let Some(report) = self.sync.report.as_ref() {
                sync_block = sync_block.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(ui::green(theme))
                        .child(format!(
                            "上次同步：{} 个媒体库 · {} 个条目 · {} ms",
                            report.libraries, report.items, report.elapsed_ms
                        )),
                );
            }
            if let Some(error) = self.sync.error.as_ref() {
                sync_block = sync_block.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(ui::red(theme))
                        .child(format!("同步失败：{error}")),
                );
            }

            card = card.child(sync_block);
        }

        v_flex()
            .w_full()
            .px(px(24.))
            .pb(px(40.))
            .child(h_flex().pt(px(22.)).pb(px(16.)).child(ui::page_head(
                "连接服务器",
                "地址与 API Key 只保存在本机程序目录",
                theme,
            )))
            .child(ui::card(theme).w_full().max_w(px(596.)).child(card))
            .into_any_element()
    }
}
