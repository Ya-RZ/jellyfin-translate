//! 媒体库页：媒体库 tab、海报网格、多选工具栏与详情抽屉。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ::core::{ItemView, LibraryView, PersonView, TranslationStatus};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputState, TextareaState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{h_flex, v_flex, Disableable as _, Sizable as _, StyledExt as _, Theme};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{AppState, Page};
use crate::covers;
use crate::ui;
use crate::views::edit_dialog::{EditSource, FieldEditorState};

/// Status filter chips above the poster grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusFilter {
    All,
    Untranslated,
    Draft,
    Written,
    Locked,
}

impl StatusFilter {
    fn label(self) -> &'static str {
        match self {
            StatusFilter::All => "全部",
            StatusFilter::Untranslated => "未翻译",
            StatusFilter::Draft => "草稿",
            StatusFilter::Written => "已写回",
            StatusFilter::Locked => "已锁定",
        }
    }
}

/// Everything the library page owns.
pub(crate) struct LibraryState {
    pub(crate) libraries: Vec<LibraryView>,
    pub(crate) items: HashMap<String, Vec<ItemView>>,
    pub(crate) active_lib: Option<String>,
    pub(crate) filter: StatusFilter,
    pub(crate) multi: bool,
    pub(crate) selected: HashSet<String>,
    pub(crate) drawer: Option<String>,
    pub(crate) editing: Option<String>,
    pub(crate) edit_busy: bool,
    pub(crate) edit_error: Option<SharedString>,
    pub(crate) edit_source: EditSource,
    /// Current draft values for the two multi-row fields.
    pub(crate) edit_tags: Vec<String>,
    pub(crate) edit_people: Vec<PersonView>,
    pub(crate) field_editor: Option<FieldEditorState>,
    pub(crate) field_busy: bool,
    pub(crate) field_error: Option<SharedString>,
    pub(crate) loading_libraries: bool,
    pub(crate) loading_items: bool,
    pub(crate) error: Option<SharedString>,
    pub(crate) search: Entity<InputState>,
    pub(crate) edit_title: Entity<TextareaState>,
    pub(crate) edit_orig: Entity<TextareaState>,
    pub(crate) edit_overview: Entity<TextareaState>,
}

impl LibraryState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<AppState>) -> Self {
        Self {
            libraries: Vec::new(),
            items: HashMap::new(),
            active_lib: None,
            filter: StatusFilter::All,
            multi: false,
            selected: HashSet::new(),
            drawer: None,
            editing: None,
            edit_busy: false,
            edit_error: None,
            edit_source: EditSource::default(),
            edit_tags: Vec::new(),
            edit_people: Vec::new(),
            field_editor: None,
            field_busy: false,
            field_error: None,
            loading_libraries: false,
            loading_items: false,
            error: None,
            search: cx.new(|cx| InputState::new(window, cx).placeholder("搜索标题、演员、标签…")),
            edit_title: cx.new(|cx| TextareaState::new(window, cx)),
            edit_orig: cx.new(|cx| TextareaState::new(window, cx)),
            edit_overview: cx.new(|cx| TextareaState::new(window, cx)),
        }
    }

    pub(crate) fn active_items(&self) -> &[ItemView] {
        self.active_lib
            .as_ref()
            .and_then(|id| self.items.get(id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub(crate) fn active_library(&self) -> Option<&LibraryView> {
        let id = self.active_lib.as_ref()?;
        self.libraries.iter().find(|library| &library.id == id)
    }

    pub(crate) fn item_by_id(&self, id: &str) -> Option<&ItemView> {
        self.items
            .values()
            .flat_map(|items| items.iter())
            .find(|item| item.id == id)
    }

    pub(crate) fn drawer_item(&self) -> Option<ItemView> {
        self.drawer
            .as_deref()
            .and_then(|id| self.item_by_id(id))
            .cloned()
    }

    pub(crate) fn editing_item(&self) -> Option<ItemView> {
        self.editing
            .as_deref()
            .and_then(|id| self.item_by_id(id))
            .cloned()
    }

    fn counts(&self) -> [usize; 5] {
        let mut counts = [0usize; 5];
        counts[0] = self.active_items().len();
        for item in self.active_items() {
            if item.locked {
                counts[4] += 1;
            }
            match item.status {
                TranslationStatus::Untranslated => {
                    if !item.locked {
                        counts[1] += 1;
                    }
                }
                TranslationStatus::Draft => counts[2] += 1,
                TranslationStatus::Written => counts[3] += 1,
            }
        }
        counts
    }

    fn matches_filter(item: &ItemView, filter: StatusFilter) -> bool {
        match filter {
            StatusFilter::All => true,
            StatusFilter::Untranslated => {
                !item.locked && item.status == TranslationStatus::Untranslated
            }
            StatusFilter::Draft => item.status == TranslationStatus::Draft,
            StatusFilter::Written => item.status == TranslationStatus::Written,
            StatusFilter::Locked => item.locked,
        }
    }

    fn visible_items(&self, search: &str) -> Vec<&ItemView> {
        let needle = search.trim().to_lowercase();
        self.active_items()
            .iter()
            .filter(|item| Self::matches_filter(item, self.filter))
            .filter(|item| {
                if needle.is_empty() {
                    return true;
                }
                item.title.to_lowercase().contains(&needle)
                    || item.original_title.to_lowercase().contains(&needle)
                    || item
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&needle))
                    || item.people.iter().any(|person| {
                        person.name.to_lowercase().contains(&needle)
                            || person
                                .role
                                .as_ref()
                                .map(|role| role.to_lowercase().contains(&needle))
                                .unwrap_or(false)
                    })
            })
            .collect()
    }

    fn selected_ids(&self) -> Vec<String> {
        self.selected.iter().cloned().collect()
    }
}

