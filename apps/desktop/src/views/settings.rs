//! 设置页：OpenAI 兼容配置、MetaTube 抓取服务、四个字段模板、语言、数据目录与并发。

use ::core::{MetatubeSettings, ProviderSettings, Templates};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::{h_flex, v_flex, Sizable as _, StyledExt as _, Theme};
use gpui_kit::*;

use crate::app::AppState;
use crate::ui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TemplateTab {
    Title,
    Overview,
    Tags,
    People,
}

impl TemplateTab {
    fn label(self) -> &'static str {
        match self {
            TemplateTab::Title => "标题",
            TemplateTab::Overview => "简介",
            TemplateTab::Tags => "标签",
            TemplateTab::People => "演职员",
        }
    }

    fn text<'a>(self, templates: &'a Templates) -> &'a str {
        match self {
            TemplateTab::Title => &templates.title,
            TemplateTab::Overview => &templates.overview,
            TemplateTab::Tags => &templates.tags,
            TemplateTab::People => &templates.people,
        }
    }

    fn text_mut<'a>(self, templates: &'a mut Templates) -> &'a mut String {
        match self {
            TemplateTab::Title => &mut templates.title,
            TemplateTab::Overview => &mut templates.overview,
            TemplateTab::Tags => &mut templates.tags,
            TemplateTab::People => &mut templates.people,
        }
    }

    fn default_text(self) -> &'static str {
        match self {
            TemplateTab::Title => {
                "你是影视元数据翻译。把下面的电影/剧集标题翻译成{{target_lang}}，保留专有名词的通用译法，只输出译文，不要解释。\n\n原文：{{text}}"
            }
            TemplateTab::Overview => {
                "你是影视简介翻译。把下面的剧情简介翻译成{{target_lang}}，保持语气与信息完整，不加评论，不剧透改写。\n\n原文：{{text}}"
            }
            TemplateTab::Tags => {
                "把下面的影视标签逐个翻译成{{target_lang}}，每行一个标签，只输出译文，保持行数一致。\n\n{{segments}}"
            }
            TemplateTab::People => {
                "把下面的演职员信息翻译成{{target_lang}}：姓名用通用译名，角色名翻译成目标语言。返回 JSON 数组，每项包含 id、name、role。\n\n{{segments}}"
            }
        }
    }
}

/// Fill any empty persisted template with the shipped default, so a fresh
/// install sends a usable prompt before the user edits settings.
pub(crate) fn fill_default_templates(templates: &mut Templates) {
    for tab in [
        TemplateTab::Title,
        TemplateTab::Overview,
        TemplateTab::Tags,
        TemplateTab::People,
    ] {
        if tab.text(templates).trim().is_empty() {
            *tab.text_mut(templates) = tab.default_text().to_string();
        }
    }
}

pub(crate) struct SettingsState {
    pub(crate) base_url: Entity<InputState>,
    pub(crate) model: Entity<InputState>,
    pub(crate) api_key: Entity<InputState>,
    pub(crate) source_lang: Entity<InputState>,
    pub(crate) target_lang: Entity<InputState>,
    pub(crate) concurrency: Entity<InputState>,
    pub(crate) metatube_base_url: Entity<InputState>,
    pub(crate) metatube_token: Entity<InputState>,
    pub(crate) template_editor: Entity<TextareaState>,
    pub(crate) templates: Templates,
    pub(crate) template_tab: TemplateTab,
    pub(crate) loaded: bool,
    pub(crate) loading: bool,
    pub(crate) saving: bool,
    pub(crate) metatube_saving: bool,
    pub(crate) error: Option<SharedString>,
}

