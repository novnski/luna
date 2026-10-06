//! Embedded icon assets + the gpui [`AssetSource`] that serves them.
//!
//! The set mirrors the original luna's icon usage exactly:
//! - Most glyphs come from the **Solar Icons** set (Linear weight) by 480 Design,
//!   the same set the Electron app used via `@solar-icons/react`. Solar Icons is
//!   licensed under CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/);
//!   attribution: "Solar Icons by 480 Design".
//! - Every interface glyph is official Solar Linear, fetched with
//!   `@solar-icons/cli` (UI polish audit DS-09). The exceptions: the filled
//!   stop square (a state glyph), the window controls (they mirror the OS),
//!   the return-key glyph, and the queue's own line family.
//! - The harness brand marks (`claude-mark`, `openai-mark`, `opencode-mark`) are
//!   ports of luna's `icons.tsx`. gpui tints SVGs with the text color, so the
//!   Claude mark's brand orange is applied at the call site ([`CLAUDE_BRAND`]).
//!
//! Icons render via [`icon`]: `icon(icons::PAPERCLIP).size(px(16.)).text_color(…)`.

use std::borrow::Cow;

use gpui::{AssetSource, Hsla, Result, SharedString, Styled as _, Svg, svg};

/// Icon size ladder (UI polish audit DS-12), paired with the type scale.
pub mod size {
    /// Filled status glyphs beside caption text only.
    pub const XS: f32 = 12.0;
    /// Toolbars, chips, menus (caption / footnote text).
    pub const SM: f32 = 14.0;
    /// Buttons, settings tiles, rows (body text).
    pub const MD: f32 = 16.0;
    /// Titlebar, empty states (title text and up).
    pub const LG: f32 = 20.0;
}

macro_rules! icon_assets {
    ($(($const_name:ident, $path:literal)),+ $(,)?) => {
        $(pub const $const_name: &str = concat!("icons/", $path, ".svg");)+

        /// Serves the embedded control icons to gpui's SVG renderer.
        struct ControlAssets;

        impl AssetSource for ControlAssets {
            fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
                Ok(match path {
                    $(concat!("icons/", $path, ".svg") => Some(Cow::Borrowed(
                        include_bytes!(concat!("../assets/icons/", $path, ".svg")).as_slice(),
                    )),)+
                    _ => None,
                })
            }

            fn list(&self, path: &str) -> Result<Vec<SharedString>> {
                let all = [$(concat!("icons/", $path, ".svg")),+];
                Ok(all
                    .iter()
                    .filter(|p| p.starts_with(path))
                    .map(|p| SharedString::from(*p))
                    .collect())
            }
        }
    };
}