impl AppState {
    // ------------------------------------------------------------- data loads

    pub(crate) fn load_libraries(&mut self, cx: &mut Context<Self>) {
        if self.service.is_none() || self.library.loading_libraries {
            return;
        }
        self.library.loading_libraries = true;
        self.library.error = None;
        cx.notify();

        self.spawn_job(
            cx,
            |service| service.libraries().map_err(ui::error_text),
            |this, result, cx| {
                this.library.loading_libraries = false;
                match result {
                    Ok(libraries) => {
                        this.library.libraries = libraries;
                        if this.library.active_lib.is_none() {
                            this.library.active_lib = this
                                .library
                                .libraries
                                .first()
                                .map(|library| library.id.clone());
                        }
                        if let Some(id) = this.library.active_lib.clone() {
                            if !this.library.items.contains_key(&id) {
                                this.load_items(id, cx);
                            }
                        }
                    }
                    Err(message) => this.library.error = Some(message.into()),
                }
            },
        );
    }

    pub(crate) fn load_items(&mut self, library_id: String, cx: &mut Context<Self>) {
        if self.service.is_none() {
            return;
        }
        self.library.loading_items = true;
        self.library.error = None;
        cx.notify();

        let id_for_result = library_id.clone();
        self.spawn_job(
            cx,
            move |service| service.items(&library_id).map_err(ui::error_text),
            move |this, result, cx| {
                this.library.loading_items = false;
                match result {
                    Ok(items) => {
                        let item_ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
                        this.library.items.insert(id_for_result, items);
                        for item_id in item_ids {
                            this.request_item_cover(&item_id, covers::POSTER_WIDTH, cx);
                        }
                    }
                    Err(message) => this.library.error = Some(message.into()),
                }
                cx.notify();
            },
        );
    }

    pub(crate) fn reload_active(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.library.active_lib.clone() {
            self.load_items(id, cx);
        }
    }

    fn switch_library(&mut self, library_id: String, cx: &mut Context<Self>) {
        self.library.active_lib = Some(library_id.clone());
        self.library.filter = StatusFilter::All;
        self.library.selected.clear();
        if !self.library.items.contains_key(&library_id) {
            self.load_items(library_id, cx);
        }
        cx.notify();
    }

    fn set_multi(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.library.multi = enabled;
        if !enabled {
            self.library.selected.clear();
        }
        cx.notify();
    }

