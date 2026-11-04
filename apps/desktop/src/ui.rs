//! Shared presentation helpers for the UI/UX v2 shell.
//!
//! Everything here is presentation only: the palette, the status badges,
//! poster placeholder gradients and the small reusable row/label pieces.
//! No I/O and no application state lives in this module.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ::core::TranslationStatus;
use gpui_kit::component::{h_flex, v_flex, StyledExt as _, Theme};
use gpui_kit::*;

/// Jellyfin-ish accent blue used for primary actions and selection.
pub(crate) fn accent() -> Hsla {
    hsla(197.0 / 360.0, 0.86, 0.44, 1.0)
}

/// Accent blue at a custom alpha, for tinted backgrounds.
pub(crate) fn accent_soft(alpha: f32) -> Hsla {
    hsla(197.0 / 360.0, 0.86, 0.44, alpha)
}

/// Purple used for the "locked" state.
pub(crate) fn purple() -> Hsla {
    hsla(267.0 / 360.0, 0.85, 0.72, 1.0)
}

pub(crate) fn green(theme: &Theme) -> Hsla {
    theme.success
}

pub(crate) fn amber(theme: &Theme) -> Hsla {
    theme.warning
}

pub(crate) fn red(theme: &Theme) -> Hsla {
    theme.danger
}

pub(crate) fn muted(theme: &Theme) -> Hsla {
    theme.muted_foreground
}

pub(crate) fn panel(theme: &Theme) -> Hsla {
    theme.group_box
}

pub(crate) fn panel2(theme: &Theme) -> Hsla {
    theme.muted
}

pub(crate) fn border(theme: &Theme) -> Hsla {
    theme.border
}

/// One-line progress summary for a running library sync.
pub(crate) fn sync_progress_text(progress: &::core::SyncProgress) -> String {
    if progress.libraries_total > 0 {
        format!(
            "同步中 {}/{} 个媒体库 · {} 个条目 · {}",
            progress.libraries_done, progress.libraries_total, progress.items, progress.message
        )
    } else if !progress.message.is_empty() {
        progress.message.clone()
    } else {
        format!("同步中 · {} 个条目", progress.items)
    }
}

pub(crate) fn transparent() -> Hsla {
    hsla(0.0, 0.0, 0.0, 0.0)
}

/// Render an anyhow error including its source chain.
pub(crate) fn error_text(error: anyhow::Error) -> String {
    format!("{error:#}")
}

/// Apply the prototype's dark palette on top of the stock dark theme.
pub(crate) fn apply_theme(cx: &mut App) {
    use gpui_kit::component::{Theme, ThemeMode};

    Theme::change(ThemeMode::Dark, None, cx);
    Theme::update(cx, |theme| {
        theme.background = rgb(0x0d0f14).into();
        theme.foreground = rgb(0xe8ecf4).into();
        theme.border = rgb(0x262c3a).into();
        theme.group_box = rgb(0x141821).into();
        theme.muted = rgb(0x1a1f2b).into();
        theme.muted_foreground = rgb(0x8b93a7).into();
        theme.input = rgb(0x262c3a).into();
        theme.primary = rgb(0x00a4dc).into();
        theme.primary_foreground = rgb(0x04121a).into();
        theme.primary_hover = rgb(0x19b5ea).into();
        theme.primary_active = rgb(0x0091c4).into();
        theme.secondary = rgb(0x1a1f2b).into();
        theme.secondary_foreground = rgb(0xe8ecf4).into();
        theme.sidebar = rgb(0x141821).into();
        theme.sidebar_foreground = rgb(0x8b93a7).into();
        theme.sidebar_border = rgb(0x262c3a).into();
        theme.sidebar_accent = rgb(0x1a1f2b).into();
        theme.sidebar_accent_foreground = rgb(0xe8ecf4).into();
        theme.popover = rgb(0x141821).into();
        theme.popover_foreground = rgb(0xe8ecf4).into();
        theme.success = rgb(0x3ecf8e).into();
        theme.warning = rgb(0xf5b544).into();
        theme.danger = rgb(0xff6b6b).into();
        theme.table = rgb(0x141821).into();
        theme.table_head = rgb(0x1a1f2b).into();
        theme.table_row_border = rgb(0x262c3a).into();
        theme.table_hover = rgb(0x1a1f2b).into();
        theme.tab = rgb(0x141821).into();
        theme.tab_bar = rgb(0x141821).into();
        theme.switch = rgb(0x1a1f2b).into();
        theme.ring = rgb(0x00a4dc).into();
    });
}