icon_assets![
    // Solar Icons (Linear), CC BY 4.0 — 480 Design.
    (MONITOR, "monitor"),
    (SUN, "sun"),
    (MOON, "moon"),
    // Browser globe: Solar `global` (the catalog's `globe` is a desk globe).
    (GLOBE, "globe"),
    (LAPTOP, "laptop"),
    (PEN_NEW_SQUARE, "pen-new-square"),
    (SORT, "sort"),
    (SORT_VERTICAL, "sort-vertical"),
    // Compact six-dot grip used to reorder queued prompts.
    (DRAG_HANDLE, "drag-handle"),
    // Original queue-only line family: 24px canvas, 1.5px round strokes and
    // medium-radius geometry. Kept separate so the queue can adopt the visual
    // language of the supplied Central Icons reference without changing
    // shared application glyphs or copying third-party artwork.
    (QUEUE_DRAG_HANDLE, "queue-drag-handle"),
    (QUEUE_SEND, "queue-send"),
    (QUEUE_CHECK, "queue-check"),
    (QUEUE_CLOSE, "queue-close"),
    (QUEUE_PAPERCLIP, "queue-paperclip"),
    (CLOCK_CIRCLE, "clock-circle"),
    // Goal card glyphs, official Solar Linear (24px canvas,
    // 1.5px strokes): the goal target, token usage, pause and resume.
    (TARGET, "target"),
    (BOLT, "bolt"),
    (PAUSE, "pause"),
    (PLAY, "play"),
    (CALENDAR, "calendar"),
    (LIST, "list"),
    (CHART, "chart"),
    (MORE_HORIZONTAL, "more-horizontal"),
    (FOLDER_WITH_FILES, "folder-with-files"),
    (FOLDER, "folder"),
    // Solar Linear composer chips: slash commands and generic file mentions.
    (MAGIC_WAND_3, "magic-wand-3"),
    (FILE, "file"),
    // Solar `diskette`: workspace editor save.
    (FLOPPY_DISK, "floppy-disk"),
    // Solar `git-branch`.
    (GIT_BRANCH, "git-branch"),
    // Compact task pin for the sidebar's pinned rows and hover action.
    (PIN, "pin"),
    // Provider-neutral pull-request glyph, drawn in the same linear family.
    (PULL_REQUEST, "pull-request"),
    // Compact history-ref glyphs, drawn in the same linear style.
    (CLOUD, "cloud"),
    (TAG, "tag"),
    (SIDEBAR_MINIMALISTIC, "sidebar-minimalistic"),
    // Mirrored variant (luna window-controls.tsx `-scale-x-100`): the LEFT
    // sidebar toggle shows the panel line on the left; gpui divs have no
    // scale transform at the pinned rev, so the flip is baked into the asset.
    (SIDEBAR_MINIMALISTIC_LEFT, "sidebar-minimalistic-left"),
    (KEY_MINIMALISTIC, "key-minimalistic"),
    // Solar Linear access levels for the composer's icon-only access chip:
    // limited (locked), approve for me (shield check), full access
    // (unlocked) and a choice still to make (shield warning).
    (LOCK_KEYHOLE, "lock-keyhole-minimalistic"),
    (LOCK_KEYHOLE_UNLOCKED, "lock-keyhole-minimalistic-unlocked"),
    (SHIELD_CHECK, "shield-check"),
    (SHIELD_WARNING, "shield-warning"),
    // Solar Linear bolt: the model chip's fast-speed mark.
    (BOLT_FAST, "bolt-fast"),
    (KEYBOARD, "keyboard"),
    (ARROW_LEFT, "arrow-left"),
    (ARROW_RIGHT, "arrow-right"),
    (ARROW_UP, "arrow-up"),
    // arrow-up mirrored (like the sidebar flip) — the Solar Linear set here
    // has no plain arrow-down.
    (ARROW_DOWN, "arrow-down"),
    // arrow-up rotated 45° — the "opens elsewhere" glyph on spawn chips;
    // the set has no diagonal arrow.
    (ARROW_UP_RIGHT, "arrow-up-right"),
    // Hand-drawn return/enter arrow in the Solar Linear style (like the
    // terminal/plus/close ports) — the set has no return glyph.
    (RETURN, "return"),
    (ALT_ARROW_DOWN, "alt-arrow-down"),
    // alt-arrow-down mirrored (drawn as its twin: same stroke, caps, joints) —
    // the embedded Solar Linear set ships no up chevron. The collapse half of
    // the long-user-message expander.
    (ALT_ARROW_UP, "alt-arrow-up"),
    // Hand-drawn expand/maximize arrows in the Solar Linear style (like the
    // terminal/plus/return ports) — the set has no expand glyph.
    (EXPAND_ARROWS, "expand-arrows"),
    // Inward-pointing companion used to restore an expanded pane.
    (COLLAPSE_ARROWS, "collapse-arrows"),
    // Hand-drawn fold-all chevrons, drawn as a family with EXPAND_ARROWS
    // (same stroke, caps, 90° joints) — Solar has no unfold-less either.
    (FOLD_VERTICAL, "fold-vertical"),
    // The changes pane's unified/split toggle: a rounded frame halved by a
    // centre rule (Solar Linear weight).
    (SPLIT_COLUMNS, "split-columns"),
    // Long-line wrapping toggle shared by changes and agent Markdown fences.
    (WRAP_TEXT, "wrap-text"),
    (ALT_ARROW_LEFT, "alt-arrow-left"),
    (ALT_ARROW_RIGHT, "alt-arrow-right"),
    (SMARTPHONE, "smartphone"),
    (ARCHIVE_UP_MINIMALISTIC, "archive-up-minimalistic"),
    (REFRESH, "refresh"),
    (RESTART, "restart"),
    (ADD_CIRCLE, "add-circle"),
    (TUNING, "tuning"),
    (EYE, "eye"),
    (EYE_CLOSED, "eye-closed"),
    (PAPERCLIP, "paperclip"),
    (PEN, "pen"),
    (ARCHIVE_MINIMALISTIC, "archive-minimalistic"),
    (TRASH_BIN_MINIMALISTIC, "trash-bin-minimalistic"),
    // Shared settings glyph: user-supplied horizontal sliders.
    (SETTINGS_MINIMALISTIC, "settings-minimalistic"),
    (SETTINGS_GEAR, "settings-gear"),
    (LOGOUT_2, "logout-2"),
    (MAGNIFER, "magnifer"),
    // Solar `magnifier`.
    (PALETTE_SEARCH, "palette-search"),
    (COMMAND, "command"),
    (DOCUMENT, "document"),
    (DOCUMENT_ADD, "document-add"),
    // File-kind glyphs, drawn in the same linear family for transcript badges.
    (FILE_CODE, "file-code"),
    (FILE_STYLE, "file-style"),
    (FILE_DATA, "file-data"),
    (FILE_MARKDOWN, "file-markdown"),
    (FILE_IMAGE, "file-image"),
    (GLOBAL, "global"),
    (CHECKLIST, "checklist"),
    (WIDGET, "widget"),
    // Solar `palette`: the accent colour setting (audit DS-21).
    (PALETTE, "palette"),
    // Solar Linear via @solar-icons/cli (audit TL-16): MCP tools and thoughts.
    (PLUG_CIRCLE, "plug-circle"),
    (LIGHTBULB, "lightbulb-minimalistic"),
    (WIFI_OFF, "wifi-off"),
    (CLOSE_CIRCLE, "close-circle"),
    // Solar `info-circle`.
    (INFO_CIRCLE, "info-circle"),
    (DANGER_TRIANGLE, "danger-triangle"),
    (CHAT_ROUND_LINE, "chat-round-line"),
    // Related chats: the surface, and the side-chat (user fork) kind mark.
    (DIALOG, "dialog"),
    (GIT_FORK, "git-fork"),
    // Solar `bot`: subagent tabs.
    (BOT, "bot"),
    // Solar `bell` and `volume-loud`.
    (BELL, "bell"),
    (VOLUME_LOUD, "volume-loud"),
    // Solar `programming` (terminal), `add` (plus) and `close`.
    (TERMINAL, "terminal"),
    (PLUS, "plus"),
    (CLOSE, "close"),
    // Hand-drawn Linux caption glyphs (minimize dash, maximize square,
    // restore stacked squares) in the same style as `close` — drawn for the
    // client-side-decoration window controls; no system glyph font exists on
    // Linux the way Segoe Fluent Icons does on Windows.
    (WINDOW_MINIMIZE, "window-minimize"),
    (WINDOW_MAXIMIZE, "window-maximize"),
    (WINDOW_RESTORE, "window-restore"),
    // The add-space palette's Locations rail: a drawn hard drive and Solar
    // `home`.
    (HARD_DRIVE, "hard-drive"),
    (HOME, "home"),
    (STOP, "stop"),
    (CHECK, "check"),
    (COPY, "copy"),
    // Solar `star` for the favorite affordance; the filled twin marks the
    // favorited state and the picker's favorites rail tab.
    (STAR, "star"),
    (STAR_BOLD, "star-bold"),
    (LUNA_LOGO, "luna-logo"),
    // Harness brand marks (icons.tsx).
    (CLAUDE_MARK, "claude-mark"),
    (OPENAI_MARK, "openai-mark"),
    (OPENCODE_MARK, "opencode-mark"),
];