impl SettingsState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<AppState>) -> Self {
        Self {
            base_url: cx
                .new(|cx| InputState::new(window, cx).placeholder("https://api.openai.com/v1")),
            model: cx.new(|cx| InputState::new(window, cx).placeholder("gpt-4.1-mini")),
            api_key: cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("sk-…")),
            source_lang: cx.new(|cx| InputState::new(window, cx).placeholder("auto")),
            target_lang: cx.new(|cx| InputState::new(window, cx).placeholder("zh-CN")),
            concurrency: cx.new(|cx| InputState::new(window, cx).placeholder("4")),
            metatube_base_url: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(::core::MetatubeSettings::default().base_url)
            }),
            metatube_token: cx.new(|cx| {
                InputState::new(window, cx)
                    .masked(true)
                    .placeholder("留空=未鉴权")
            }),
            template_editor: cx.new(|cx| TextareaState::new(window, cx)),
            templates: Templates::default(),
            template_tab: TemplateTab::Title,
            loaded: false,
            loading: false,
            saving: false,
            metatube_saving: false,
            error: None,
        }
    }

    fn apply(
        &mut self,
        settings: ProviderSettings,
        mut templates: Templates,
        metatube: MetatubeSettings,
        window: &mut Window,
        cx: &mut Context<AppState>,
    ) {
        fill_default_templates(&mut templates);
        self.templates = templates;

        self.base_url.update(cx, |state, cx| {
            state.set_value(settings.base_url.clone(), window, cx)
        });
        self.model.update(cx, |state, cx| {
            state.set_value(settings.model.clone(), window, cx)
        });
        self.api_key.update(cx, |state, cx| {
            state.set_value(settings.api_key.clone(), window, cx)
        });
        self.source_lang.update(cx, |state, cx| {
            state.set_value(settings.source_lang.clone(), window, cx)
        });
        self.target_lang.update(cx, |state, cx| {
            state.set_value(settings.target_lang.clone(), window, cx)
        });
        self.concurrency.update(cx, |state, cx| {
            state.set_value(settings.concurrency.to_string(), window, cx)
        });
        self.metatube_base_url.update(cx, |state, cx| {
            state.set_value(metatube.base_url.clone(), window, cx)
        });
        self.metatube_token.update(cx, |state, cx| {
            state.set_value(metatube.token.clone(), window, cx)
        });

        self.loaded = true;
        self.loading = false;
        self.load_editor(window, cx);
    }

    fn load_editor(&mut self, window: &mut Window, cx: &mut Context<AppState>) {
        let text = self.template_tab.text(&self.templates).to_string();
        self.template_editor
            .update(cx, |state, cx| state.set_value(text, window, cx));
    }

    fn capture_template(&mut self, cx: &mut Context<AppState>) {
        let text = self.template_editor.read(cx).value().to_string();
        *self.template_tab.text_mut(&mut self.templates) = text;
    }

    fn switch_tab(&mut self, tab: TemplateTab, window: &mut Window, cx: &mut Context<AppState>) {
        if self.template_tab == tab {
            return;
        }
        self.capture_template(cx);
        self.template_tab = tab;
        self.load_editor(window, cx);
        cx.notify();
    }

    fn restore_default(&mut self, window: &mut Window, cx: &mut Context<AppState>) {
        *self.template_tab.text_mut(&mut self.templates) =
            self.template_tab.default_text().to_string();
        self.load_editor(window, cx);
        cx.notify();
    }
}

