//! Keeps the shown theme in step with the saved preference. For System it
//! re-resolves whenever the window appearance changes or Omarchy switches
//! theme, so it follows the desktop rather than a snapshot taken at start.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{AppContext as _, Context, SharedString, Subscription, Task, Window};

use super::{ThemePreference, ThemeTarget};
use crate::services::Services;

/// Omarchy writes many files per switch; apply once they settle.
const OMARCHY_SETTLE_DELAY: Duration = Duration::from_millis(400);

pub struct ThemeController {
    preference: ThemePreference,
    save_error: Option<SharedString>,
    is_saving: bool,
    has_changed_preference: bool,
    /// A custom Omarchy theme directory from the settings, if any.
    omarchy_directory: Option<PathBuf>,
    /// Present while System is chosen.
    omarchy_watch: Option<(notify::RecommendedWatcher, Task<()>)>,
    _appearance: Subscription,
}

impl ThemeController {
    /// Loads the saved preference and shows it.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            if this.preference == ThemePreference::System {
                this.show(window, cx);
            }
        });

        let controller = Self {
            preference: ThemePreference::System,
            save_error: None,
            is_saving: false,
            has_changed_preference: false,
            omarchy_directory: None,
            omarchy_watch: None,
            _appearance: appearance,
        };

        controller.load(window, cx);
        controller
    }

    pub fn preference(&self) -> ThemePreference {
        self.preference
    }

    pub fn save_error(&self) -> Option<&SharedString> {
        self.save_error.as_ref()
    }

    /// Shows `preference` and saves it.
    pub fn set_preference(
        &mut self,
        preference: ThemePreference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.preference = preference;
        self.has_changed_preference = true;
        self.show(window, cx);
        cx.notify();

        self.save_preference(window, cx);
    }

    fn save_preference(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_saving {
            return;
        }
        self.is_saving = true;
        self.save_error = None;
        let preference = self.preference;

        let local_state = Arc::clone(&Services::app(cx).local_state);
        let save = cx.background_spawn(async move {
            local_state.set_setting(super::THEME_SETTING, &preference)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = save.await;
            this.update_in(cx, |this, window, cx| {
                this.is_saving = false;
                if preference != this.preference {
                    this.save_preference(window, cx);
                    return;
                }
                if let Err(error) = result {
                    let message: SharedString =
                        format!("The theme is applied, but could not be saved: {error}").into();
                    this.save_error = Some(message.clone());
                    window.push_notification(Notification::error(message), cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn load(&self, window: &mut Window, cx: &mut Context<Self>) {
        let local_state = Arc::clone(&Services::app(cx).local_state);
        let load = cx.background_spawn(async move {
            // An unknown or unreadable value falls back to System.
            let preference: ThemePreference = local_state
                .setting(super::THEME_SETTING)
                .ok()
                .flatten()
                .unwrap_or_default();
            let omarchy_directory: Option<PathBuf> = local_state
                .setting(super::OMARCHY_DIRECTORY_SETTING)
                .ok()
                .flatten();
            (preference, omarchy_directory)
        });

        cx.spawn_in(window, async move |this, cx| {
            let (preference, omarchy_directory) = load.await;

            this.update_in(cx, |this, window, cx| {
                if !this.has_changed_preference {
                    this.preference = preference;
                }
                this.omarchy_directory = omarchy_directory;
                // A watch started before loading may be on the wrong directory.
                this.omarchy_watch = None;
                this.show(window, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Applies the current preference. System reads the Omarchy palette off
    /// the UI thread and watches it for changes.
    fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(name) = self.preference.bundled_theme() {
            self.omarchy_watch = None;
            super::apply(&ThemeTarget::Bundled(name), cx);
            return;
        }

        self.watch_omarchy(window, cx);

        let omarchy_directory = self.omarchy_directory.clone();
        let read =
            cx.background_spawn(async move { super::read_omarchy_palette(omarchy_directory) });

        cx.spawn_in(window, async move |this, cx| {
            let palette = read.await;

            this.update_in(cx, |this, window, cx| {
                // The preference may have changed while the palette loaded.
                if this.preference != ThemePreference::System {
                    return;
                }

                let mode = super::appearance_mode(window.appearance());
                let resolution = super::resolve_system(mode, palette);
                super::apply(&resolution.target, cx);

                if let Some(reason) = resolution.warning {
                    window.push_notification(
                        Notification::warning(format!(
                            "Omarchy palette unavailable, using the system appearance. {reason}"
                        )),
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-applies System when Omarchy switches theme. Without Omarchy there
    /// is nothing to watch and System follows the appearance alone.
    fn watch_omarchy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.omarchy_watch.is_some() {
            return;
        }

        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<()>();
        let watcher = super::watch_omarchy(self.omarchy_directory.clone(), move || {
            // The receiver lives as long as the watch; nothing to do if gone.
            let _ = sender.send(());
        });

        let Ok(watcher) = watcher else {
            return;
        };

        let task = cx.spawn_in(window, async move |this, cx| {
            while receiver.recv().await.is_some() {
                cx.background_executor().timer(OMARCHY_SETTLE_DELAY).await;
                while receiver.try_recv().is_ok() {}

                let shown = this.update_in(cx, |this, window, cx| {
                    if this.preference == ThemePreference::System {
                        this.show(window, cx);
                    }
                });

                if shown.is_err() {
                    break;
                }
            }
        });

        self.omarchy_watch = Some((watcher, task));
    }
}
