use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{PathPromptOptions, Task};
use qwerio_app::database_transfer::{TransferRequest, run, tools};

use super::*;

impl TransferForm {
    pub(super) fn load_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let resolve = self.model.read(cx).resolve(self.connection, cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = resolve.await;
            this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(resolved) => this.options = Some(resolved.descriptor.options),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn check_tools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tool) = self.format.tool(self.engine, self.operation) else {
            return;
        };
        if self.is_running || self.is_checking {
            return;
        }
        let directory = self.selected_tool_directory(cx);
        let checked_directory = directory.clone();
        let format = self.format;
        self.is_checking = true;
        self.tool = None;
        self.tool_error = None;
        let check = cx.background_spawn(async move { tools::detect(tool, directory.as_deref()) });
        cx.spawn_in(window, async move |this, cx| {
            let result = check.await;
            this.update_in(cx, |this, _, cx| {
                this.is_checking = false;
                if this.format == format && this.selected_tool_directory(cx) == checked_directory {
                    match result {
                        Ok(tool) => this.tool = Some(tool),
                        Err(error) => this.tool_error = Some(error.into()),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub(super) fn choose_file(
        &mut self,
        is_restored_file: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_running {
            return;
        }
        let is_save = is_restored_file || self.operation == Operation::Backup;
        let prompt: Task<Option<PathBuf>> = if is_save {
            let directory = self.model.read(cx).directory();
            let name = if is_restored_file {
                "restored.sqlite".to_owned()
            } else {
                self.suggested_name()
            };
            let prompt = cx.prompt_for_new_path(&directory, Some(&name));
            cx.spawn(async move |_, _| prompt.await.ok().and_then(Result::ok).flatten())
        } else {
            let prompt = cx.prompt_for_paths(PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: Some("Choose backup".into()),
            });
            cx.spawn(async move |_, _| {
                prompt
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten()
                    .and_then(|paths| paths.into_iter().next())
            })
        };
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = prompt.await else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                let input = if is_restored_file {
                    &this.restored_file
                } else {
                    &this.file
                };
                input.update(cx, |input, cx| {
                    input.set_value(path.display().to_string(), window, cx)
                });
                this.has_confirmed_restore = false;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn locate_tools(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Locate Tools".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = prompt.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| {
                this.tool_directory.update(cx, |input, cx| {
                    input.set_value(path.display().to_string(), window, cx)
                });
                this.check_tools(window, cx);
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_start(cx) {
            return;
        }
        let expected_options = self.options.clone();
        let resolve = self.model.read(cx).resolve(self.connection, cx);
        let database = self.database.clone();
        let operation = self.operation;
        let format = self.format;
        let file = PathBuf::from(Self::text(&self.file, cx));
        let restored_file = (self.engine == EngineKind::Sqlite && operation == Operation::Restore)
            .then(|| PathBuf::from(Self::text(&self.restored_file, cx)));
        let tool = self.tool.as_ref().map(|tool| tool.path.clone());
        self.cancelled = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&self.cancelled);
        self.is_running = true;
        self.error = None;
        self.notice = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let resolved = resolve.await.map_err(|error| error.to_string())?;
                if Some(&resolved.descriptor.options) != expected_options.as_ref() {
                    return Err(
                        "Connection settings changed. Reopen this dialog before continuing."
                            .to_owned(),
                    );
                }
                let request = TransferRequest {
                    options: resolved.descriptor.options,
                    credentials: resolved.credentials,
                    database,
                    operation,
                    format,
                    file,
                    restored_file,
                    tool,
                };
                cx.background_spawn(async move { run(request, &cancelled) })
                    .await
            }
            .await;
            this.update_in(cx, |this, window, cx| {
                this.is_running = false;
                this.has_confirmed_restore = false;
                match result {
                    Ok(path) => {
                        let message = match operation {
                            Operation::Backup => format!("Backup saved to {}", path.display()),
                            Operation::Restore if this.engine == EngineKind::Sqlite => format!(
                                "Restored to {}. Open this file as a SQLite connection.",
                                path.display()
                            ),
                            Operation::Restore => {
                                "Database restored. Refresh open tables to see the restored data."
                                    .to_owned()
                            }
                        };
                        this.notice = Some(message.clone().into());
                        window.push_notification(Notification::success(message), cx);
                    }
                    Err(error) => this.error = Some(error.into()),
                }
                if operation == Operation::Restore && this.engine != EngineKind::Sqlite {
                    this.model.update(cx, |model, cx| {
                        model.forget_connection_cache(this.connection, cx)
                    });
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}
