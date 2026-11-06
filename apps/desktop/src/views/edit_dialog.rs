//! 双栏编辑弹窗：左侧服务器原文，右侧可编辑译文，支持按字段翻译/写回。

use ::core::{FieldKind, ItemEdit, ItemView, PersonView};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::{h_flex, v_flex, Disableable as _, Sizable as _, StyledExt as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::AppState;
use crate::ui;
use crate::views::settings::fill_default_templates;

/// The five editable metadata fields, in dialog order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum EditField {
    Title,
    OriginalTitle,
    Overview,
    Tags,
    People,
}

impl EditField {
    pub(crate) const ALL: [EditField; 5] = [
        EditField::Title,
        EditField::OriginalTitle,
        EditField::Overview,
        EditField::Tags,
        EditField::People,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            EditField::Title => "标题",
            EditField::OriginalTitle => "原名",
            EditField::Overview => "简介",
            EditField::Tags => "标签",
            EditField::People => "演职员",
        }
    }

    fn kind(self) -> FieldKind {
        match self {
            EditField::Title => FieldKind::Title,
            EditField::OriginalTitle => FieldKind::OriginalTitle,
            EditField::Overview => FieldKind::Overview,
            EditField::Tags => FieldKind::Tags,
            EditField::People => FieldKind::People,
        }
    }
}

/// Snapshot of the server's original values, shown in the left column.
#[derive(Clone, Default)]
pub(crate) struct EditSource {
    pub(crate) title: String,
    pub(crate) original_title: String,
    pub(crate) overview: String,
    pub(crate) tags: Vec<String>,
    pub(crate) people: Vec<PersonView>,
}

impl EditSource {
    pub(crate) fn from_item(item: &ItemView) -> Self {
        Self {
            title: item.source.title.clone(),
            original_title: item.source.original_title.clone(),
            overview: item.source.overview.clone(),
            tags: item.source.tags.clone(),
            people: item.source.people.clone(),
        }
    }

    fn text(&self, field: EditField) -> String {
        match field {
            EditField::Title => self.title.clone(),
            EditField::OriginalTitle => self.original_title.clone(),
            EditField::Overview => self.overview.clone(),
            EditField::Tags => {
                if self.tags.is_empty() {
                    "—".to_string()
                } else {
                    self.tags.join(", ")
                }
            }
            EditField::People => format_people(&self.people),
        }
    }
}

