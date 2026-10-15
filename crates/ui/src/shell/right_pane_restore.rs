//! Restart restoration of each chat's right sidebar
//! ([`UiSettings::right_panes`]): whether it showed, the place it showed, and
//! the Files place's browser and file tabs. A chat's saved pane applies the
//! first time the chat shows in an app run; from then on its live pane is
//! written back whenever it changes.

use super::*;
use crate::settings::{RIGHT_PANES_CAP, SavedFileTab, SavedRightPane};

impl Shell {
    /// Reopen the shown chat's right sidebar as this device last left it.
    /// Waits for the engine and the chat's row, so the restored Files
    /// surfaces resolve their checkout rather than report a service that is
    /// still starting. A pane the user already changed in this run wins.
    pub(super) fn restore_right_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chat_id = self.active_chat.clone();
        if chat_id.is_empty() || self.right_pane_restored.contains(&chat_id) {
            return;
        }
        {
            let state = self.state.read(cx);
            if state.engine().is_none() || !state.chats.iter().any(|chat| chat.id == chat_id) {
                return;
            }
        }
        self.right_pane_restored.insert(chat_id.clone());
        let live = self.panels.get(&chat_id);
        let touched = live.changes_open
            || live.right_active != RightSurface::default()
            || self
                .right_tabs
                .get(&chat_id)
                .is_some_and(|tabs| !tabs.is_empty());
        if touched {
            return;
        }
        let Some(saved) = self
            .settings
            .right_panes
            .iter()
            .find(|pane| pane.chat_id == chat_id)
            .cloned()
        else {
            return;
        };
        let mut files_shown = None;
        let mut first_files_tab = None;
        for tab in saved.files {
            let surface = if tab.browser {
                self.ensure_files_place(&chat_id, tab.path, window, cx);
                RightSurface::Files
            } else {
                let Some(path) = tab.path else {
                    continue;
                };
                let owner = tab.owner.unwrap_or_else(|| chat_id.clone());
                // Another chat's file needs that chat's checkout.
                if !self
                    .state
                    .read(cx)
                    .chats
                    .iter()
                    .any(|chat| chat.id == owner)
                {
                    continue;
                }
                RightSurface::File(self.register_file_surface(
                    chat_id.clone(),
                    owner,
                    path,
                    window,
                    cx,
                ))
            };
            // Load it now, as every open tab had in the last run: a loaded
            // tab watches its checkout, so it follows a rename even before
            // it shows again.
            let files = match surface {
                RightSurface::Files => self.files.get(&chat_id),
                RightSurface::File(id) => self.file_surfaces.get(&id),
                _ => None,
            };
            if let Some(files) = files.cloned() {
                files.update(cx, |files, cx| files.ensure_loaded(cx));
            }
            first_files_tab.get_or_insert(surface);
            if tab.active {
                files_shown = Some(surface);
            }
        }
        let files_shown = files_shown.or(first_files_tab);
        if let Some(surface) = files_shown {
            self.right_place_memory
                .entry(chat_id.clone())
                .or_default()
                .insert(RightPlace::Files, surface);
        }
        let place = saved
            .place
            .as_deref()
            .and_then(RightPlace::from_key)
            .unwrap_or(RightPlace::Overview);
        let shown = match place {
            RightPlace::Files => files_shown.unwrap_or(RightSurface::Home(place)),
            // Only Files tabs are rebuilt; another place reopens on its page.
            place => RightSurface::Home(place),
        };
        // Snap, no tween: like a chat switch, the pane is the chat's own.
        self.panels.update(&chat_id, |panel| {
            panel.changes_open = saved.open;
            panel.right_active = shown;
        });
        cx.notify();
    }

    /// Write the shown chat's live right sidebar back to the settings when
    /// it changed: most recently changed first, a default pane forgotten,
    /// the list capped.
    pub(super) fn save_right_pane(&mut self, cx: &mut Context<Self>) {
        let chat_id = self.active_chat.clone();
        if chat_id.is_empty() || !self.right_pane_restored.contains(&chat_id) {
            return;
        }
        let live = self.right_pane_snapshot(&chat_id, cx);
        let at = self
            .settings
            .right_panes
            .iter()
            .position(|pane| pane.chat_id == chat_id);
        let forgotten = SavedRightPane {
            chat_id,
            ..SavedRightPane::default()
        };
        let saved = at.map(|at| &self.settings.right_panes[at]);
        if saved.unwrap_or(&forgotten) == &live {
            return;
        }
        if let Some(at) = at {
            self.settings.right_panes.remove(at);
        }
        if live != forgotten {
            self.settings.right_panes.insert(0, live);
            self.settings.right_panes.truncate(RIGHT_PANES_CAP);
        }
        self.schedule_save(cx);
    }

    /// A file of `panel_key`'s Files place was renamed on disk: the browser's
    /// open file (`owner` `None`) or `owner`'s file tab. A chat's live pane is
    /// written back only while it shows, so a chat in the background has its
    /// saved tab follow the rename here; otherwise a restart would reopen the
    /// old path.
    pub(super) fn follow_saved_rename(
        &mut self,
        panel_key: &str,
        owner: Option<&str>,
        old_path: &str,
        new_path: &str,
        cx: &mut Context<Self>,
    ) {
        if panel_key == self.active_chat {
            return;
        }
        let Some(pane) = self
            .settings
            .right_panes
            .iter_mut()
            .find(|pane| pane.chat_id == panel_key)
        else {
            return;
        };
        let mut followed = false;
        for tab in &mut pane.files {
            let same_tab = match owner {
                None => tab.browser,
                Some(owner) => !tab.browser && tab.owner.as_deref().unwrap_or(panel_key) == owner,
            };
            if same_tab && tab.path.as_deref() == Some(old_path) {
                tab.path = Some(new_path.to_owned());
                followed = true;
            }
        }
        if followed {
            self.schedule_save(cx);
        }
    }

    /// `chat_id`'s right sidebar as it shows now. `chat_id` is the shown
    /// chat, whose panel key it is.
    fn right_pane_snapshot(&self, chat_id: &str, cx: &App) -> SavedRightPane {
        let shown = self.resolved_right_active(cx);
        let place = RightPlace::of(shown);
        let files_shown = if place == RightPlace::Files {
            Some(shown)
        } else {
            self.right_place_memory
                .get(chat_id)
                .and_then(|memory| memory.get(&RightPlace::Files))
                .copied()
        };
        let files = self
            .right_tabs
            .get(chat_id)
            .into_iter()
            .flatten()
            .filter_map(|surface| {
                let tab = match *surface {
                    RightSurface::Files => SavedFileTab {
                        browser: true,
                        path: self
                            .files
                            .get(chat_id)?
                            .read(cx)
                            .attachment_path()
                            .map(str::to_owned),
                        ..SavedFileTab::default()
                    },
                    RightSurface::File(id) => {
                        let (_, owner) = self.file_surface_owners.get(&id)?;
                        SavedFileTab {
                            path: Some(self.file_surface_paths.get(&id)?.clone()),
                            owner: (owner != chat_id).then(|| owner.clone()),
                            ..SavedFileTab::default()
                        }
                    }
                    _ => return None,
                };
                Some(SavedFileTab {
                    active: files_shown == Some(*surface),
                    ..tab
                })
            })
            .collect();
        SavedRightPane {
            chat_id: chat_id.to_owned(),
            open: self.panels.get(chat_id).changes_open,
            place: match place {
                // A contributed panel's handle is not an identity; it
                // reopens on the Overview, its known-good fallback.
                RightPlace::Overview | RightPlace::Contributed(_) => None,
                place => Some(place.key().to_owned()),
            },
            files,
        }
    }
}
