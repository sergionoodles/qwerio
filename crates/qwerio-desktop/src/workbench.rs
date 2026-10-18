//! The root view of each window: the welcome screen when no workspace is
//! open, otherwise the workspace shell. Owns window-wide commands such as
//! opening workspaces, theme selection, and text size.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, IconName, StyledExt as _, Theme, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _,
    Subscription, Task, Window, WindowBounds, WindowDecorations, WindowOptions, div, px, size,
};
use qwerio_app::local_state::RecentWorkspace;
use qwerio_workspace::Workspace;

use crate::actions::*;
use crate::platform::paths::APPLICATION_ID;
use crate::services::Services;
use crate::shell::WorkspaceShell;
use crate::theme::{self, Applied, ThemeSource};
use crate::workspace_model::WorkspaceModel;

const RECENT_LIMIT: u32 = 8;
const DEFAULT_FONT_SIZE: f32 = 14.0;
const MIN_FONT_SIZE: f32 = 10.0;
const MAX_FONT_SIZE: f32 = 24.0;
const FONT_SIZE_STEP: f32 = 1.0;
const FONT_SIZE_SETTING: &str = "font_size";
const OMARCHY_SETTLE_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

pub fn open_window(workspace: Option<PathBuf>, cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        app_id: Some(APPLICATION_ID.to_owned()),
        // Small enough for narrow tiled windows; panels collapse below it.
        window_min_size: Some(size(px(480.), px(360.))),
        // Let the compositor decide (Hyprland draws none), avoiding a second
        // title bar inside the window.
        window_decorations: Some(WindowDecorations::Server),
        ..WindowOptions::default()
    };

    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| Workbench::new(workspace, window, cx))
    });

    if let Err(error) = opened {
        eprintln!("Qwerio could not open a window: {error}");
    }
}

pub struct Workbench {
    focus_handle: FocusHandle,
    shell: Option<Entity<WorkspaceShell>>,
    recent: Vec<RecentWorkspace>,
    is_opening: bool,
    error: Option<SharedString>,
    theme_source: ThemeSource,
    omarchy_watch: Option<(notify::RecommendedWatcher, Task<()>)>,
    _appearance: Subscription,
}

impl Workbench {
    fn new(workspace: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance = cx.observe_window_appearance(window, |this, window, cx| {
            if this.theme_source == ThemeSource::System {
                this.apply_theme(ThemeSource::System, window, cx);
            }
        });

        let mut workbench = Self {
            focus_handle: cx.focus_handle(),
            shell: None,
            recent: Vec::new(),
            is_opening: false,
            error: None,
            theme_source: ThemeSource::System,
            omarchy_watch: None,
            _appearance: appearance,
        };

        workbench.load_preferences(window, cx);
        workbench.load_recent(cx);

        if let Some(path) = workspace {
            workbench.open_path(path, None, window, cx);
        }

        workbench
    }