    fn on_poster_click(&mut self, id: String, cx: &mut Context<Self>) {
        if self.library.multi {
            if !self.library.selected.remove(&id) {
                self.library.selected.insert(id);
            }
            cx.notify();
        } else {
            self.library.drawer = Some(id.clone());
            self.request_item_cover(&id, covers::DRAWER_WIDTH, cx);
            if let Some(item) = self.library.item_by_id(&id).cloned() {
                for person in item.people {
                    if let Some(person_id) = person.id.as_deref() {
                        self.request_person_cover(person_id, covers::PERSON_WIDTH, cx);
                    }
                }
            }
            cx.notify();
        }
    }

    // --------------------------------------------------------------- editing

    pub(crate) fn open_edit(
        &mut self,
        item_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.library.item_by_id(&item_id).cloned() else {
            return;
        };
        if item.locked {
            self.notify_toast("已锁定，先解锁再编辑", cx);
            return;
        }

        self.library.editing = Some(item_id);
        self.library.edit_source = EditSource::from_item(&item);
        self.library.edit_tags = item.tags.clone();
        self.library.edit_people = item.people.clone();
        self.library.edit_busy = false;
        self.library.edit_error = None;
        self.library.field_editor = None;
        self.library.field_busy = false;
        self.library.field_error = None;
        self.library.edit_title.update(cx, |state, cx| {
            state.set_value(item.title.clone(), window, cx)
        });
        self.library.edit_orig.update(cx, |state, cx| {
            state.set_value(item.original_title.clone(), window, cx)
        });
        self.library.edit_overview.update(cx, |state, cx| {
            state.set_value(item.overview.clone(), window, cx)
        });
        cx.notify();
    }

    // --------------------------------------------------------------- actions

    pub(crate) fn translate_items(
        &mut self,
        ids: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ids.is_empty() {
            self.notify_toast("没有可翻译的条目（已锁定的会跳过）", cx);
            return;
        }

        self.spawn_async_job_in(
            window,
            cx,
            move |service| {
                Box::pin(async move {
                    let mut templates = service.templates().map_err(ui::error_text)?;
                    crate::views::settings::fill_default_templates(&mut templates);
                    let settings = service.provider_settings().map_err(ui::error_text)?;
                    service
                        .translate(ids, templates, settings)
                        .await
                        .map_err(ui::error_text)
                })
            },
            |this, result, _window, cx| match result {
                Ok(report) => {
                    let message = if report.failed.is_empty() {
                        format!("已生成 {} 条草稿（未写回）", report.translated)
                    } else {
                        let first = report
                            .failed
                            .first()
                            .map(|(title, error)| format!("{title}: {error}"))
                            .unwrap_or_default();
                        format!(
                            "翻译 {} 条，失败 {} 条 · {first}",
                            report.translated,
                            report.failed.len()
                        )
                    };
                    this.record_task("翻译", message.clone(), report.failed.is_empty(), cx);
                    this.notify_toast(message, cx);
                    this.reload_active(cx);
                }
                Err(message) => {
                    this.record_task("翻译", message.clone(), false, cx);
                    // Keep the drawer open: the user should see the dialog on
                    // top of it and be able to jump straight to settings.
                    this.show_error("翻译失败", message, cx);
                }
            },
        );
    }

    pub(crate) fn write_back_items(
        &mut self,
        ids: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ids.is_empty() {
            self.notify_toast("没有待写回的草稿", cx);
            return;
        }

        let was_edit = self.library.editing.is_some();
        self.spawn_async_job_in(
            window,
            cx,
            move |service| {
                Box::pin(async move { service.write_back(ids).await.map_err(ui::error_text) })
            },
            move |this, result, _window, cx| match result {
                Ok(report) => {
                    let message = if report.failed.is_empty() {
                        format!("已写回 {} 条", report.written)
                    } else {
                        let first = report
                            .failed
                            .first()
                            .map(|(title, error)| format!("{title}: {error}"))
                            .unwrap_or_default();
                        format!(
                            "已写回 {} 条，失败 {} 条 · {first}",
                            report.written,
                            report.failed.len()
                        )
                    };
                    this.record_task("写回", message.clone(), report.failed.is_empty(), cx);
                    this.notify_toast(message, cx);
                    if was_edit {
                        this.library.editing = None;
                    }
                    this.reload_active(cx);
                }
                Err(message) => {
                    this.record_task("写回", message.clone(), false, cx);
                    this.show_error("写回失败", message, cx);
                }
            },
        );
    }

