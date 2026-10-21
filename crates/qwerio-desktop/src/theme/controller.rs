//! Keeps the shown theme and frosted glass in step with the saved settings.
//! For System it re-resolves whenever the window appearance changes or
//! Omarchy switches theme, so it follows the desktop rather than a snapshot
//! taken at start.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{
    AppContext as _, Context, SharedString, Subscription, Task, Window, WindowBackgroundAppearance,
};

use super::glass::{
    FROSTED_GLASS_SETTING, FROSTED_GLASS_SHADE_SETTING, FrostedGlass, MAX_SHADE, MIN_SHADE,
};
use super::{ThemePreference, ThemeTarget};
use crate::services::Services;

/// Omarchy writes many files per switch; apply once they settle.
const OMARCHY_SETTLE_DELAY: Duration = Duration::from_millis(400);

/// What Settings → Appearance controls, saved under separate keys.
#[derive(Clone, Copy, PartialEq)]
struct Appearance {
    preference: ThemePreference,
    frosted_glass: FrostedGlass,
}

pub struct ThemeController {
    preference: ThemePreference,
    frosted_glass: FrostedGlass,
    save_error: Option<SharedString>,
    is_saving: bool,
    has_changed_preference: bool,
    has_changed_frosted_glass: bool,
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
            frosted_glass: FrostedGlass::default(),
            save_error: None,
            is_saving: false,
            has_changed_preference: false,
            has_changed_frosted_glass: false,
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

    pub fn frosted_glass(&self) -> FrostedGlass {
        self.frosted_glass
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

        self.save(window, cx);
    }

    /// Shows `frosted_glass` and saves it.
    pub fn set_frosted_glass(
        &mut self,
        frosted_glass: FrostedGlass,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.frosted_glass = FrostedGlass {
            shade: frosted_glass.shade.clamp(MIN_SHADE, MAX_SHADE),
            ..frosted_glass
        };
        self.has_changed_frosted_glass = true;
        self.show(window, cx);
        cx.notify();

        self.save(window, cx);
    }

    fn appearance(&self) -> Appearance {
        Appearance {
            preference: self.preference,
            frosted_glass: self.frosted_glass,
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_saving {
            return;
        }
        self.is_saving = true;
        self.save_error = None;
        let appearance = self.appearance();

        let local_state = Arc::clone(&Services::app(cx).local_state);
        let save = cx.background_spawn(async move {
            local_state.set_setting(super::THEME_SETTING, &appearance.preference)?;
            let frosted_glass = appearance.frosted_glass;
            local_state.set_setting(FROSTED_GLASS_SETTING, &frosted_glass.is_on)?;
            local_state.set_setting(FROSTED_GLASS_SHADE_SETTING, &frosted_glass.shade)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = save.await;
            this.update_in(cx, |this, window, cx| {
                this.is_saving = false;
                if appearance != this.appearance() {
                    this.save(window, cx);
                    return;
                }
                if let Err(error) = result {
                    let message: SharedString =
                        format!("The appearance is applied, but could not be saved: {error}")
                            .into();
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
            let default_glass = FrostedGlass::default();
            let frosted_glass = FrostedGlass {
                is_on: local_state
                    .setting(FROSTED_GLASS_SETTING)
                    .ok()
                    .flatten()
                    .unwrap_or(default_glass.is_on),
                shade: local_state
                    .setting::<f32>(FROSTED_GLASS_SHADE_SETTING)
                    .ok()
                    .flatten()
                    .map_or(default_glass.shade, |shade| {
                        shade.clamp(MIN_SHADE, MAX_SHADE)
                    }),
            };
            let omarchy_directory: Option<PathBuf> = local_state
                .setting(super::OMARCHY_DIRECTORY_SETTING)
                .ok()
                .flatten();
            (preference, frosted_glass, omarchy_directory)
        });

        cx.spawn_in(window, async move |this, cx| {
            let (preference, frosted_glass, omarchy_directory) = load.await;

            this.update_in(cx, |this, window, cx| {
                if !this.has_changed_preference {
                    this.preference = preference;
                }
                if !this.has_changed_frosted_glass {
                    this.frosted_glass = frosted_glass;
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

    /// Applies the current settings. System reads the Omarchy palette off
    /// the UI thread and watches it for changes.
    fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.set_background_appearance(if self.frosted_glass.is_on {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        });

        if let Some(name) = self.preference.bundled_theme() {
            self.omarchy_watch = None;
            super::apply(&ThemeTarget::Bundled(name), self.frosted_glass, cx);
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
                super::apply(&resolution.target, this.frosted_glass, cx);

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