/// Serves both the compact control-icon set and the complete file-identity
/// icon theme through the single asset source registered at app startup.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(asset) = ControlAssets.load(path)? {
            return Ok(Some(asset));
        }
        crate::file_icons::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut assets = ControlAssets.list(path)?;
        assets.extend(crate::file_icons::Assets.list(path)?);
        Ok(assets)
    }
}

/// The Claude mark's brand orange (`#D97757`) — luna keeps it even on the
/// monochrome surface.
pub fn claude_brand() -> Hsla {
    gpui::rgb(0xD97757).into()
}

/// An icon element for an embedded asset path. Size and colour are set by the
/// caller (`.size(..)`, `.text_color(..)`), matching the web app's
/// `[&_svg]:size-4` idiom.
pub fn icon(path: &'static str) -> Svg {
    svg().path(path).flex_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_icon_loads_and_parses() {
        let assets = Assets;
        for path in assets.list("icons/").unwrap() {
            let bytes = assets
                .load(&path)
                .unwrap()
                .unwrap_or_else(|| panic!("missing asset {path}"));
            let text = std::str::from_utf8(&bytes).expect("icon svg is utf-8");
            assert!(text.contains("<svg"), "{path} is not an svg");
            assert!(text.contains("viewBox"), "{path} lacks a viewBox");
        }
    }

    #[test]
    fn unknown_paths_are_none() {
        assert!(Assets.load("icons/nope.svg").unwrap().is_none());
    }

    #[test]
    fn list_filters_by_prefix() {
        assert!(!Assets.list("icons/").unwrap().is_empty());
        assert!(Assets.list("fonts/").unwrap().is_empty());
    }
}
