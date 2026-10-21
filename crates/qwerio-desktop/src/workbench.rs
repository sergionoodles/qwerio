//! The root view of each window: the workspace shell, or the welcome screen
//! until a first workspace exists. Reopens the last workspace on start and
//! owns window-wide commands such as opening workspace files, settings, and
//! text size.

mod files;

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, IconName, StyledExt as _, Theme, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Window, WindowBounds,
    WindowDecorations, WindowOptions, div, img, px, size,
};
use qwerio_app::local_state::RecentWorkspace;

use crate::about_dialog;
use crate::actions::*;
use crate::assets::LOGO_PATH;
use crate::platform::paths::APPLICATION_ID;
use crate::services::Services;
use crate::settings_dialog;
use crate::shell::WorkspaceShell;
use crate::theme::controller::ThemeController;
use crate::workspace_model::WorkspaceModel;

const RECENT_LIMIT: u32 = 8;
const DEFAULT_FONT_SIZE: f32 = 14.0;
const MIN_FONT_SIZE: f32 = 10.0;
const MAX_FONT_SIZE: f32 = 24.0;
const FONT_SIZE_STEP: f32 = 1.0;
pub(crate) const FONT_SIZE_SETTING: &str = "font_size";

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
    /// Startup is reopening the last workspace; the welcome screen waits.
    is_restoring: bool,
    is_opening: bool,
    error: Option<SharedString>,
    theme: Entity<ThemeController>,
}

impl Workbench {
    pub(crate) fn new(
        workspace: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut workbench = Self {
            focus_handle: cx.focus_handle(),
            shell: None,
            recent: Vec::new(),
            is_restoring: workspace.is_none(),
            is_opening: false,
            error: None,
            theme: cx.new(|cx| ThemeController::new(window, cx)),
        };

        let this = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            this.update(cx, |this, cx| {
                this.guard_workspace(window, cx, |window, _| window.remove_window());
            })
            .ok();
            false
        });
        window.focus(&workbench.focus_handle, cx);
        workbench.load_font_size(window, cx);

        match workspace {
            Some(path) => {
                workbench.load_recent(cx);
                workbench.open_path(path, None, window, cx);
            }
            None => workbench.reopen_last_workspace(window, cx),
        }

        workbench
    }

    fn load_font_size(&self, window: &mut Window, cx: &mut Context<Self>) {
        let local_state = Arc::clone(&Services::app(cx).local_state);
        let load = cx.background_spawn(async move {
            local_state.setting::<f32>(FONT_SIZE_SETTING).ok().flatten()
        });

        cx.spawn_in(window, async move |_, cx| {
            if let Some(font_size) = load.await {
                cx.update(|_, cx| set_font_size(font_size, cx)).ok();
            }
        })
        .detach();
    }

    /// Opens the most recently opened workspace, or shows the welcome screen
    /// when there is none.
    fn reopen_last_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let local_state = Arc::clone(&Services::app(cx).local_state);
        let load = cx.background_spawn(async move { local_state.recent_workspaces(RECENT_LIMIT) });

        cx.spawn_in(window, async move |this, cx| {
            let recent = load.await.unwrap_or_default();

            this.update_in(cx, |this, window, cx| {
                let last = recent.first().map(|workspace| workspace.path.clone());
                this.recent = recent;

                match last {
                    Some(path) => this.open_path(path, None, window, cx),
                    None => {
                        this.is_restoring = false;
                        this.initialize_default(window, cx);
                    }
                }

                cx.notify();
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

    pub fn open_path(
        &mut self,
        path: PathBuf,
        create_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        self.guard_workspace(window, cx, move |window, cx| {
            this.update(cx, |this, cx| {
                this.prepare_open(path, create_name, window, cx)
            })
            .ok();
        });
    }

    fn load_workspace(
        &mut self,
        path: PathBuf,
        create_name: Option<String>,
        password: Option<qwerio_core::connection::Secret>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_opening {
            return;
        }

        self.is_opening = true;
        self.error = None;
        cx.notify();

        let open = WorkspaceModel::open_with_password(path, create_name, password, cx);

        cx.spawn_in(window, async move |this, cx| {
            let result = open.await;

            this.update_in(cx, |this, window, cx| {
                this.complete_open(result, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn complete_open(
        &mut self,
        result: Result<Entity<WorkspaceModel>, SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_opening = false;
        self.is_restoring = false;
        match result {
            Ok(model) => {
                let owner = cx.entity().downgrade();
                self.guard_workspace(window, cx, move |window, cx| {
                    owner
                        .update(cx, |this, cx| this.install_workspace(model, window, cx))
                        .ok();
                });
            }
            Err(error) => self.report_error(error, window, cx),
        }
        cx.notify();
    }

    fn install_workspace(
        &mut self,
        model: Entity<WorkspaceModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let recent = model.read(cx).record_recent(cx);
        if let Some(previous) = self.shell.take() {
            previous.update(cx, |shell, cx| shell.shut_down(cx));
        }
        let shell = cx.new(|cx| WorkspaceShell::new(model, window, cx));
        window.set_window_title(&shell.read(cx).title(cx));
        self.shell = Some(shell);
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = recent.await {
                this.update_in(cx, |this, window, cx| this.report_error(error, window, cx))
                    .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn close_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        self.guard_workspace(window, cx, move |window, cx| {
            this.update(cx, |this, cx| {
                if let Some(shell) = this.shell.take() {
                    shell.update(cx, |shell, cx| shell.shut_down(cx));
                }
                window.set_window_title("Qwerio");
                window.focus(&this.focus_handle, cx);
                this.load_recent(cx);
                cx.notify();
            })
            .ok();
        });
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .w(px(440.))
                    .max_w_full()
                    .p_6()
                    .gap_6()
                    .child(img(LOGO_PATH).size(px(96.)))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xl().font_semibold().child("Qwerio"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Open a .qwerio workspace to browse databases, queries, and notes."),
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

            )
    }
}

fn set_font_size(size: f32, cx: &mut App) {
    Theme::update(cx, |theme| theme.font_size = px(size));
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
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &OpenWorkspace, window, cx| {
                this.prompt_and_open(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewWorkspace, window, cx| {
                this.prompt_and_open(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ImportWorkspace, window, cx| {
                this.import_workspace(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ExportWorkspace, window, cx| {
                this.export_workspace(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &CloseWorkspace, window, cx| {
                    this.close_workspace(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                settings_dialog::open(
                    this.theme.clone(),
                    this.shell.as_ref().map(|shell| shell.read(cx).model()),
                    window,
                    cx,
                )
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
            .on_action(cx.listener(|this, _: &Quit, window, cx| {
                this.guard_workspace(window, cx, |_, cx| cx.quit());
            }))
            .on_action(|_: &About, window, cx| about_dialog::open(window, cx))
            .map(|this| match &self.shell {
                Some(shell) => this.child(shell.clone()),
                None if self.is_restoring => this,
                None => this.child(self.render_welcome(cx)),
            })
    }
}