impl AppState {
    /// Open the portable data directory in the platform file manager.
    fn open_data_dir(&mut self, cx: &mut Context<Self>) {
        let data_dir = self
            .service
            .as_ref()
            .map(|service| service.data_dir_path().to_path_buf())
            .unwrap_or_else(::core::AppService::data_dir);

        #[cfg(target_os = "windows")]
        {
            match std::process::Command::new("explorer")
                .arg(&data_dir)
                .spawn()
            {
                Ok(_) => self.notify_toast(format!("已打开数据目录：{}", data_dir.display()), cx),
                Err(error) => self.notify_toast(format!("打开目录失败：{error}"), cx),
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = data_dir;
            self.notify_toast("当前平台暂不支持打开数据目录（Windows 使用 explorer）", cx);
        }
    }

    pub(crate) fn load_settings(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.service.is_none() || self.settings.loading || self.settings.loaded {
            return;
        }
        self.settings.loading = true;
        self.settings.error = None;
        cx.notify();

        self.spawn_job_in(
            window,
            cx,
            |service| {
                let settings = service.provider_settings().map_err(ui::error_text)?;
                let templates = service.templates().map_err(ui::error_text)?;
                let metatube = service.metatube_settings().map_err(ui::error_text)?;
                Ok((settings, templates, metatube))
            },
            |this, result, window, cx| {
                this.settings.loading = false;
                match result {
                    Ok((settings, templates, metatube)) => {
                        this.settings
                            .apply(settings, templates, metatube, window, cx);
                        this.settings.error = None;
                    }
                    Err(message) => this.settings.error = Some(message.into()),
                }
                cx.notify();
            },
        );
    }

    fn save_templates(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.settings.capture_template(cx);
        let templates = self.settings.templates.clone();
        self.settings.saving = true;
        cx.notify();

        self.spawn_job_in(
            window,
            cx,
            move |service| service.save_templates(&templates).map_err(ui::error_text),
            |this, result, _window, cx| {
                this.settings.saving = false;
                match result {
                    Ok(()) => this.notify_toast("模板已保存", cx),
                    Err(message) => this.notify_toast(message, cx),
                }
                cx.notify();
            },
        );
    }

    fn save_settings(&mut self, window: &Window, cx: &mut Context<Self>) {
        // Validate concurrency up front so an invalid value surfaces an error
        // instead of silently falling back to the default (which left the input
        // showing "abc" while the app actually used 4).
        let concurrency_raw = self
            .settings
            .concurrency
            .read(cx)
            .value()
            .trim()
            .to_string();
        let concurrency = match concurrency_raw.parse::<usize>() {
            Ok(value) if value >= 1 => value.clamp(1, 16),
            _ => {
                self.notify_toast("并发数无效，请输入 1–16 的整数", cx);
                return;
            }
        };

        let settings = ProviderSettings {
            base_url: self.settings.base_url.read(cx).value().trim().to_string(),
            model: self.settings.model.read(cx).value().trim().to_string(),
            api_key: self.settings.api_key.read(cx).value().trim().to_string(),
            source_lang: self
                .settings
                .source_lang
                .read(cx)
                .value()
                .trim()
                .to_string(),
            target_lang: self
                .settings
                .target_lang
                .read(cx)
                .value()
                .trim()
                .to_string(),
            concurrency,
        };

        self.settings.saving = true;
        cx.notify();

        self.spawn_job_in(
            window,
            cx,
            move |service| {
                service
                    .save_provider_settings(&settings)
                    .map_err(ui::error_text)
            },
            |this, result, _window, cx| {
                this.settings.saving = false;
                match result {
                    Ok(()) => this.notify_toast("设置已保存", cx),
                    Err(message) => this.notify_toast(message, cx),
                }
                cx.notify();
            },
        );
    }

    fn save_metatube_settings(&mut self, window: &Window, cx: &mut Context<Self>) {
        let settings = MetatubeSettings {
            base_url: self
                .settings
                .metatube_base_url
                .read(cx)
                .value()
                .trim()
                .to_string(),
            token: self
                .settings
                .metatube_token
                .read(cx)
                .value()
                .trim()
                .to_string(),
        };

        self.settings.metatube_saving = true;
        cx.notify();

        self.spawn_job_in(
            window,
            cx,
            move |service| {
                service
                    .save_metatube_settings(&settings)
                    .map_err(ui::error_text)
            },
            |this, result, _window, cx| {
                this.settings.metatube_saving = false;
                match result {
                    Ok(()) => this.notify_toast("MetaTube 设置已保存", cx),
                    Err(message) => this.notify_toast(message, cx),
                }
                cx.notify();
            },
        );
    }

    pub(crate) fn render_settings(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let mut column = v_flex().w_full().px(px(24.)).pb(px(40.)).child(
            h_flex().pt(px(22.)).pb(px(16.)).child(ui::page_head(
                "设置",
                "所有数据保存在程序目录，可整目录拷走",
                theme,
            )),
        );

        if !self.settings.loaded {
            return column
                .child(
                    ui::card(theme)
                        .p(px(40.))
                        .text_center()
                        .text_size(px(13.))
                        .text_color(ui::muted(theme))
                        .child(if let Some(error) = self.settings.error.as_ref() {
                            format!("读取设置失败：{error}")
                        } else {
                            "正在读取设置…".to_string()
                        }),
                )
                .into_any_element();
        }

        let pair = |left: AnyElement, right: AnyElement| {
            h_flex()
                .w_full()
                .gap(px(14.))
                .child(div().flex_1().min_w_0().child(left))
                .child(div().flex_1().min_w_0().child(right))
        };

        let provider_block = v_flex()
            .w_full()
            .gap(px(12.))
            .p(px(18.))
            .border_b_1()
            .border_color(ui::border(theme))
            .child(
                div()
                    .text_size(px(13.5))
                    .font_semibold()
                    .child("翻译服务（OpenAI 兼容）"),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(ui::muted(theme))
                    .child("只支持 /chat/completions 兼容接口；保存后下一次翻译生效。"),
            )
            .child(pair(
                v_flex()
                    .gap(px(6.))
                    .child(ui::field_label("Base URL", theme))
                    .child(Input::new(&self.settings.base_url).w_full())
                    .into_any_element(),
                v_flex()
                    .gap(px(6.))
                    .child(ui::field_label("模型", theme))
                    .child(Input::new(&self.settings.model).w_full())
                    .into_any_element(),
            ))
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(ui::field_label("API Key", theme))
                    .child(Input::new(&self.settings.api_key).w_full())
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("仅保存在本机数据目录，不会上传。"),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("save-provider")
                            .primary()
                            .label("保存设置")
                            .loading(self.settings.saving)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.save_settings(window, cx)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("超时 120s · 失败重试 1 次"),
                    ),
            );