    pub(crate) fn set_items_locked(
        &mut self,
        ids: Vec<String>,
        locked: bool,
        cx: &mut Context<Self>,
    ) {
        if ids.is_empty() {
            return;
        }
        let count = ids.len();
        self.spawn_job(
            cx,
            move |service| service.set_locked(&ids, locked).map_err(ui::error_text),
            move |this, result, cx| {
                match result {
                    Ok(()) => {
                        let message =
                            format!("已{} {} 条", if locked { "锁定" } else { "解锁" }, count);
                        this.notify_toast(message, cx);
                        this.reload_active(cx);
                    }
                    Err(message) => this.notify_toast(message, cx),
                }
                cx.notify();
            },
        );
    }

    fn batch_translate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self
            .library
            .selected_ids()
            .into_iter()
            .filter(|id| {
                self.library
                    .item_by_id(id)
                    .map(|item| !item.locked)
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        self.translate_items(ids, window, cx);
    }

    fn batch_write(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self
            .library
            .selected_ids()
            .into_iter()
            .filter(|id| {
                self.library
                    .item_by_id(id)
                    .map(|item| !item.locked && item.status == TranslationStatus::Draft)
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        self.write_back_items(ids, window, cx);
    }

    fn batch_lock(&mut self, locked: bool, cx: &mut Context<Self>) {
        let ids = self.library.selected_ids();
        self.set_items_locked(ids, locked, cx);
    }

    // ---------------------------------------------------------------- render

    pub(crate) fn render_library(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let counts = self.library.counts();
        let active_name = self
            .library
            .active_library()
            .map(|library| library.name.clone())
            .unwrap_or_else(|| "媒体库".to_string());
        let description = if self.library.active_library().is_some() {
            format!(
                "{} 个条目 · 未翻译 {} · 草稿 {} · 已写回 {} · 已锁定 {}",
                counts[0], counts[1], counts[2], counts[3], counts[4]
            )
        } else if self.library.loading_libraries {
            "正在读取本地媒体库索引…".to_string()
        } else {
            "本地还没有媒体库，先在「连接服务器」页连接并同步".to_string()
        };

        let selected_count = self.library.selected.len();
        let has_selection = self.library.multi && selected_count > 0;

        let mut head = h_flex()
            .w_full()
            .items_end()
            .flex_wrap()
            .gap_3()
            .pt(px(22.))
            .pb(px(16.))
            .child(ui::page_head(active_name, description, theme))
            .child(div().flex_1())
            .child(
                Button::new("sync-libraries")
                    .small()
                    .label("同步媒体库")
                    .disabled(self.sync.running)
                    .on_click(cx.listener(|this, _, _, cx| this.open_sync_picker(cx))),
            );

        if self.library.multi {
            head = head.child(
                div()
                    .text_size(px(12.))
                    .text_color(ui::muted(theme))
                    .child(format!("已选 {selected_count}")),
            );
        }

        head = head.child(
            Switch::new("multi-toggle")
                .checked(self.library.multi)
                .label("多选")
                .on_change(cx.listener(|this, checked: &bool, _, cx| this.set_multi(*checked, cx))),
        );

        head = head
            .child(
                Button::new("batch-lock")
                    .small()
                    .label("锁定")
                    .disabled(!has_selection)
                    .on_click(cx.listener(|this, _, _, cx| this.batch_lock(true, cx))),
            )
            .child(
                Button::new("batch-unlock")
                    .small()
                    .label("解锁")
                    .disabled(!has_selection)
                    .on_click(cx.listener(|this, _, _, cx| this.batch_lock(false, cx))),
            )
            .child(
                Button::new("batch-write")
                    .small()
                    .label("写回选中")
                    .disabled(!has_selection)
                    .on_click(cx.listener(|this, _, window, cx| this.batch_write(window, cx))),
            )
            .child(
                Button::new("batch-translate")
                    .small()
                    .primary()
                    .label("翻译选中")
                    .disabled(!has_selection)
                    .on_click(cx.listener(|this, _, window, cx| this.batch_translate(window, cx))),
            );

        let tabs = h_flex()
            .w_full()
            .gap(px(4.))
            .border_b_1()
            .border_color(ui::border(theme))
            .mb(px(16.))
            .when(self.library.libraries.is_empty(), |row| {
                row.child(
                    div()
                        .py(px(9.))
                        .text_size(px(13.))
                        .text_color(ui::muted(theme))
                        .child("暂无媒体库"),
                )
            })
            .children(self.library.libraries.iter().map(|library| {
                let active = self.library.active_lib.as_deref() == Some(library.id.as_str());
                let library_id = library.id.clone();
                h_flex()
                    .id(format!("lib-tab-{}", library.id))
                    .gap(px(6.))
                    .px(px(14.))
                    .py(px(9.))
                    .border_b_2()
                    .border_color(if active {
                        ui::accent()
                    } else {
                        ui::transparent()
                    })
                    .cursor_pointer()
                    .text_size(px(13.))
                    .text_color(if active {
                        theme.foreground
                    } else {
                        ui::muted(theme)
                    })
                    .font_weight(if active {
                        gpui_kit::FontWeight::SEMIBOLD
                    } else {
                        gpui_kit::FontWeight::NORMAL
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.switch_library(library_id.clone(), cx)
                    }))
                    .child(library.name.clone())
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(ui::muted(theme))
                            .child(library.item_count.to_string()),
                    )
            }));

        let chip = |filter: StatusFilter, count: usize| {
            let active = self.library.filter == filter;
            div()
                .id(("filter", filter as usize))
                .h(px(30.))
                .px(px(12.))
                .rounded_full()
                .border_1()
                .border_color(if active {
                    ui::accent_soft(0.5)
                } else {
                    ui::border(theme)
                })
                .bg(if active {
                    ui::accent_soft(0.16)
                } else {
                    ui::transparent()
                })
                .cursor_pointer()
                .text_size(px(12.5))
                .text_color(if active {
                    theme.foreground
                } else {
                    ui::muted(theme)
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.library.filter = filter;
                    cx.notify();
                }))
                .child(
                    h_flex().gap_1().child(filter.label()).child(
                        div()
                            .text_size(px(11.))
                            .text_color(ui::muted(theme))
                            .child(count.to_string()),
                    ),
                )
        };

        let chips = h_flex()
            .w_full()
            .flex_wrap()
            .gap_2()
            .mb(px(16.))
            .child(chip(StatusFilter::All, counts[0]))
            .child(chip(StatusFilter::Untranslated, counts[1]))
            .child(chip(StatusFilter::Draft, counts[2]))
            .child(chip(StatusFilter::Written, counts[3]))
            .child(chip(StatusFilter::Locked, counts[4]));

        let search = self.library.search.read(cx).value().to_string();
        let visible = self.library.visible_items(&search);

        let grid: AnyElement = if self.library.libraries.is_empty() {
            if self.library.loading_libraries {
                ui::card(theme)
                    .p(px(40.))
                    .text_center()
                    .text_size(px(13.))
                    .text_color(ui::muted(theme))
                    .child("正在读取媒体库…")
                    .into_any_element()
            } else {
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
                            "没有媒体库。先连接服务器，再同步索引。".to_string()
                        }
                    });