    fn load_preferences(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let local_state = Arc::clone(&Services::app(cx).local_state);
        let load = cx.background_spawn(async move {
            let source: ThemeSource = local_state
                .setting(theme::THEME_SOURCE_SETTING)
                .ok()
                .flatten()
                .unwrap_or_default();
            let font_size: Option<f32> = local_state.setting(FONT_SIZE_SETTING).ok().flatten();
            (source, font_size)
        });

        cx.spawn_in(window, async move |this, cx| {
            let (source, font_size) = load.await;

            this.update_in(cx, |this, window, cx| {
                if let Some(font_size) = font_size {
                    set_font_size(font_size, cx);
                }

                this.apply_theme(source, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn load_recent(&mut self, cx: &mut Context<Self>) {
        let local_state = Arc::clone(&Services::app(cx).local_state);
        let load = cx.background_spawn(async move { local_state.recent_workspaces(RECENT_LIMIT) });

        cx.spawn(async move |this, cx| {
            let recent = load.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                this.recent = recent;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Applies a theme source, reading the Omarchy palette off the UI thread.
    pub fn apply_theme(
        &mut self,
        source: ThemeSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.theme_source = source;

        if source != ThemeSource::Omarchy {
            theme::apply(source, None, window, cx);
            self.save_theme_source(source, cx);
            return;
        }

        let local_state = Arc::clone(&Services::app(cx).local_state);
        let read = cx.background_spawn(async move {
            let custom: Option<PathBuf> = local_state
                .setting(theme::OMARCHY_DIRECTORY_SETTING)
                .ok()
                .flatten();
            theme::read_omarchy_palette(custom)
        });

        cx.spawn_in(window, async move |this, cx| {
            let palette = read.await;

            this.update_in(cx, |this, window, cx| {
                if let Applied::FellBack { reason } =
                    theme::apply(source, Some(palette), window, cx)
                {
                    window.push_notification(
                        gpui_kit::component::notification::Notification::warning(format!(
                            "Omarchy palette unavailable, using the system appearance. {reason}"
                        )),
                        cx,
                    );
                }

                this.save_theme_source(source, cx);
                this.watch_omarchy(window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Re-applies the Omarchy palette when the active theme changes.
    fn watch_omarchy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.omarchy_watch.is_some() {
            return;
        }

        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<()>();
        let watcher = theme::watch_omarchy(None, move || {
            // The receiver lives as long as the watch; nothing to do if gone.
            let _ = sender.send(());
        });

        let Ok(watcher) = watcher else {
            return;
        };

        let task = cx.spawn_in(window, async move |this, cx| {
            while receiver.recv().await.is_some() {
                // Theme switches write many files; apply once they settle.
                cx.background_executor().timer(OMARCHY_SETTLE_DELAY).await;
                while receiver.try_recv().is_ok() {}

                let applied = this.update_in(cx, |this, window, cx| {
                    if this.theme_source == ThemeSource::Omarchy {
                        this.apply_theme(ThemeSource::Omarchy, window, cx);
                    }
                });

                if applied.is_err() {
                    break;
                }
            }
        });

        self.omarchy_watch = Some((watcher, task));
    }

    fn save_theme_source(&self, source: ThemeSource, cx: &mut Context<Self>) {
        let local_state = Arc::clone(&Services::app(cx).local_state);

        cx.background_spawn(async move {
            // Preferences are best-effort; the theme already applied.
            let _ = local_state.set_setting(theme::THEME_SOURCE_SETTING, &source);
        })
        .detach();
    }

    fn change_font_size(&mut self, delta: Option<f32>, cx: &mut Context<Self>) {
        let current = f32::from(cx.theme().font_size);
        let next = match delta {
            Some(delta) => (current + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE),
            None => DEFAULT_FONT_SIZE,
        };

        set_font_size(next, cx);

        let local_state = Arc::clone(&Services::app(cx).local_state);
        cx.background_spawn(async move {
            let _ = local_state.set_setting(FONT_SIZE_SETTING, &next);
        })
        .detach();
    }

    fn prompt_and_open(&mut self, create: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(if create { "Create Workspace" } else { "Open" }.into()),
        });

        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            this.update_in(cx, |this, window, cx| {
                if create || Workspace::is_workspace(&path) {
                    let name = create.then(|| folder_name(&path));
                    this.open_path(path, name, window, cx);
                } else {
                    this.confirm_create(path, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn confirm_create(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let name = folder_name(&path);
        let title = format!("Make “{name}” a workspace?");

        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            let path = path.clone();

            alert
                .title(title.clone())
                .description("Qwerio adds a .qwerio folder with the workspace manifest. Existing files are not changed.")
                .ok_text("Create Workspace")
                .show_cancel(true)
                .on_ok(move |_, window, cx| {
                    let name = folder_name(&path);
                    this.update(cx, |this, cx| this.open_path(path.clone(), Some(name), window, cx))
                        .ok();
                    true
                })
        });
    }

    pub fn open_path(
        &mut self,
        path: PathBuf,
        create_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_opening {
            return;
        }

        self.is_opening = true;
        self.error = None;
        cx.notify();

        let open = WorkspaceModel::open(path, create_name, cx);

        cx.spawn_in(window, async move |this, cx| {
            let result = open.await;

            this.update_in(cx, |this, window, cx| {
                this.is_opening = false;

                match result {
                    Ok(model) => {
                        let shell = cx.new(|cx| WorkspaceShell::new(model, window, cx));
                        window.set_window_title(&shell.read(cx).title(cx));
                        this.shell = Some(shell);
                    }
                    Err(error) => this.error = Some(error),
                }

                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn close_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(shell) = self.shell.take() {
            shell.update(cx, |shell, cx| shell.shut_down(cx));
        }

        window.set_window_title("Qwerio");
        self.load_recent(cx);
        cx.notify();
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let recent = self.recent.iter().map(|workspace| {
            let path = workspace.path.clone();
            let id = SharedString::from(format!("recent-{}", path.display()));

            Button::new(id)
                .ghost()
                .w_full()
                .justify_start()
                .child(
                    v_flex().items_start().child(workspace.name.clone()).child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(path.display().to_string()),
                    ),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_path(path.clone(), None, window, cx);
                }))
        });

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .w(px(440.))
                    .max_w_full()
                    .p_6()
                    .gap_6()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xl().font_semibold().child("Qwerio"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Open a folder to browse databases beside your queries and notes."),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("open-workspace")
                                    .primary()
                                    .icon(IconName::FolderOpen)
                                    .label("Open Workspace…")
                                    .loading(self.is_opening)
                                    .on_click(cx.listener(|this, _, window, cx| this.prompt_and_open(false, window, cx))),
                            )
                            .child(
                                Button::new("new-workspace")
                                    .outline()
                                    .label("New Workspace…")
                                    .on_click(cx.listener(|this, _, window, cx| this.prompt_and_open(true, window, cx))),
                            ),
                    )
                    .when_some(self.error.clone(), |this, error| {
                        this.child(div().text_sm().text_color(cx.theme().danger).child(error))
                    })
                    .when(!self.recent.is_empty(), |this| {
                        this.child(
                            v_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Recent"),
                                )
                                .children(recent),
                        )
                    }),
            )
    }
}

fn set_font_size(size: f32, cx: &mut App) {
    Theme::update(cx, |theme| theme.font_size = px(size));
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Workspace".to_owned())
}

impl Focusable for Workbench {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workbench {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("workbench")
            .key_context(WORKBENCH_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &OpenWorkspace, window, cx| {
                this.prompt_and_open(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewWorkspace, window, cx| {
                this.prompt_and_open(true, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &CloseWorkspace, window, cx| {
                    this.close_workspace(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &UseSystemTheme, window, cx| {
                this.apply_theme(ThemeSource::System, window, cx)
            }))
            .on_action(cx.listener(|this, _: &UseLightTheme, window, cx| {
                this.apply_theme(ThemeSource::Light, window, cx)
            }))
            .on_action(cx.listener(|this, _: &UseDarkTheme, window, cx| {
                this.apply_theme(ThemeSource::Dark, window, cx)
            }))
            .on_action(cx.listener(|this, _: &UseOmarchyTheme, window, cx| {
                this.apply_theme(ThemeSource::Omarchy, window, cx)
            }))
            .on_action(cx.listener(|this, _: &IncreaseFontSize, _, cx| {
                this.change_font_size(Some(FONT_SIZE_STEP), cx)
            }))
            .on_action(cx.listener(|this, _: &DecreaseFontSize, _, cx| {
                this.change_font_size(Some(-FONT_SIZE_STEP), cx)
            }))
            .on_action(
                cx.listener(|this, _: &ResetFontSize, _, cx| this.change_font_size(None, cx)),
            )
            .map(|this| match &self.shell {
                Some(shell) => this.child(shell.clone()),
                None => this.child(self.render_welcome(cx)),
            })
    }
}
