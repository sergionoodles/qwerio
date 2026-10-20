//! Workspace lifecycle: native file pickers, password choices, and leave guards.
use super::*;
use crate::platform::paths::AppPaths;
use crate::workspace_password;
use crate::workspace_switcher::{choose_destination, choose_source};
use gpui_kit::WeakEntity;
use qwerio_core::connection::Secret;

impl Workbench {
    pub(super) fn guard_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        proceed: impl FnOnce(&mut Window, &mut App) + 'static,
    ) {
        let deferred = move |window: &mut Window, cx: &mut App| window.defer(cx, proceed);
        match &self.shell {
            Some(shell) => {
                shell.update(cx, |shell, cx| shell.leave_workspace(window, cx, deferred))
            }
            None => deferred(window, cx),
        }
    }

    pub(super) fn initialize_default(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let services = Services::app(cx);
        let load = cx.background_spawn(async move {
            let configured = services
                .local_state
                .setting::<PathBuf>("default_workspace_path")
                .map_err(|error| error.to_string())?;
            configured
                .or_else(|| {
                    AppPaths::discover().map(|paths| paths.data_directory.join("Default.qwerio"))
                })
                .ok_or_else(|| "Could not locate your application data directory".to_owned())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(path) => {
                    if path.exists() {
                        return this.prepare_open(path, None, window, cx);
                    }
                    let this = cx.entity().downgrade();
                    workspace_password::open(
                        "Encrypt Your Default Workspace?",
                        true,
                        window,
                        cx,
                        move |password, window, cx| {
                            this.update(cx, |this, cx| {
                                this.load_workspace(
                                    path,
                                    Some("Default".into()),
                                    password,
                                    window,
                                    cx,
                                )
                            })
                            .ok();
                        },
                    );
                }
                Err(error) => this.report_error(error, window, cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn prompt_and_open(
        &mut self,
        create: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity().downgrade();
        self.guard_workspace(window, cx, move |window, cx| {
            if create {
                choose_destination(
                    this,
                    "New Workspace.qwerio",
                    window,
                    cx,
                    |this, path, window, cx| {
                        this.update(cx, |this, cx| {
                            this.prepare_open(path.clone(), Some(workspace_name(&path)), window, cx)
                        })
                        .ok();
                    },
                );
            } else {
                choose_source(this, false, window, cx, |this, path, window, cx| {
                    this.update(cx, |this, cx| this.prepare_open(path, None, window, cx))
                        .ok();
                });
            }
        });
    }

    pub(super) fn prepare_open(
        &mut self,
        path: PathBuf,
        create: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if create.is_none()
            && self
                .shell
                .as_ref()
                .is_some_and(|shell| shell.read(cx).model().read(cx).root() == &path)
        {
            return;
        }
        let optional = create.is_some();
        if !optional && path.is_dir() {
            return self.load_workspace(path, None, None, window, cx);
        }
        let this = cx.entity().downgrade();
        let inspect_path = path.clone();
        let inspect = cx.background_spawn(async move {
            if optional {
                Ok(true)
            } else {
                qwerio_workspace::file::requires_password(&inspect_path)
            }
        });
        cx.spawn_in(window, async move |_, cx| {
            let result = inspect.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(false) => this.load_workspace(path, create, None, window, cx),
                Ok(true) => {
                    this.is_restoring = false;
                    cx.notify();
                    let this = cx.entity().downgrade();
                    workspace_password::open(
                        if optional {
                            "Encrypt Workspace?"
                        } else {
                            "Unlock Workspace"
                        },
                        optional,
                        window,
                        cx,
                        move |password, window, cx| {
                            this.update(cx, |this, cx| {
                                this.load_workspace(path, create, password, window, cx)
                            })
                            .ok();
                        },
                    );
                }
                Err(error) => this.report_error(error.to_string(), window, cx),
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn report_error(
        &mut self,
        error: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let error = error.into();
        self.is_restoring = false;
        self.error = Some(error.clone());
        window.push_notification(Notification::error(error), cx);
        cx.notify();
    }

    pub(super) fn export_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.shell.as_ref().map(|shell| shell.read(cx).model()) else {
            return;
        };
        let this = cx.entity().downgrade();
        choose_destination(
            this,
            "Export.qwerio",
            window,
            cx,
            move |this, path, window, cx| {
                workspace_password::open(
                    "Encrypt Export?",
                    true,
                    window,
                    cx,
                    move |password, window, cx| {
                        let export = model.read(cx).export(path, password, cx);
                        this.update(cx, |_, cx| {
                            cx.spawn_in(window, async move |this, cx| {
                                let result = export.await;
                                this.update_in(cx, |this, window, cx| match result {
                                    Ok(()) => window.push_notification(
                                        Notification::success("Workspace exported"),
                                        cx,
                                    ),
                                    Err(error) => this.report_error(error, window, cx),
                                })
                                .ok();
                            })
                            .detach();
                        })
                        .ok();
                    },
                );
            },
        );
    }

    pub(super) fn import_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        self.guard_workspace(window, cx, move |window, cx| {
            choose_source(this, true, window, cx, |this, path, window, cx| {
                let is_file = path.is_file();
                if !is_file {
                    return import_unlocked(this, path, None, window, cx);
                }
                // Inspect the header off the UI thread before asking for a password.
                let inspected_path = path.clone();
                let inspect = cx.background_spawn(async move {
                    qwerio_workspace::file::requires_password(&inspected_path)
                });
                let task_owner = this.clone();
                this.update(cx, |_, cx| {
                    cx.spawn_in(window, async move |_, cx| {
                        let result = inspect.await;
                        cx.update(|window, cx| match result {
                            Ok(false) => import_unlocked(task_owner, path, None, window, cx),
                            Ok(true) => workspace_password::open(
                                "Unlock Import",
                                false,
                                window,
                                cx,
                                move |password, window, cx| {
                                    import_unlocked(task_owner, path, password, window, cx)
                                },
                            ),
                            Err(error) => {
                                task_owner
                                    .update(cx, |this, cx| {
                                        this.report_error(error.to_string(), window, cx)
                                    })
                                    .ok();
                            }
                        })
                        .ok();
                    })
                    .detach();
                })
                .ok();
            });
        });
    }
}

fn workspace_name(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Workspace".to_owned())
}

fn import_unlocked(
    owner: WeakEntity<Workbench>,
    path: PathBuf,
    password: Option<Secret>,
    window: &mut Window,
    cx: &mut App,
) {
    let services = Services::app(cx);
    let read = cx.background_spawn(async move {
        qwerio_app::workspace_store::WorkspaceStore::open_for_import(
            &path,
            password.as_ref(),
            services,
        )
    });
    window
        .spawn(cx, async move |cx| {
            let result = read.await;
            cx.update(|window, cx| match result {
                Ok(storage) => choose_import_destination(owner, storage, window, cx),
                Err(error) => {
                    owner
                        .update(cx, |this, cx| {
                            this.report_error(error.to_string(), window, cx)
                        })
                        .ok();
                }
            })
            .ok();
        })
        .detach();
}

fn choose_import_destination(
    owner: WeakEntity<Workbench>,
    storage: qwerio_app::workspace_store::WorkspaceStore,
    window: &mut Window,
    cx: &mut App,
) {
    choose_destination(
        owner,
        "Imported.qwerio",
        window,
        cx,
        move |owner, path, window, cx| {
            workspace_password::open(
                "Encrypt Imported Workspace?",
                true,
                window,
                cx,
                move |password, window, cx| {
                    write_import(owner, storage, path, password, window, cx);
                },
            );
        },
    );
}

fn write_import(
    owner: WeakEntity<Workbench>,
    storage: qwerio_app::workspace_store::WorkspaceStore,
    path: PathBuf,
    password: Option<Secret>,
    window: &mut Window,
    cx: &mut App,
) {
    let destination = path.clone();
    let export_password = password.clone();
    let write =
        cx.background_spawn(async move { storage.export(&destination, export_password.as_ref()) });
    window
        .spawn(cx, async move |cx| {
            let result = write.await;
            cx.update(|window, cx| {
                owner
                    .update(cx, |this, cx| match result {
                        Ok(()) => this.load_workspace(path, None, password, window, cx),
                        Err(error) => this.report_error(error.to_string(), window, cx),
                    })
                    .ok();
            })
            .ok();
        })
        .detach();
}