                let mut empty = ui::card(theme)
                    .p(px(40.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(ui::muted(theme))
                            .child(progress),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(Button::new("go-connect").label("去连接服务器").on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.navigate_external(Page::Connect, window, cx)
                                }),
                            ))
                            .child(
                                Button::new("sync-from-empty")
                                    .primary()
                                    .label(if running {
                                        "同步中…"
                                    } else {
                                        "同步媒体库"
                                    })
                                    .loading(running)
                                    .disabled(running || self.service.is_none())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.open_sync_picker(cx)),
                                    ),
                            ),
                    );

                if let Some(report) = self.sync.report.as_ref() {
                    empty = empty.child(
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
                    empty = empty.child(
                        div()
                            .text_size(px(11.5))
                            .text_color(ui::red(theme))
                            .child(format!("同步失败：{error}")),
                    );
                }

                empty.into_any_element()
            }
        } else if visible.is_empty() {
            let running = self.sync.running;
            let mut empty = ui::card(theme)
                .p(px(40.))
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .child(div().text_size(px(13.)).text_color(ui::muted(theme)).child(
                    if self.library.loading_items {
                        "正在读取条目…".to_string()
                    } else if self.library.active_items().is_empty() {
                        "这个媒体库还没有条目，同步后会出现在这里。".to_string()
                    } else {
                        "没有符合筛选条件的条目".to_string()
                    },
                ));

            if !self.library.loading_items {
                empty = empty.child(
                    Button::new("sync-from-items")
                        .primary()
                        .label(if running {
                            "同步中…"
                        } else {
                            "同步媒体库"
                        })
                        .loading(running)
                        .disabled(running || self.service.is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.open_sync_picker(cx))),
                );
            }

            empty.into_any_element()
        } else {
            h_flex()
                .w_full()
                .flex_wrap()
                .gap(px(16.))
                .pb(px(40.))
                .children(visible.into_iter().map(|item| {
                    let selected = self.library.selected.contains(&item.id);
                    let cover = self
                        .covers
                        .image(&covers::item_key(&item.id, covers::POSTER_WIDTH));
                    render_poster(item, selected, self.library.multi, cover, theme, cx)
                }))
                .into_any_element()
        };

        v_flex()
            .w_full()
            .px(px(24.))
            .child(head)
            .when_some(self.library.error.as_ref(), |column, error| {
                column.child(
                    div()
                        .mb(px(12.))
                        .p(px(10.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(ui::red(theme))
                        .bg(ui::red(theme).opacity(0.10))
                        .text_size(px(12.))
                        .text_color(ui::red(theme))
                        .child(error.clone()),
                )
            })
            .child(tabs)
            .child(chips)
            .child(grid)
            .into_any_element()
    }
}

