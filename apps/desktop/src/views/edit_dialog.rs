//! 字段列表编辑弹窗 + 单字段编辑小弹窗。
//!
//! 主弹窗只展示服务器原文和每个字段的 [编辑]/[写回]；[编辑] 打开嵌套小弹窗，
//! 在原文旁边编辑译文并单独翻译/保存。标签与演职员按元素逐行编辑。

use ::core::{FieldKind, FieldPreview, ItemEdit, ItemView, PersonView};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{h_flex, v_flex, Disableable as _, Sizable as _, StyledExt as _, Theme};
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
            EditField::People => {
                if self.people.is_empty() {
                    "—".to_string()
                } else {
                    self.people
                        .iter()
                        .map(|person| person.name.clone())
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
        }
    }
}

/// One row in the per-field tags editor.
pub(crate) struct TagRow {
    pub(crate) source: String,
    pub(crate) value: Entity<TextareaState>,
}

/// One row in the per-field people editor.
pub(crate) struct PersonEditRow {
    pub(crate) id: Option<String>,
    pub(crate) source_name: String,
    pub(crate) name: Entity<TextareaState>,
    /// Role is not shown or edited anymore, but must survive a save.
    pub(crate) role: Option<String>,
}

pub(crate) enum FieldEditorState {
    Text {
        field: EditField,
        input: Entity<TextareaState>,
    },
    Tags {
        rows: Vec<TagRow>,
    },
    People {
        rows: Vec<PersonEditRow>,
    },
}

impl FieldEditorState {
    pub(crate) fn field(&self) -> EditField {
        match self {
            FieldEditorState::Text { field, .. } => *field,
            FieldEditorState::Tags { .. } => EditField::Tags,
            FieldEditorState::People { .. } => EditField::People,
        }
    }

    fn heading(&self) -> &'static str {
        self.field().label()
    }
}

