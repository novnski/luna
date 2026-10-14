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
//!   the return-key glyph, the queue's own line family, and the few glyphs
//!   Solar lacks, taken from Lucide, Tabler and Simple Icons with strokes set
//!   to 1.5px (`assets/icons/licenses/THIRD_PARTY_NOTICES.md`).
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
    // 1.5px strokes): the goal target, pause and resume.
    (TARGET, "target"),
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
    // Solar `git-diff`: the Diffs surface and the apply-patch tool row.
    (GIT_DIFF, "git-diff"),
    // Compact task pin for the sidebar's pinned rows and hover action.
    (PIN, "pin"),
    // Lucide `git-pull-request` (Solar has none), stroke set to 1.5px.
    (PULL_REQUEST, "git-pull-request"),
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
    (ARROW_DOWN, "arrow-down"),
    // Solar `arrow-right-up`: the "opens elsewhere" glyph on spawn chips.
    (ARROW_UP_RIGHT, "arrow-right-up"),
    // Hand-drawn return/enter arrow in the Solar Linear style (like the
    // terminal/plus/close ports) — the set has no return glyph.
    (RETURN, "return"),
    (ALT_ARROW_DOWN, "alt-arrow-down"),
    // The collapse half of the long-user-message expander.
    (ALT_ARROW_UP, "alt-arrow-up"),
    // Solar `maximize` / `minimize`: expand a pane, and restore it.
    (EXPAND_ARROWS, "maximize"),
    (COLLAPSE_ARROWS, "minimize"),
    // Hand-drawn fold-all chevrons (same stroke, caps, 90° joints as the
    // Solar set) — Solar has no unfold-less glyph.
    (FOLD_VERTICAL, "fold-vertical"),
    // Solar `columns-2`: the changes pane's unified/split toggle.
    (SPLIT_COLUMNS, "columns-2"),
    // Long-line wrapping toggle shared by changes and agent Markdown fences.
    (WRAP_TEXT, "wrap-text"),
    (ALT_ARROW_LEFT, "alt-arrow-left"),
    (ALT_ARROW_RIGHT, "alt-arrow-right"),
    (SMARTPHONE, "smartphone"),
    (ARCHIVE_UP_MINIMALISTIC, "archive-up-minimalistic"),
    (REFRESH, "refresh"),
    (RESTART, "restart"),
    (ADD_CIRCLE, "add-circle"),
    (EYE, "eye"),
    (EYE_CLOSED, "eye-closed"),
    (PAPERCLIP, "paperclip"),
    (PEN, "pen"),
    (ARCHIVE_MINIMALISTIC, "archive-minimalistic"),
    // Settings sections: General, Appearance, Providers, Backups, Appshots
    // and Updates.
    (SETTINGS, "settings"),
    (PAINT_BRUSH, "paint-brush"),
    (SERVER_2, "server-2"),
    (DATABASE, "database"),
    (CAMERA_MINIMALISTIC, "camera-minimalistic"),
    (ROUND_ARROW_DOWN, "round-arrow-down"),
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
    // Activity chain: thoughts, mixed groups, failed rows and each tool kind
    // (Solar Linear). The MCP mark is Simple Icons' Model Context Protocol
    // logo; webhook and package-search are Lucide (Solar has neither).
    (BRAIN, "brain"),
    (LAYERS_MINIMALISTIC, "layers-minimalistic"),
    (TOOLBOX, "toolbox"),
    (DANGER_CIRCLE, "danger-circle"),
    (MCP_MARK, "mcp-mark"),
    (FILE_TEXT, "file-text"),
    (FILE_ADD, "file-add"),
    (FILE_PEN, "file-pen"),
    (FILE_SEARCH, "file-search"),
    (FOLDER_SEARCH, "folder-search"),
    (GALLERY, "gallery"),
    (WEBHOOK, "webhook"),
    (STOPWATCH, "stopwatch"),
    (CURSOR, "cursor"),
    (PACKAGE_SEARCH, "package-search"),
    (CHAT_ROUND_QUESTION_MARK, "chat-round-question-mark"),
    (CLIPBOARD_LIST, "clipboard-list"),
    // Board page (Solar Linear): the Board/List switch, filters, From
    // GitHub issue and the card key.
    (KANBAN, "kanban"),
    (FILTER, "filter"),
    (INBOX_IN, "inbox-in"),
    (TICKET, "ticket"),
    // Board identity on chat surfaces: Solar `ticket` (a ticket's working
    // chat) and `magnifier-bug` (its temporary reviewer side chat).
    (CHAT_TICKET, "ticket"),
    (CHAT_REVIEWER, "magnifier-bug"),
    (STOP_CIRCLE, "stop-circle"),
    (NOTEBOOK, "notebook"),
    (WIFI_OFF, "wifi-off"),
    (CLOSE_CIRCLE, "close-circle"),
    // Solar `info-circle`.
    (INFO_CIRCLE, "info-circle"),
    (DANGER_TRIANGLE, "danger-triangle"),
    (CHAT_ROUND_LINE, "chat-round-line"),
    // Solar `chat-line` (reply in a side chat) and `undo-left` (send a side
    // chat's result back to the main draft).
    (CHAT_LINE, "chat-line"),
    (UNDO_LEFT, "undo-left"),
    // Related chats: the surface (Solar `dialog-2`), and the fork mark for
    // forks and side chats (Tabler `arrow-fork`, stroke set to 1.5px).
    (DIALOG_2, "dialog-2"),
    (FORK, "arrow-fork"),
    // Solar `bot`: subagent tabs.
    (BOT, "bot"),
    // Solar `stopwatch-play`: automations.
    (STOPWATCH_PLAY, "stopwatch-play"),
    // Solar `window-frame`: the browser surface.
    (WINDOW_FRAME, "window-frame"),
    // Solar `bell` and `volume-loud`.
    (BELL, "bell"),
    (VOLUME_LOUD, "volume-loud"),
    // Solar `programming` (terminal), `add` (plus) and `close`.
    (TERMINAL, "terminal"),
    (PLUS, "plus"),
    (CLOSE, "close"),
    // Hand-drawn Linux caption glyphs (minimize dash, maximize square,
    // restore stacked squares) in the same style as `close` — drawn for the
    // client-side-decoration window controls; Linux has no system caption
    // glyph font.
    (WINDOW_MINIMIZE, "window-minimize"),
    (WINDOW_MAXIMIZE, "window-maximize"),
    (WINDOW_RESTORE, "window-restore"),
    // The add-space palette's Locations rail: Solar `ssd-square` and `home`.
    (HARD_DRIVE, "ssd-square"),
    (HOME, "home"),
    (STOP, "stop"),
    (CHECK, "check"),
    (COPY, "copy"),
    // Solar `copy-check`: a copy button's confirmation state.
    (COPY_CHECK, "copy-check"),
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
        let icons = assets.list("icons/").unwrap();
        assert!(!icons.is_empty());
        for path in icons {
            let bytes = assets
                .load(&path)
                .unwrap()
                .unwrap_or_else(|| panic!("missing asset {path}"));
            let text = std::str::from_utf8(&bytes).expect("icon svg is utf-8");
            assert!(text.contains("<svg"), "{path} is not an svg");
            assert!(text.contains("viewBox"), "{path} lacks a viewBox");
        }
        assert!(assets.load("icons/nope.svg").unwrap().is_none());
        assert!(assets.list("fonts/").unwrap().is_empty());
    }
}