fn format_people(people: &[PersonView]) -> String {
    if people.is_empty() {
        return "—".to_string();
    }
    people
        .iter()
        .map(|person| match person.role.as_ref() {
            Some(role) if !role.is_empty() => format!("{} / {}", person.name, role),
            _ => person.name.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn translate_error(message: &str) -> String {
    if message.contains("API key must not be empty") {
        "未配置翻译服务的 API Key，请到「设置 → 翻译服务」填写。".to_string()
    } else if message.contains("base URL must not be empty") {
        "未配置翻译服务的 Base URL，请到「设置 → 翻译服务」填写。".to_string()
    } else {
        format!("翻译失败：{message}")
    }
}

impl AppState {
    pub(crate) fn render_edit_dialog(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(item) = self.library.editing_item() else {
            return div().into_any_element();
        };
        let busy = self.library.edit_busy;
        let locked = item.locked;

        let overlay = div()
            .id("edit-dialog-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0.0, 0.0, 0.0, 0.52))
            .on_click(cx.listener(|this, _, _, cx| this.close_edit_dialog(cx)));

        let header = h_flex()
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px(px(18.))
            .py(px(14.))
            .border_b_1()
            .border_color(ui::border(theme))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(320.))
                    .gap(px(2.))
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .truncate()
                            .text_size(px(15.))
                            .font_semibold()
                            .child(format!("编辑 · {}", item.title)),
                    )
                    .child(
                        div()
                            .w_full()
                            .min_w_0()
                            .whitespace_normal()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("左侧为服务器原文，右侧为本地译文；写回前会先保存当前草稿。"),
                    ),
            )
            .child(
                Button::new("edit-translate-all")
                    .small()
                    .primary()
                    .label("全部翻译")
                    .disabled(busy || locked)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.translate_edit_fields(EditField::ALL.to_vec(), window, cx)
                    })),
            )
            .child(
                Button::new("edit-write-all")
                    .small()
                    .label("全部写回")
                    .disabled(busy || locked)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.write_back_edit_fields(EditField::ALL.to_vec(), true, window, cx)
                    })),
            );

        let mut body = v_flex().w_full().min_w_0().gap(px(18.));

        if busy {
            body = body.child(
                div()
                    .w_full()
                    .min_w_0()
                    .p(px(10.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(ui::accent_soft(0.4))
                    .bg(ui::accent_soft(0.10))
                    .text_size(px(12.))
                    .text_color(ui::accent())
                    .child("正在处理，请稍候…"),
            );
        }

        if let Some(error) = self.library.edit_error.as_ref() {
            body = body.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .items_start()
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
                        Button::new("edit-error-dismiss")
                            .ghost()
                            .compact()
                            .label("✕")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.library.edit_error = None;
                                cx.notify();
                            })),
                    ),
            );
        }

        for (index, field) in EditField::ALL.iter().enumerate() {
            let field = *field;
            let label = field.label();
            let source = self.library.edit_source.text(field);

            let translated: AnyElement = match field {
                EditField::Title => Textarea::new(&self.library.edit_title)
                    .h(px(48.))
                    .w_full()
                    .disabled(busy)
                    .into_any_element(),
                EditField::OriginalTitle => Textarea::new(&self.library.edit_orig)
                    .h(px(48.))
                    .w_full()
                    .disabled(busy)
                    .into_any_element(),
                EditField::Overview => Textarea::new(&self.library.edit_overview)
                    .h(px(100.))
                    .w_full()
                    .disabled(busy)
                    .into_any_element(),
                EditField::Tags => Textarea::new(&self.library.edit_tags)
                    .h(px(60.))
                    .w_full()
                    .disabled(busy)
                    .into_any_element(),
                EditField::People => Textarea::new(&self.library.edit_people)
                    .h(px(120.))
                    .w_full()
                    .disabled(busy)
                    .into_any_element(),
            };

            let hint = match field {
                EditField::Tags => Some("逗号分隔；写回时转成 Jellyfin Tags 数组。"),
                EditField::People => Some("每行一位，格式：姓名 / 角色；没有角色只写姓名。"),
                _ => None,
            };

            body = body.child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(div().text_size(px(13.)).font_semibold().child(label))
                            .child(div().flex_1())
                            .child(
                                Button::new(format!("edit-translate-{index}"))
                                    .small()
                                    .ghost()
                                    .label("翻译")
                                    .disabled(busy || locked)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.translate_edit_fields(vec![field], window, cx)
                                    })),
                            )
                            .child(
                                Button::new(format!("edit-write-{index}"))
                                    .small()
                                    .ghost()
                                    .label("写回")
                                    .disabled(busy || locked)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.write_back_edit_fields(vec![field], false, window, cx)
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .flex_wrap()
                            .gap_3()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w(px(320.))
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(ui::muted(theme))
                                            .child("原文"),
                                    )
                                    .child(
                                        div()
                                            .w_full()
                                            .min_w_0()
                                            .whitespace_normal()
                                            .p(px(10.))
                                            .rounded(px(8.))
                                            .border_1()
                                            .border_color(ui::border(theme))
                                            .bg(theme.muted)
                                            .text_size(px(12.5))
                                            .text_color(theme.foreground)
                                            .child(if source.is_empty() {
                                                "—".to_string()
                                            } else {
                                                source
                                            }),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w(px(320.))
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .text_size(px(11.5))
                                            .text_color(ui::muted(theme))
                                            .child("译文"),
                                    )
                                    .child(translated)
                                    .when_some(hint, |column, hint| {
                                        column.child(
                                            div()
                                                .w_full()
                                                .min_w_0()
                                                .whitespace_normal()
                                                .text_size(px(11.5))
                                                .text_color(ui::muted(theme))
                                                .child(hint),
                                        )
                                    }),
                            ),
                    ),
            );
        }

        let body = div()
            .id("edit-dialog-body")
            .w_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(px(18.))
            .child(body);

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
                    .child("翻译只改本地草稿；写回会把当前译文提交到 Jellyfin。"),
            )
            .child(
                Button::new("edit-save-draft")
                    .label("保存草稿")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.save_edit_draft(window, cx))),
            )
            .child(
                Button::new("edit-close")
                    .ghost()
                    .label("关闭")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.close_edit_dialog(cx))),
            );

        let card = v_flex()
            .id("edit-dialog-card")
            .occlude()
            .w_full()
            .max_w(px(1080.))
            .h_full()
            .max_h(px(860.))
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

    fn close_edit_dialog(&mut self, cx: &mut Context<Self>) {
        if self.library.edit_busy {
            return;
        }
        self.library.editing = None;
        self.library.edit_error = None;
        cx.notify();
    }

    fn build_edit(&self, cx: &Context<Self>) -> ItemEdit {
        let title = self.library.edit_title.read(cx).value().to_string();
        let original_title = self.library.edit_orig.read(cx).value().to_string();
        let overview = self.library.edit_overview.read(cx).value().to_string();
        let tags = self
            .library
            .edit_tags
            .read(cx)
            .value()
            .to_string()
            .split(|ch| ch == ',' || ch == '，')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        let people = self
            .library
            .edit_people
            .read(cx)
            .value()
            .to_string()
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() {
                    return None;
                }
                let (name, role) = line
                    .split_once(" / ")
                    .or_else(|| line.split_once('/'))
                    .or_else(|| line.split_once('／'))
                    .map(|(name, role)| (name.trim(), role.trim()))
                    .unwrap_or((line, ""));
                if name.is_empty() {
                    return None;
                }
                Some(PersonView {
                    id: None,
                    name: name.to_string(),
                    role: if role.is_empty() {
                        None
                    } else {
                        Some(role.to_string())
                    },
                })
            })
            .collect::<Vec<_>>();

        ItemEdit {
            title: if title.trim().is_empty() {
                "未命名".to_string()
            } else {
                title.trim().to_string()
            },
            original_title,
            overview,
            tags,
            people,
        }
    }

    /// Replace the cached item and refresh only the inputs for `fields`, so
    /// unsaved text in the other fields survives a per-field operation.
    fn apply_refreshed_item(
        &mut self,
        item: ItemView,
        fields: &[EditField],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = item.id.clone();
        let library_id = item.library_id.clone();
        if let Some(items) = self.library.items.get_mut(&library_id) {
            if let Some(slot) = items.iter_mut().find(|cached| cached.id == id) {
                *slot = item.clone();
            }
        }
        self.library.edit_source = EditSource::from_item(&item);

        for field in fields {
            match field {
                EditField::Title => {
                    let value = item.title.clone();
                    self.library
                        .edit_title
                        .update(cx, |state, cx| state.set_value(value, window, cx));
                }
                EditField::OriginalTitle => {
                    let value = item.original_title.clone();
                    self.library
                        .edit_orig
                        .update(cx, |state, cx| state.set_value(value, window, cx));
                }
                EditField::Overview => {
                    let value = item.overview.clone();
                    self.library
                        .edit_overview
                        .update(cx, |state, cx| state.set_value(value, window, cx));
                }
                EditField::Tags => {
                    let value = item.tags.join(", ");
                    self.library
                        .edit_tags
                        .update(cx, |state, cx| state.set_value(value, window, cx));
                }
                EditField::People => {
                    let value = format_people(&item.people);
                    self.library
                        .edit_people
                        .update(cx, |state, cx| state.set_value(value, window, cx));
                }
            }
        }
        cx.notify();
    }

    fn edit_service_ready(&mut self, cx: &mut Context<Self>) -> bool {
        if self.service.is_some() {
            return true;
        }
        self.library.edit_error = Some("本地数据服务尚未就绪".into());
        cx.notify();
        false
    }

    fn save_edit_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.library.edit_busy || !self.edit_service_ready(cx) {
            return;
        }
        let Some(item) = self.library.editing_item() else {
            return;
        };
        let item_id = item.id.clone();
        let library_id = item.library_id.clone();
        let edit = self.build_edit(cx);

        self.library.edit_busy = true;
        self.library.edit_error = None;
        cx.notify();

        self.spawn_async_job_in(
            window,
            cx,
            move |service| {
                Box::pin(async move {
                    service.save_draft(&item_id, edit).map_err(ui::error_text)?;
                    let item = service
                        .items(&library_id)
                        .map_err(ui::error_text)?
                        .into_iter()
                        .find(|item| item.id == item_id);
                    Ok(item)
                })
            },
            |this, result, window, cx| {
                this.library.edit_busy = false;
                match result {
                    Ok(Some(item)) => {
                        // Inputs already hold what was just saved; refresh the
                        // cached item/status/source only.
                        this.apply_refreshed_item(item, &[], window, cx);
                        this.notify_toast("已保存草稿", cx);
                    }
                    Ok(None) => this.notify_toast("已保存草稿", cx),
                    Err(message) => {
                        this.library.edit_error = Some(format!("保存草稿失败：{message}").into())
                    }
                }
                cx.notify();
            },
        );
    }

    fn translate_edit_fields(
        &mut self,
        fields: Vec<EditField>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if fields.is_empty() || self.library.edit_busy || !self.edit_service_ready(cx) {
            return;
        }
        let Some(item) = self.library.editing_item() else {
            return;
        };
        let item_id = item.id.clone();
        let library_id = item.library_id.clone();
        let kinds = fields.iter().map(|field| field.kind()).collect::<Vec<_>>();
        let applied_fields = fields.clone();

        self.library.edit_busy = true;
        self.library.edit_error = None;
        cx.notify();

        self.spawn_async_job_in(
            window,
            cx,
            move |service| {
                Box::pin(async move {
                    let mut templates = service.templates().map_err(ui::error_text)?;
                    fill_default_templates(&mut templates);
                    let settings = service.provider_settings().map_err(ui::error_text)?;
                    let report = service
                        .translate_fields(&item_id, kinds, templates, settings)
                        .await
                        .map_err(ui::error_text)?;
                    let item = service
                        .items(&library_id)
                        .map_err(ui::error_text)?
                        .into_iter()
                        .find(|item| item.id == item_id);
                    Ok((report, item))
                })
            },
            move |this, result, window, cx| {
                this.library.edit_busy = false;
                match result {
                    Ok((report, item)) => {
                        if let Some(item) = item {
                            this.apply_refreshed_item(item, &applied_fields, window, cx);
                        }
                        if let Some((_, error)) = report.failed.first() {
                            let message = translate_error(error);
                            this.library.edit_error = Some(message.clone().into());
                            this.notify_toast(message, cx);
                        } else if report.translated == 0 {
                            this.notify_toast("没有可翻译的内容", cx);
                        } else {
                            this.notify_toast("翻译完成，已更新译文", cx);
                        }
                    }
                    Err(message) => {
                        let message = translate_error(&message);
                        this.library.edit_error = Some(message.clone().into());
                        this.notify_toast(message, cx);
                    }
                }
                cx.notify();
            },
        );
    }

    fn write_back_edit_fields(
        &mut self,
        fields: Vec<EditField>,
        all: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if fields.is_empty() || self.library.edit_busy || !self.edit_service_ready(cx) {
            return;
        }
        let Some(item) = self.library.editing_item() else {
            return;
        };
        let item_id = item.id.clone();
        let library_id = item.library_id.clone();
        let edit = self.build_edit(cx);
        let kinds = fields.iter().map(|field| field.kind()).collect::<Vec<_>>();
        let applied_fields = fields.clone();

        self.library.edit_busy = true;
        self.library.edit_error = None;
        cx.notify();

        self.spawn_async_job_in(
            window,
            cx,
            move |service| {
                Box::pin(async move {
                    service.save_draft(&item_id, edit).map_err(ui::error_text)?;
                    let report = if all {
                        // Full write also promotes the item to Written.
                        service
                            .write_back(vec![item_id.clone()])
                            .await
                            .map_err(ui::error_text)?
                    } else {
                        service
                            .write_back_fields(&item_id, kinds)
                            .await
                            .map_err(ui::error_text)?
                    };
                    let item = service
                        .items(&library_id)
                        .map_err(ui::error_text)?
                        .into_iter()
                        .find(|item| item.id == item_id);
                    Ok((report, item))
                })
            },
            move |this, result, window, cx| {
                this.library.edit_busy = false;
                match result {
                    Ok((report, item)) => {
                        if let Some(item) = item {
                            this.apply_refreshed_item(item, &applied_fields, window, cx);
                        }
                        if let Some((_, error)) = report.failed.first() {
                            let message = format!("写回失败：{error}");
                            this.library.edit_error = Some(message.clone().into());
                            this.notify_toast(message, cx);
                        } else if report.written == 0 {
                            let message = "写回未成功：条目可能已锁定".to_string();
                            this.library.edit_error = Some(message.clone().into());
                            this.notify_toast(message, cx);
                        } else {
                            let message = if all {
                                "已全部写回，条目状态已标记为已写回".to_string()
                            } else {
                                "写回成功".to_string()
                            };
                            if all {
                                this.record_task("写回", message.clone(), true, cx);
                            }
                            this.notify_toast(message, cx);
                        }
                    }
                    Err(message) => {
                        let message = format!("写回失败：{message}");
                        this.library.edit_error = Some(message.clone().into());
                        this.notify_toast(message, cx);
                    }
                }
                cx.notify();
            },
        );
    }
}