fn render_poster(
    item: &ItemView,
    selected: bool,
    multi: bool,
    cover: Option<Arc<Image>>,
    theme: &Theme,
    cx: &mut Context<AppState>,
) -> AnyElement {
    let item_id = item.id.clone();
    let title = item.title.clone();
    let subtitle = if item.original_title.is_empty() {
        item.item_type.clone()
    } else {
        item.original_title.clone()
    };

    let mut art = v_flex()
        .relative()
        .w_full()
        .h(px(222.))
        .p(px(12.))
        .justify_end()
        .bg(ui::poster_background(&item.id));

    if let Some(image) = cover {
        art = art.child(
            img(image)
                .absolute()
                .inset_0()
                .size_full()
                .object_fit(ObjectFit::Cover),
        );
    }

    // The title is rendered below the artwork only; overlaying it on the
    // poster duplicated the text and covered the artwork.

    if multi {
        art = art.child(
            div()
                .absolute()
                .top(px(8.))
                .right(px(8.))
                .size(px(20.))
                .rounded(px(6.))
                .border_1()
                .border_color(if selected {
                    ui::accent()
                } else {
                    hsla(0.0, 0.0, 1.0, 0.45)
                })
                .bg(if selected {
                    ui::accent()
                } else {
                    hsla(0.0, 0.0, 0.0, 0.45)
                })
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(13.))
                .font_semibold()
                .text_color(theme.primary_foreground)
                .child(if selected { "✓" } else { "" }),
        );
    }

    v_flex()
        .id(format!("poster-{}", item.id))
        .w(px(158.))
        .flex_shrink_0()
        .relative()
        .rounded(px(10.))
        .overflow_hidden()
        .border_1()
        .border_color(if selected {
            ui::accent()
        } else {
            ui::border(theme)
        })
        .bg(theme.group_box)
        .cursor_pointer()
        .hover(|style| style.border_color(ui::accent_soft(0.5)))
        .on_click(cx.listener(move |this, _, _, cx| this.on_poster_click(item_id.clone(), cx)))
        .child(
            div()
                .relative()
                .child(art)
                .child(ui::status_badge(item.status, item.locked, theme)),
        )
        .child(
            v_flex()
                .gap(px(2.))
                .px(px(10.))
                .py(px(9.))
                .child(
                    div()
                        .text_size(px(12.5))
                        .font_semibold()
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(ui::muted(theme))
                        .truncate()
                        .child(subtitle),
                ),
        )
        .into_any_element()
}