        let metatube_block = v_flex()
            .w_full()
            .gap(px(12.))
            .p(px(18.))
            .border_b_1()
            .border_color(ui::border(theme))
            .child(
                div()
                    .text_size(px(13.5))
                    .font_semibold()
                    .child("视频元数据抓取（MetaTube）"),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(ui::muted(theme))
                    .child("详情抽屉里的「抓取元数据」使用这个服务；结果只填入本地草稿，不会自动写回 Jellyfin。"),
            )
            .child(pair(
                v_flex()
                    .gap(px(6.))
                    .child(ui::field_label("服务地址", theme))
                    .child(Input::new(&self.settings.metatube_base_url).w_full())
                    .into_any_element(),
                v_flex()
                    .gap(px(6.))
                    .child(ui::field_label("Token（可为空）", theme))
                    .child(Input::new(&self.settings.metatube_token).w_full())
                    .into_any_element(),
            ))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("save-metatube")
                            .primary()
                            .label("保存 MetaTube 设置")
                            .loading(self.settings.metatube_saving)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_metatube_settings(window, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("服务未设置 -token 时 Token 留空即可。"),
                    ),
            );

        let tab_button =
            |tab: TemplateTab| {
                let active = self.settings.template_tab == tab;
                div()
                    .id(("tmpl-tab", tab as usize))
                    .px(px(11.))
                    .py(px(5.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(if active {
                        ui::accent_soft(0.4)
                    } else {
                        ui::border(theme)
                    })
                    .bg(if active {
                        ui::accent_soft(0.14)
                    } else {
                        ui::transparent()
                    })
                    .cursor_pointer()
                    .text_size(px(12.))
                    .text_color(if active {
                        theme.foreground
                    } else {
                        ui::muted(theme)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.settings.switch_tab(tab, window, cx)
                    }))
                    .child(tab.label())
            };

        let variable = |name: &'static str| {
            div()
                .px(px(5.))
                .py(px(1.))
                .rounded(px(5.))
                .border_1()
                .border_color(ui::border(theme))
                .bg(theme.muted)
                .text_size(px(11.5))
                .text_color(ui::muted(theme))
                .child(name)
        };

        let templates_block = v_flex()
            .w_full()
            .gap(px(10.))
            .p(px(18.))
            .border_b_1()
            .border_color(ui::border(theme))
            .child(
                div()
                    .text_size(px(13.5))
                    .font_semibold()
                    .child("提示词模板"),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(ui::muted(theme))
                    .child(
                        "按字段分开；翻译时把对应字段的文本填入 {{text}}，批量时按 JSON 数组放入 {{segments}}。",
                    ),
            )
            .child(
                h_flex()
                    .gap(px(4.))
                    .child(tab_button(TemplateTab::Title))
                    .child(tab_button(TemplateTab::Overview))
                    .child(tab_button(TemplateTab::Tags))
                    .child(tab_button(TemplateTab::People)),
            )
            .child(
                Textarea::new(&self.settings.template_editor)
                    .h(px(120.))
                    .w_full(),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .items_center()
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("可用变量："),
                    )
                    .child(variable("{{text}}"))
                    .child(variable("{{segments}}"))
                    .child(variable("{{source_lang}}"))
                    .child(variable("{{target_lang}}"))
                    .child(variable("{{field}}")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("save-templates")
                            .small()
                            .label("保存模板")
                            .loading(self.settings.saving)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.save_templates(window, cx)
                            })),
                    )
                    .child(
                        Button::new("restore-templates")
                            .small()
                            .ghost()
                            .label("恢复默认")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.settings.restore_default(window, cx);
                                this.notify_toast("已恢复默认模板，点「保存模板」生效", cx);
                            })),
                    ),
            );

        let language_block = v_flex()
            .w_full()
            .gap(px(12.))
            .p(px(18.))
            .border_b_1()
            .border_color(ui::border(theme))
            .child(
                div()
                    .text_size(px(13.5))
                    .font_semibold()
                    .child("语言与翻译范围"),
            )
            .child(pair(
                v_flex()
                    .gap(px(6.))
                    .child(ui::field_label("源语言（auto 为自动检测）", theme))
                    .child(Input::new(&self.settings.source_lang).w_full())
                    .into_any_element(),
                v_flex()
                    .gap(px(6.))
                    .child(ui::field_label("目标语言", theme))
                    .child(Input::new(&self.settings.target_lang).w_full())
                    .into_any_element(),
            ))
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(ui::muted(theme))
                    .child("写回字段：标题 / 排序标题 / 原名 / 类型 / 简介 / 标签 / 演职员。Studios、Tagline、分级与图片文字暂不翻译。"),
            );

        let data_dir = self
            .service
            .as_ref()
            .map(|service| service.data_dir_path().to_path_buf())
            .unwrap_or_else(::core::AppService::data_dir);
        let data_block = v_flex()
            .w_full()
            .gap(px(8.))
            .p(px(18.))
            .border_b_1()
            .border_color(ui::border(theme))
            .child(div().text_size(px(13.5)).font_semibold().child("数据目录"))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(ui::muted(theme))
                    .child("程序目录下的 data/，包含数据库、日志与配置。"),
            )
            .child(
                div()
                    .p(px(10.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(ui::border(theme))
                    .bg(theme.muted)
                    .text_size(px(11.5))
                    .child(format!("数据目录：{}", data_dir.display())),
            )
            .child(
                v_flex()
                    .gap(px(5.))
                    .text_size(px(11.5))
                    .text_color(ui::muted(theme))
                    .child(format!("数据库：{}", data_dir.join("library.db").display()))
                    .child(format!("配置：{}", data_dir.join("config.json").display()))
                    .child(format!(
                        "日志：{}",
                        data_dir.join("logs").join("app.log").display()
                    )),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("open-data-dir")
                            .small()
                            .label("打开目录")
                            .on_click(cx.listener(|this, _, _, cx| this.open_data_dir(cx))),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("Windows 下用资源管理器打开；其他平台仅提示。"),
                    ),
            );

        let concurrency_block = v_flex()
            .w_full()
            .gap(px(12.))
            .p(px(18.))
            .child(
                div()
                    .text_size(px(13.5))
                    .font_semibold()
                    .child("并发与限流"),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap(px(14.))
                    .child(
                        v_flex()
                            .w(px(200.))
                            .gap(px(6.))
                            .child(ui::field_label("并发请求", theme))
                            .child(Input::new(&self.settings.concurrency).w_full()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("同时发往翻译服务的请求数量，默认 4；网络较慢时调低。"),
                    ),
            )
            .child(
                Button::new("save-settings-bottom")
                    .primary()
                    .label("保存设置")
                    .loading(self.settings.saving)
                    .on_click(cx.listener(|this, _, window, cx| this.save_settings(window, cx))),
            );

        if let Some(error) = self.settings.error.as_ref() {
            column = column.child(
                div()
                    .mb(px(12.))
                    .p(px(10.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(ui::red(theme))
                    .bg(ui::red(theme).opacity(0.10))
                    .text_size(px(12.))
                    .text_color(ui::red(theme))
                    .child(format!("读取设置失败：{error}")),
            );
        }

        column
            .child(
                ui::card(theme)
                    .max_w(px(860.))
                    .child(provider_block)
                    .child(metatube_block)
                    .child(templates_block)
                    .child(language_block)
                    .child(data_block)
                    .child(concurrency_block),
            )
            .into_any_element()
    }
}