fn effective_text(item: &ItemView, field: EditField) -> String {
    match field {
        EditField::Title => item.title.clone(),
        EditField::OriginalTitle => item.original_title.clone(),
        EditField::Overview => item.overview.clone(),
        EditField::Tags => {
            if item.tags.is_empty() {
                "—".to_string()
            } else {
                item.tags.join(", ")
            }
        }
        EditField::People => {
            if item.people.is_empty() {
                "—".to_string()
            } else {
                item.people
                    .iter()
                    .map(|person| person.name.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }
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

fn preview(text: &str) -> String {
    const LIMIT: usize = 160;
    let mut output = text.chars().take(LIMIT).collect::<String>();
    if text.chars().count() > LIMIT {
        output.push('…');
    }
    output
}

fn new_textarea(
    value: &str,
    window: &mut Window,
    cx: &mut Context<AppState>,
) -> Entity<TextareaState> {
    let entity = cx.new(|cx| TextareaState::new(window, cx));
    let value = value.to_string();
    entity.update(cx, |state, cx| state.set_value(value, window, cx));
    entity
}

fn build_tag_rows_from(
    source_tags: &[String],
    translated_tags: &[String],
    window: &mut Window,
    cx: &mut Context<AppState>,
) -> Vec<TagRow> {
    let count = source_tags.len().max(translated_tags.len());
    (0..count)
        .map(|index| {
            let source = source_tags.get(index).cloned().unwrap_or_default();
            let value = translated_tags.get(index).cloned().unwrap_or_default();
            TagRow {
                source,
                value: new_textarea(&value, window, cx),
            }
        })
        .collect()
}

fn build_tag_rows(item: &ItemView, window: &mut Window, cx: &mut Context<AppState>) -> Vec<TagRow> {
    build_tag_rows_from(&item.source.tags, &item.tags, window, cx)
}

fn build_people_rows(
    item: &ItemView,
    window: &mut Window,
    cx: &mut Context<AppState>,
) -> Vec<PersonEditRow> {
    let count = item.source.people.len().max(item.people.len());
    (0..count)
        .map(|index| {
            let source = item.source.people.get(index);
            let translated = item.people.get(index);
            PersonEditRow {
                id: source
                    .and_then(|person| person.id.clone())
                    .or_else(|| translated.and_then(|person| person.id.clone())),
                source_name: source.map(|person| person.name.clone()).unwrap_or_default(),
                name: new_textarea(
                    translated.map(|person| person.name.as_str()).unwrap_or(""),
                    window,
                    cx,
                ),
                role: translated
                    .and_then(|person| person.role.clone())
                    .or_else(|| source.and_then(|person| person.role.clone())),
            }
        })
        .collect()
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
                            .child("列表显示当前值（有草稿用草稿，否则原文）；点「编辑」查看原文并修改译文。"),
                    ),
            );

        let mut body = v_flex().w_full().min_w_0().gap(px(0.));

        if busy {
            body = body.child(
                div()
                    .w_full()
                    .min_w_0()
                    .m(px(18.))
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
                    .m(px(18.))
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

        for field in EditField::ALL {
            let current = effective_text(&item, field);
            let current = if current.is_empty() {
                "—".to_string()
            } else {
                current
            };

            body = body.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .gap_3()
                    .px(px(18.))
                    .py(px(14.))
                    .border_b_1()
                    .border_color(ui::border(theme))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(5.))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_semibold()
                                            .child(field.label()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(ui::muted(theme))
                                            .child("当前值"),
                                    ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .line_clamp(4)
                                    .p(px(10.))
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(ui::border(theme))
                                    .bg(theme.muted)
                                    .text_size(px(12.5))
                                    .text_color(theme.foreground)
                                    .child(current),
                            ),
                    )
                    .child(
                        h_flex().flex_shrink_0().items_center().child(
                            Button::new(format!("edit-field-open-{}", field.label()))
                                .small()
                                .label("编辑")
                                .disabled(busy || locked)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_field_editor(field, window, cx)
                                })),
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
                    .child("写回会把当前草稿提交到 Jellyfin；翻译只改本地草稿。"),
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

    pub(crate) fn render_field_editor(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(editor) = self.library.field_editor.as_ref() else {
            return div().into_any_element();
        };
        let field = editor.field();
        let busy = self.library.field_busy;
        let source = self.library.edit_source.text(field);

        let overlay = div()
            .id("field-editor-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0.0, 0.0, 0.0, 0.55))
            .on_click(cx.listener(|this, _, window, cx| this.close_field_editor(window, cx)));

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
                            .child(format!("编辑字段 · {}", editor.heading())),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::muted(theme))
                            .child("左侧为原文，右侧为当前译文。"),
                    ),
            )
            .child(
                Button::new("field-editor-close-x")
                    .ghost()
                    .compact()
                    .label("✕")
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.close_field_editor(window, cx)),
                    ),
            );

        let mut body = v_flex().w_full().min_w_0().gap(px(12.)).p(px(18.));

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

        if let Some(error) = self.library.field_error.as_ref() {
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

        match editor {
            FieldEditorState::Text { field, input } => {
                let input_height = match field {
                    EditField::Title | EditField::OriginalTitle => px(60.),
                    EditField::Overview => px(160.),
                    _ => px(120.),
                };
                body = body.child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap(px(12.))
                        .child(
                            v_flex()
                                .w_full()
                                .min_w_0()
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
                                        .child(if source.is_empty() {
                                            "—".to_string()
                                        } else {
                                            source.clone()
                                        }),
                                ),
                        )
                        .child(
                            v_flex()
                                .w_full()
                                .min_w_0()
                                .gap(px(4.))
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(ui::muted(theme))
                                        .child("译文"),
                                )
                                .child(
                                    Textarea::new(input).h(input_height).w_full().disabled(busy),
                                ),
                        ),
                );
            }
            FieldEditorState::Tags { rows } => {
                body = body.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .text_size(px(11.5))
                        .text_color(ui::muted(theme))
                        .child(div().flex_1().min_w_0().child("原文"))
                        .child(div().flex_1().min_w_0().child("译文")),
                );
                body = body.child(v_flex().w_full().min_w_0().gap(px(8.)).children(
                    rows.iter().map(|row| {
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .items_start()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .p(px(10.))
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(ui::border(theme))
                                    .bg(theme.muted)
                                    .text_size(px(12.5))
                                    .child(if row.source.is_empty() {
                                        "—".to_string()
                                    } else {
                                        row.source.clone()
                                    }),
                            )
                            .child(div().flex_1().min_w_0().child(
                                Textarea::new(&row.value).h(px(42.)).w_full().disabled(busy),
                            ))
                    }),
                ));
            }
            FieldEditorState::People { rows } => {
                body = body.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .text_size(px(11.5))
                        .text_color(ui::muted(theme))
                        .child(div().flex_1().min_w_0().child("原文姓名"))
                        .child(div().flex_1().min_w_0().child("译文姓名")),
                );
                body = body.child(v_flex().w_full().min_w_0().gap(px(10.)).children(
                    rows.iter().map(|row| {
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .items_start()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .p(px(10.))
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(ui::border(theme))
                                    .bg(theme.muted)
                                    .text_size(px(12.5))
                                    .child(if row.source_name.is_empty() {
                                        "—".to_string()
                                    } else {
                                        row.source_name.clone()
                                    }),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    Textarea::new(&row.name).h(px(40.)).w_full().disabled(busy),
                                ),
                            )
                    }),
                ));
            }
        }

        let body = div()
            .id("field-editor-body")
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
                Button::new("field-editor-translate")
                    .primary()
                    .label("翻译")
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.translate_field_editor(window, cx)),
                    ),
            )
            .child(
                Button::new("field-editor-draft")
                    .label("草稿")
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.save_field_editor(window, cx)),
                    ),
            )
            .child(
                Button::new("field-editor-write")
                    .label("写回")
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.write_field_editor(window, cx)),
                    ),
            )
            .child(
                Button::new("field-editor-close")
                    .ghost()
                    .label("关闭")
                    .disabled(busy)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.close_field_editor(window, cx)),
                    ),
            );

        let card = v_flex()
            .id("field-editor-card")
            .occlude()
            .w_full()
            .max_w(px(860.))
            .h_full()
            .max_h(px(680.))
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

    pub(crate) fn open_field_editor(
        &mut self,
        field: EditField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.library.editing_item() else {
            return;
        };
        self.library.field_error = None;
        self.library.field_busy = false;

        match field {
            EditField::Title => {
                let value = item.title.clone();
                self.library
                    .edit_title
                    .update(cx, |state, cx| state.set_value(value, window, cx));
                self.library.field_editor = Some(FieldEditorState::Text {
                    field,
                    input: self.library.edit_title.clone(),
                });
            }
            EditField::OriginalTitle => {
                let value = item.original_title.clone();
                self.library
                    .edit_orig
                    .update(cx, |state, cx| state.set_value(value, window, cx));
                self.library.field_editor = Some(FieldEditorState::Text {
                    field,
                    input: self.library.edit_orig.clone(),
                });
            }
            EditField::Overview => {
                let value = item.overview.clone();
                self.library
                    .edit_overview
                    .update(cx, |state, cx| state.set_value(value, window, cx));
                self.library.field_editor = Some(FieldEditorState::Text {
                    field,
                    input: self.library.edit_overview.clone(),
                });
            }
            EditField::Tags => {
                let rows = build_tag_rows(&item, window, cx);
                self.library.field_editor = Some(FieldEditorState::Tags { rows });
            }
            EditField::People => {
                let rows = build_people_rows(&item, window, cx);
                self.library.field_editor = Some(FieldEditorState::People { rows });
            }
        }
        cx.notify();
    }

    pub(crate) fn close_field_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.library.field_busy {
            return;
        }
        if let (Some(field), Some(item)) = (
            self.library
                .field_editor
                .as_ref()
                .map(FieldEditorState::field),
            self.library.editing_item(),
        ) {
            // Closing discards this editor's unsaved text: restore the current
            // stored item value. Tags/people rows are simply dropped.
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
                EditField::Tags | EditField::People => {}
            }
        }
        self.library.field_editor = None;
        self.library.field_error = None;
        cx.notify();
    }

    fn close_edit_dialog(&mut self, cx: &mut Context<Self>) {
        if self.library.edit_busy || self.library.field_busy {
            return;
        }
        self.library.editing = None;
        self.library.edit_error = None;
        self.library.field_editor = None;
        self.library.field_error = None;
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

    fn build_edit(&self, cx: &Context<Self>) -> ItemEdit {
        let title = self.library.edit_title.read(cx).value().to_string();
        let original_title = self.library.edit_orig.read(cx).value().to_string();
        let overview = self.library.edit_overview.read(cx).value().to_string();
        ItemEdit {
            title: if title.trim().is_empty() {
                "未命名".to_string()
            } else {
                title.trim().to_string()
            },
            original_title,
            overview,
            tags: self.library.edit_tags.clone(),
            people: self.library.edit_people.clone(),
        }
    }

    /// Replace the cached item and refresh the visible values from the latest
    /// `ItemView`. The caller decides when this is safe (operations overwrite
    /// the affected field only; the field editor rebuilds its own rows).
    fn apply_item_snapshot(
        &mut self,
        item: ItemView,
        applied: &[EditField],
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
        self.library.edit_tags = item.tags.clone();
        self.library.edit_people = item.people.clone();

        // Only overwrite the field(s) the operation touched; unsaved text in
        // other scalar fields must survive.
        if applied.contains(&EditField::Title) {
            let title = item.title.clone();
            self.library
                .edit_title
                .update(cx, |state, cx| state.set_value(title, window, cx));
        }
        if applied.contains(&EditField::OriginalTitle) {
            let original_title = item.original_title.clone();
            self.library
                .edit_orig
                .update(cx, |state, cx| state.set_value(original_title, window, cx));
        }
        if applied.contains(&EditField::Overview) {
            let overview = item.overview.clone();
            self.library
                .edit_overview
                .update(cx, |state, cx| state.set_value(overview, window, cx));
        }

        self.refresh_field_editor(&item, applied, window, cx);
        cx.notify();
    }

    fn refresh_field_editor(
        &mut self,
        item: &ItemView,
        applied: &[EditField],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(field) = self
            .library
            .field_editor
            .as_ref()
            .map(FieldEditorState::field)
        else {
            return;
        };
        if !applied.contains(&field) {
            return;
        }
        match field {
            EditField::Title | EditField::OriginalTitle | EditField::Overview => {}
            EditField::Tags => {
                let rows = build_tag_rows(item, window, cx);
                if let Some(FieldEditorState::Tags { rows: slot }) =
                    self.library.field_editor.as_mut()
                {
                    *slot = rows;
                }
            }
            EditField::People => {
                let rows = build_people_rows(item, window, cx);
                if let Some(FieldEditorState::People { rows: slot }) =
                    self.library.field_editor.as_mut()
                {
                    *slot = rows;
                }
            }
        }
    }

    fn preview_text(&self, field: EditField, field_preview: &FieldPreview) -> String {
        match field {
            EditField::Title => field_preview.title.clone().unwrap_or_default(),
            EditField::OriginalTitle => field_preview.original_title.clone().unwrap_or_default(),
            EditField::Overview => field_preview.overview.clone().unwrap_or_default(),
            EditField::Tags => field_preview
                .tags
                .as_ref()
                .map(|tags| tags.join(", "))
                .unwrap_or_default(),
            EditField::People => field_preview
                .people
                .as_ref()
                .map(|people| {
                    people
                        .iter()
                        .map(|person| person.name.clone())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
        }
    }

    /// Compare the preview value with the server source (never with the store,
    /// which is intentionally untouched by preview translation).
    fn preview_unchanged(&self, field: EditField, field_preview: &FieldPreview) -> bool {
        match field {
            EditField::Title => field_preview
                .title
                .as_deref()
                .map(|value| value.trim() == self.library.edit_source.title.trim())
                .unwrap_or(false),
            EditField::OriginalTitle => field_preview
                .original_title
                .as_deref()
                .map(|value| value.trim() == self.library.edit_source.original_title.trim())
                .unwrap_or(false),
            EditField::Overview => field_preview
                .overview
                .as_deref()
                .map(|value| value.trim() == self.library.edit_source.overview.trim())
                .unwrap_or(false),
            EditField::Tags => {
                let Some(tags) = field_preview.tags.as_ref() else {
                    return false;
                };
                let source = self
                    .library
                    .edit_source
                    .tags
                    .iter()
                    .map(|tag| tag.trim())
                    .collect::<Vec<_>>();
                let translated = tags.iter().map(|tag| tag.trim()).collect::<Vec<_>>();
                !translated.is_empty() && translated == source
            }
            EditField::People => {
                let Some(people) = field_preview.people.as_ref() else {
                    return false;
                };
                let source = self
                    .library
                    .edit_source
                    .people
                    .iter()
                    .map(|person| person.name.trim())
                    .collect::<Vec<_>>();
                let translated = people
                    .iter()
                    .map(|person| person.name.trim())
                    .collect::<Vec<_>>();
                !translated.is_empty() && translated == source
            }
        }
    }

    fn log_preview(&self, field: EditField, field_preview: &FieldPreview, unchanged: bool) {
        let source_text = preview(&self.library.edit_source.text(field));
        let translated_text = preview(&self.preview_text(field, field_preview));
        tracing::debug!(
            field = field.label(),
            unchanged,
            source = %source_text,
            translated = %translated_text,
            "edit_field_preview_translated"
        );
        #[cfg(debug_assertions)]
        eprintln!(
            "[edit-field-preview] field={} unchanged={unchanged} source={source_text:?} translated={translated_text:?}",
            field.label()
        );
    }

    /// Fill the preview result into the editor inputs only. Nothing is written
    /// to the store here.
    fn apply_preview(
        &mut self,
        field: EditField,
        field_preview: &FieldPreview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let missing = || "翻译未返回该字段结果，请检查模型/提示词".to_string();
        match field {
            EditField::Title => {
                let value = field_preview.title.clone().ok_or_else(missing)?;
                self.library
                    .edit_title
                    .update(cx, |state, cx| state.set_value(value, window, cx));
            }
            EditField::OriginalTitle => {
                let value = field_preview.original_title.clone().ok_or_else(missing)?;
                self.library
                    .edit_orig
                    .update(cx, |state, cx| state.set_value(value, window, cx));
            }
            EditField::Overview => {
                let value = field_preview.overview.clone().ok_or_else(missing)?;
                self.library
                    .edit_overview
                    .update(cx, |state, cx| state.set_value(value, window, cx));
            }
            EditField::Tags => {
                let tags = field_preview.tags.clone().ok_or_else(missing)?;
                let source = self.library.edit_source.tags.clone();
                let rows = build_tag_rows_from(&source, &tags, window, cx);
                if let Some(FieldEditorState::Tags { rows: slot }) =
                    self.library.field_editor.as_mut()
                {
                    *slot = rows;
                }
            }
            EditField::People => {
                let people = field_preview.people.clone().ok_or_else(missing)?;
                if let Some(FieldEditorState::People { rows }) = self.library.field_editor.as_mut()
                {
                    for (index, row) in rows.iter_mut().enumerate() {
                        if let Some(person) = people.get(index) {
                            let value = person.name.clone();
                            row.name
                                .update(cx, |state, cx| state.set_value(value, window, cx));
                        }
                    }
                }
                // row.role and row.id stay untouched: the dialog hides roles
                // and must not clear the server-side value.
            }
        }
        Ok(())
    }

    /// Copy the nested editor rows back into the canonical draft values.
    fn materialize_field_editor(&mut self, cx: &Context<Self>) -> Option<EditField> {
        let field = self
            .library
            .field_editor
            .as_ref()
            .map(FieldEditorState::field)?;

        let mut tags = None;
        let mut people = None;
        match &self.library.field_editor {
            Some(FieldEditorState::Tags { rows }) => {
                tags = Some(
                    rows.iter()
                        .map(|row| row.value.read(cx).value().trim().to_string())
                        .filter(|tag| !tag.is_empty())
                        .collect::<Vec<_>>(),
                );
            }
            Some(FieldEditorState::People { rows }) => {
                people = Some(
                    rows.iter()
                        .filter_map(|row| {
                            let name = row.name.read(cx).value().trim().to_string();
                            if name.is_empty() {
                                return None;
                            }
                            // Role is hidden in the new UI; keep the stored
                            // value instead of clearing it on save.
                            Some(PersonView {
                                id: row.id.clone(),
                                name,
                                role: row.role.clone(),
                            })
                        })
                        .collect::<Vec<_>>(),
                );
            }
            _ => {}
        }
        if let Some(tags) = tags {
            self.library.edit_tags = tags;
        }
        if let Some(people) = people {
            self.library.edit_people = people;
        }
        Some(field)
    }

    fn save_field_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.library.field_busy || self.library.field_editor.is_none() {
            return;
        }
        if !self.edit_service_ready(cx) {
            return;
        }
        let Some(item) = self.library.editing_item() else {
            return;
        };
        let Some(editor_field) = self.materialize_field_editor(cx) else {
            return;
        };
        let item_id = item.id.clone();
        let library_id = item.library_id.clone();
        let edit = self.build_edit(cx);
        self.library.field_busy = true;
        self.library.field_error = None;
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
            move |this, result, window, cx| {
                this.library.field_busy = false;
                match result {
                    Ok(Some(item)) => {
                        this.apply_item_snapshot(item, &[editor_field], window, cx);
                        this.notify_toast("已保存草稿", cx);
                    }
                    Ok(None) => this.notify_toast("已保存草稿", cx),
                    Err(message) => {
                        let message = format!("保存草稿失败：{message}");
                        this.library.field_error = Some(message.clone().into());
                        this.notify_toast(message, cx);
                    }
                }
                cx.notify();
            },
        );
    }

    fn translate_field_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.library.field_busy {
            return;
        }
        let Some(editor) = self.library.field_editor.as_ref() else {
            return;
        };
        let field = editor.field();
        let Some(item) = self.library.editing_item() else {
            return;
        };
        if !self.edit_service_ready(cx) {
            return;
        }
        let item_id = item.id.clone();
        let kind = field.kind();

        self.library.field_busy = true;
        self.library.field_error = None;
        cx.notify();

        self.spawn_async_job_in(
            window,
            cx,
            move |service| {
                Box::pin(async move {
                    let mut templates = service.templates().map_err(ui::error_text)?;
                    fill_default_templates(&mut templates);
                    let settings = service.provider_settings().map_err(ui::error_text)?;
                    service
                        .translate_fields_preview(&item_id, vec![kind], templates, settings)
                        .await
                        .map_err(ui::error_text)
                })
            },
            move |this, result, window, cx| {
                this.library.field_busy = false;
                match result {
                    Ok(field_preview) => {
                        let unchanged = this.preview_unchanged(field, &field_preview);
                        this.log_preview(field, &field_preview, unchanged);
                        match this.apply_preview(field, &field_preview, window, cx) {
                            Ok(()) => {
                                if unchanged {
                                    this.notify_toast("翻译结果与原文相同，请检查模型/提示词", cx);
                                } else {
                                    this.notify_toast("翻译完成，已填入译文（未保存）", cx);
                                }
                            }
                            Err(message) => {
                                this.library.field_error = Some(message.clone().into());
                                this.notify_toast(message, cx);
                            }
                        }
                    }
                    Err(message) => {
                        let message = translate_error(&message);
                        this.library.field_error = Some(message.clone().into());
                        this.notify_toast(message, cx);
                    }
                }
                cx.notify();
            },
        );
    }

    fn field_matches_source(&self, field: EditField, item: &ItemView) -> bool {
        match field {
            EditField::Title => item.title.trim() == item.source.title.trim(),
            EditField::OriginalTitle => {
                item.original_title.trim() == item.source.original_title.trim()
            }
            EditField::Overview => item.overview.trim() == item.source.overview.trim(),
            EditField::Tags => {
                let source = item
                    .source
                    .tags
                    .iter()
                    .map(|tag| tag.trim())
                    .collect::<Vec<_>>();
                let effective = item.tags.iter().map(|tag| tag.trim()).collect::<Vec<_>>();
                !effective.is_empty() && effective == source
            }
            EditField::People => {
                let source = item
                    .source
                    .people
                    .iter()
                    .map(|person| person.name.trim())
                    .collect::<Vec<_>>();
                let effective = item
                    .people
                    .iter()
                    .map(|person| person.name.trim())
                    .collect::<Vec<_>>();
                !effective.is_empty() && effective == source
            }
        }
    }

    fn write_field_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.library.field_busy || !self.edit_service_ready(cx) {
            return;
        }
        let Some(field) = self.materialize_field_editor(cx) else {
            return;
        };
        let Some(item) = self.library.editing_item() else {
            return;
        };
        let item_id = item.id.clone();
        let library_id = item.library_id.clone();
        let edit = self.build_edit(cx);
        let kind = field.kind();

        self.library.field_busy = true;
        self.library.field_error = None;
        cx.notify();

        self.spawn_async_job_in(
            window,
            cx,
            move |service| {
                Box::pin(async move {
                    service.save_draft(&item_id, edit).map_err(ui::error_text)?;
                    let report = service
                        .write_back_fields(&item_id, vec![kind])
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
                this.library.field_busy = false;
                match result {
                    Ok((report, item)) => {
                        let matches_source = item
                            .as_ref()
                            .map(|item| this.field_matches_source(field, item))
                            .unwrap_or(false);
                        if let Some(item) = item {
                            this.apply_item_snapshot(item, &[field], window, cx);
                        }
                        if let Some((_, error)) = report.failed.first() {
                            let message = format!("写回失败：{error}");
                            this.library.field_error = Some(message.clone().into());
                            this.notify_toast(message, cx);
                        } else if report.written == 0 {
                            let message = "写回未成功：条目可能已锁定".to_string();
                            this.library.field_error = Some(message.clone().into());
                            this.notify_toast(message, cx);
                        } else if matches_source {
                            this.notify_toast("写回成功，但本地当前值与原文相同，请检查", cx);
                        } else {
                            this.notify_toast("已写回", cx);
                        }
                    }
                    Err(message) => {
                        let message = format!("写回失败：{message}");
                        this.library.field_error = Some(message.clone().into());
                        this.notify_toast(message, cx);
                    }
                }
                cx.notify();
            },
        );
    }
}