/// Chinese label for an item's state, locked takes precedence.
pub(crate) fn status_text(status: TranslationStatus, locked: bool) -> &'static str {
    if locked {
        "已锁定"
    } else {
        match status {
            TranslationStatus::Untranslated => "未翻译",
            TranslationStatus::Draft => "草稿",
            TranslationStatus::Written => "已写回",
        }
    }
}

/// Small pill rendered on top of a poster.
pub(crate) fn status_badge(status: TranslationStatus, locked: bool, theme: &Theme) -> Div {
    let (background, foreground) = if locked {
        (purple(), hsla(0.0, 0.0, 1.0, 1.0))
    } else {
        match status {
            TranslationStatus::Untranslated => (
                hsla(220.0 / 360.0, 0.24, 0.13, 0.75),
                hsla(220.0 / 360.0, 0.25, 0.90, 1.0),
            ),
            TranslationStatus::Draft => (amber(theme), rgb(0x241a05).into()),
            TranslationStatus::Written => (green(theme), rgb(0x06251a).into()),
        }
    };

    div()
        .absolute()
        .top(px(8.))
        .left(px(8.))
        .px(px(7.))
        .py(px(2.))
        .rounded_full()
        .bg(background)
        .text_color(foreground)
        .text_size(px(10.5))
        .child(status_text(status, locked))
}

/// One fixed poster gradient, keyed by a stable hash of the item id.
pub(crate) fn poster_background(id: &str) -> Background {
    const PALETTE: [(f32, f32, f32, f32, f32, f32); 6] = [
        // hue, saturation, lightness for the two gradient stops
        (203.0, 0.52, 0.24, 287.0, 0.42, 0.36),
        (28.0, 0.44, 0.25, 17.0, 0.55, 0.38),
        (196.0, 0.48, 0.22, 160.0, 0.42, 0.33),
        (255.0, 0.40, 0.25, 266.0, 0.50, 0.36),
        (338.0, 0.48, 0.24, 329.0, 0.48, 0.37),
        (145.0, 0.42, 0.22, 100.0, 0.38, 0.34),
    ];

    let (h1, s1, l1, h2, s2, l2) = PALETTE[hash_bucket(id, PALETTE.len() as u64) as usize];
    linear_gradient(
        150.0,
        linear_color_stop(hsla(h1 / 360.0, s1, l1, 1.0), 0.0),
        linear_color_stop(hsla(h2 / 360.0, s2, l2, 1.0), 1.0),
    )
}

/// Accent gradient for the brand mark and avatars.
pub(crate) fn brand_background() -> Background {
    linear_gradient(
        135.0,
        linear_color_stop(accent(), 0.0),
        linear_color_stop(purple(), 1.0),
    )
}

fn hash_bucket(value: &str, buckets: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish() % buckets.max(1)
}

/// A muted uppercase-ish section heading used inside settings cards.
pub(crate) fn field_label(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .text_size(px(12.))
        .text_color(muted(theme))
        .child(text.into())
}

/// Page header: title plus one-line description.
pub(crate) fn page_head(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    theme: &Theme,
) -> Div {
    v_flex()
        .gap_1()
        .child(
            div()
                .text_size(px(20.))
                .font_semibold()
                .text_color(theme.foreground)
                .child(title.into()),
        )
        .child(
            div()
                .text_size(px(12.5))
                .text_color(muted(theme))
                .child(description.into()),
        )
}

/// A card surface used by every page.
pub(crate) fn card(theme: &Theme) -> Div {
    div()
        .w_full()
        .rounded(px(12.))
        .border_1()
        .border_color(border(theme))
        .bg(panel(theme))
}

/// A read-only key/value row used in the details drawer.
pub(crate) fn kv_row(key: impl Into<SharedString>, value: impl IntoElement, theme: &Theme) -> Div {
    h_flex()
        .w_full()
        .items_start()
        .gap_3()
        .py(px(9.))
        .border_b_1()
        .border_color(border(theme))
        .child(
            div()
                .w(px(72.))
                .flex_shrink_0()
                .text_size(px(12.5))
                .text_color(muted(theme))
                .child(key.into()),
        )
        .child(div().flex_1().min_w_0().text_size(px(12.5)).child(value))
}
