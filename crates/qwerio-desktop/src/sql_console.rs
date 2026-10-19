//! The SQL console under a SQL document: its explicit execution target,
//! a retained session, run/cancel, transaction controls, and results.
//!
//! Nothing runs until the user runs a statement, and only after choosing the
//! access mode. The target is always visible; moving the document never
//! changes where it runs.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::table::{DataTable, TableState};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, IndexPath, Selectable as _, Sizable as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, TestSupportExt as _, Window, div,
};
use qwerio_app::console::{ConsoleState, ExecutionTarget, RunOutcome};
use qwerio_app::sessions::ConsoleLease;
use qwerio_core::connection::EngineKind;
use qwerio_core::driver::QueryRequest;
use qwerio_core::ids::ConnectionId;
use qwerio_core::result::RequestEventKind;
use qwerio_core::session::{AccessMode, TransactionState};
use qwerio_workspace::manifest::{DocumentId, ExecutionBinding, Placement};

use crate::content::ContentEvent;
use crate::grid::ResultGridDelegate;
use crate::workspace_model::WorkspaceModel;

/// Where a statement's text sits in the document, to locate error positions.
#[derive(Clone, Copy)]
struct SourceSpan {
    start: usize,
}

pub struct SqlConsole {
    model: Entity<WorkspaceModel>,
    /// The saved document whose execution binding this console uses and
    /// updates; temporary queries have none.
    document: Option<DocumentId>,
    suggested_target: Option<ExecutionBinding>,
    state: ConsoleState,
    lease: Option<(ExecutionTarget, ConsoleLease)>,
    connection_ids: Vec<ConnectionId>,
    connection_select: Entity<SelectState<Vec<SharedString>>>,
    database_input: Entity<InputState>,
    access: Option<AccessMode>,
    results: Entity<TableState<ResultGridDelegate>>,
    is_opening: bool,
    notice: Option<SharedString>,
    last_span: Option<SourceSpan>,
    run_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Requests from the console to its document.
pub enum ConsoleEvent {
    /// Select a byte range in the document, such as an error position.
    SelectRange(std::ops::Range<usize>),
}

impl EventEmitter<ConsoleEvent> for SqlConsole {}
impl EventEmitter<ContentEvent> for SqlConsole {}

impl SqlConsole {
    pub fn new(
        model: Entity<WorkspaceModel>,
        document: Option<DocumentId>,
        suggested_target: Option<ExecutionBinding>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let connection_select =
            cx.new(|cx| SelectState::new(Vec::<SharedString>::new(), None, window, cx));
        let database_input = cx.new(|cx| InputState::new(window, cx).placeholder("Database"));
        let results = cx
            .new(|cx| TableState::new(ResultGridDelegate::new(), window, cx).cell_selectable(true));

        let subscriptions = vec![
            cx.observe_in(&model, window, |this, _, window, cx| {
                this.sync_connections(window, cx)
            }),
            cx.subscribe(
                &connection_select,
                |this, _, _: &SelectEvent<Vec<SharedString>>, cx| {
                    this.notice = None;
                    cx.emit(ContentEvent::Changed);
                    cx.notify();
                },
            ),
        ];

        let mut console = Self {
            model,
            document,
            suggested_target,
            state: ConsoleState::new(None),
            lease: None,
            connection_ids: Vec::new(),
            connection_select,
            database_input,
            access: None,
            results,
            is_opening: false,
            notice: None,
            last_span: None,
            run_task: None,
            _subscriptions: subscriptions,
        };

        console.sync_connections(window, cx);
        console.select_default_target(window, cx);
        console
    }

    fn sync_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let manifest = Arc::clone(self.model.read(cx).manifest());
        let ids: Vec<ConnectionId> = manifest
            .connections
            .iter()
            .map(|connection| connection.id)
            .collect();

        if ids == self.connection_ids {
            return;
        }

        let selected = self.selected_connection(cx);
        let names: Vec<SharedString> = manifest
            .connections
            .iter()
            .map(|connection| {
                SharedString::from(format!("{} ({})", connection.name, connection.engine))
            })
            .collect();
        let index = selected.and_then(|selected| ids.iter().position(|id| *id == selected));
        self.connection_ids = ids;

        self.connection_select.update(cx, |select, cx| {
            select.set_items(names, window, cx);
            select.set_selected_index(index.map(|row| IndexPath::default().row(row)), window, cx);
        });
    }

    /// Prefers the document's saved binding; otherwise suggests the
    /// connection it is placed under. The suggestion is visible and only
    /// becomes the binding when the user runs a statement.
    fn select_default_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let manifest = Arc::clone(self.model.read(cx).manifest());
        let entry = self
            .document
            .and_then(|document| manifest.document(document));

        let (connection, database) = match entry.map(|entry| (&entry.execution, &entry.placement)) {
            Some((Some(binding), _)) => (Some(binding.connection), binding.database.clone()),
            Some((
                None,
                Placement::Container {
                    connection,
                    database,
                    ..
                },
            )) => (Some(*connection), database.clone()),
            _ => match &self.suggested_target {
                Some(binding) => (Some(binding.connection), binding.database.clone()),
                None => (None, None),
            },
        };

        if let Some(index) = connection
            .and_then(|connection| self.connection_ids.iter().position(|id| *id == connection))
        {
            self.connection_select.update(cx, |select, cx| {
                select.set_selected_index(Some(IndexPath::default().row(index)), window, cx);
            });
        }

        if let Some(database) = database {
            self.database_input
                .update(cx, |input, cx| input.set_value(database, window, cx));
        }
    }

    fn selected_connection(&self, cx: &App) -> Option<ConnectionId> {
        let index = self.connection_select.read(cx).selected_index(cx)?;
        self.connection_ids.get(index.row).copied()
    }

    fn selected_engine(&self, cx: &App) -> Option<EngineKind> {
        let connection = self.selected_connection(cx)?;
        self.model
            .read(cx)
            .manifest()
            .connection(connection)
            .map(|alias| alias.engine)
    }

    fn target(&self, cx: &App) -> Option<ExecutionTarget> {
        let connection = self.selected_connection(cx)?;
        let access = self.access?;
        let database = match self.selected_engine(cx) {
            Some(EngineKind::Postgres) => {
                let value = self.database_input.read(cx).value().trim().to_owned();
                (!value.is_empty()).then_some(value)
            }
            _ => None,
        };

        Some(ExecutionTarget {
            connection,
            database,
            access,
        })
    }

    pub fn is_running(&self) -> bool {
        self.state.is_running()
    }

    /// A short description of the target and state for the status bar.
    pub fn status_text(&self, cx: &App) -> SharedString {
        let manifest = self.model.read(cx).manifest();
        let target = match self
            .selected_connection(cx)
            .and_then(|id| manifest.connection(id))
        {
            Some(alias) => alias.name.clone(),
            None => "No connection".to_owned(),
        };
        let access = match self.access {
            Some(AccessMode::ReadOnly) => "read-only",
            Some(AccessMode::ReadWrite) => "read-write",
            None => "mode not chosen",
        };
        let transaction = match self.state.transaction() {
            TransactionState::Idle => "",
            TransactionState::Active => " · transaction open",
            TransactionState::Failed => " · transaction failed",
            TransactionState::Unknown => " · transaction state unknown",
        };

        format!("Runs on {target} ({access}){transaction}").into()
    }

    pub fn needs_confirmation_to_close(&self) -> bool {
        self.state.needs_confirmation_to_close()
    }

    fn set_access(&mut self, access: AccessMode, cx: &mut Context<Self>) {
        self.access = Some(access);
        self.notice = None;
        cx.emit(ContentEvent::Changed);
        cx.notify();
    }

    /// Runs one statement. `span_start` is the statement's byte offset in the
    /// document, used to point at error positions.
    pub fn run(
        &mut self,
        sql: String,
        span_start: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.is_running() || self.is_opening {
            return;
        }

        let Some(target) = self.target(cx) else {
            self.notice = Some(if self.selected_connection(cx).is_none() {
                "Choose the connection this document runs on.".into()
            } else {
                "Choose read-only or read-write before running.".into()
            });
            cx.notify();
            return;
        };

        if let Some((current, _)) = &self.lease
            && *current != target
            && self.state.transaction().is_open()
        {
            self.notice = Some("Commit or roll back before changing the target.".into());
            cx.notify();
            return;
        }

        self.remember_binding(&target, cx);
        self.last_span = Some(SourceSpan { start: span_start });

        let lease = self.lease_for(target.clone(), cx);
        self.is_opening = true;
        self.notice = None;
        cx.notify();

        self.run_task = Some(cx.spawn_in(window, async move |this, cx| {
            let lease = match lease.await {
                Ok(lease) => lease,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.is_opening = false;
                        this.notice = Some(error);
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };

            let request = QueryRequest::new(sql);
            let started = this.update(cx, |this, cx| {
                this.is_opening = false;

                if let Some(lease) = lease {
                    this.lease = Some((target.clone(), lease));
                }

                let session = this
                    .lease
                    .as_ref()
                    .map(|(_, lease)| Arc::clone(lease.session()))?;
                let stream = match session.execute(request.clone()) {
                    Ok(stream) => stream,
                    Err(error) => {
                        this.notice = Some(error.message.into());
                        cx.notify();
                        return None;
                    }
                };

                this.state.start(
                    request.id,
                    request.sql.clone(),
                    Instant::now(),
                    SystemTime::now(),
                );
                this.results.update(cx, |results, cx| {
                    results.delegate_mut().set_empty_message("Running…");
                    cx.notify();
                });
                cx.emit(ContentEvent::Changed);
                cx.notify();
                Some(stream)
            });

            let Ok(Some(mut stream)) = started else {
                return;
            };

            while let Some(event) = stream.events.recv().await {
                let applied =
                    this.update_in(cx, |this, window, cx| this.apply_event(event, window, cx));

                if applied.is_err() {
                    break;
                }
            }
        }));
    }

    /// The open session for `target`, or a task opening one. `Ok(None)`
    /// means the current lease already matches.
    fn lease_for(
        &mut self,
        target: ExecutionTarget,
        cx: &mut Context<Self>,
    ) -> Task<Result<Option<ConsoleLease>, SharedString>> {
        if self
            .lease
            .as_ref()
            .is_some_and(|(current, _)| *current == target)
        {
            return Task::ready(Ok(None));
        }

        if let Some((_, previous)) = self.lease.take() {
            let session = Arc::clone(previous.session());
            cx.background_spawn(async move {
                // Closing rolls back nothing here: target changes require an
                // idle transaction state.
                let _ = session.close().await;
                drop(previous);
            })
            .detach();
        }

        let resolve = self.model.read(cx).resolve(target.connection, cx);
        let sessions = Arc::clone(&self.model.read(cx).services().sessions);

        cx.spawn(async move |_, _| {
            let resolved = resolve.await?;
            sessions
                .open_console(
                    resolved.descriptor,
                    resolved.credentials,
                    target.database.clone(),
                    target.access,
                )
                .await
                .map(Some)
                .map_err(|error| SharedString::from(error.message))
        })
    }

    fn remember_binding(&mut self, target: &ExecutionTarget, cx: &mut Context<Self>) {
        let Some(document) = self.document else {
            return;
        };

        let binding = ExecutionBinding {
            connection: target.connection,
            database: target.database.clone(),
        };
        let current = self
            .model
            .read(cx)
            .manifest()
            .document(document)
            .and_then(|entry| entry.execution.clone());

        if current.as_ref() == Some(&binding) {
            return;
        }

        let update = self.model.update(cx, |model, cx| {
            model.update_workspace(cx, move |workspace| {
                workspace.set_execution_binding(document, Some(binding))
            })
        });
        cx.spawn(async move |_, _| {
            // A failed manifest write leaves the binding unsaved; the query
            // still runs on the visible target.
            let _ = update.await;
        })
        .detach();
    }

    fn apply_event(
        &mut self,
        event: qwerio_core::result::RequestEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_result = matches!(
            event.kind,
            RequestEventKind::ResultStarted { .. }
                | RequestEventKind::Rows { .. }
                | RequestEventKind::StatementCompleted { .. }
        );
        let completion = self.state.apply_event(event, Instant::now());

        if is_result {
            let (columns, rows) = match self.state.result() {
                Some(result) => (Arc::clone(&result.columns), result.rows.clone()),
                None => (Arc::from(Vec::new()), Vec::new()),
            };

            self.results.update(cx, |results, cx| {
                let column_count_changed = results.delegate().columns().len() != columns.len();
                results.delegate_mut().set_result(columns, rows);
                results
                    .delegate_mut()
                    .set_empty_message("The statement returned no rows");

                if column_count_changed {
                    results.refresh(cx);
                }

                cx.notify();
            });
        }

        if let Some(completion) = completion {
            if let RunOutcome::Failed(error) = &completion.run.outcome
                && let (Some(position), Some(span)) = (error.position, self.last_span)
            {
                let start = span.start + position.byte_offset;
                cx.emit(ConsoleEvent::SelectRange(start..start + 1));
            }

            if let Some(history) = completion.history {
                let model = self.model.read(cx);
                let local_state = Arc::clone(&model.services().local_state);
                let workspace = model.manifest().id;

                cx.background_spawn(async move {
                    // History is best-effort and bounded by its retention.
                    let _ = local_state.record_history(workspace, &history, SystemTime::now());
                })
                .detach();
            }
        }

        cx.emit(ContentEvent::Changed);
        cx.notify();
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        let (Some(request), Some((_, lease))) = (self.state.running_request(), &self.lease) else {
            return;
        };

        self.state.mark_cancel_requested();
        let session = Arc::clone(lease.session());

        cx.spawn(async move |this, cx| {
            let result = session.cancel(request).await;

            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.notice = Some(error.message.into());
                }

                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    pub fn transaction_command(
        &mut self,
        command: TransactionCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.state.is_running() {
            return;
        }

        let Some(target) = self.target(cx) else {
            self.notice = Some("Choose the connection and access mode first.".into());
            cx.notify();
            return;
        };

        let lease = self.lease_for(target.clone(), cx);

        cx.spawn_in(window, async move |this, cx| {
            let lease = lease.await;
            let session = this.update(cx, |this, _| {
                if let Ok(Some(lease)) = lease {
                    this.lease = Some((target, lease));
                }

                this.lease
                    .as_ref()
                    .map(|(_, lease)| Arc::clone(lease.session()))
            });

            let Ok(Some(session)) = session else {
                return;
            };

            let result = match command {
                TransactionCommand::Begin => session.begin().await,
                TransactionCommand::Commit => session.commit().await,
                TransactionCommand::Rollback => session.rollback().await,
            };

            this.update(cx, |this, cx| {
                match result {
                    Ok(state) => {
                        this.state.set_transaction(state);
                        this.notice = None;
                    }
                    Err(error) => {
                        this.state.set_transaction(session.transaction_state());
                        this.notice = Some(error.message.into());
                    }
                }

                cx.emit(ContentEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Ends the session, rolling back any open transaction.
    pub fn shut_down(&mut self, cx: &mut Context<Self>) {
        if let Some((_, lease)) = self.lease.take() {
            let session = Arc::clone(lease.session());
            cx.background_spawn(async move {
                let _ = session.close().await;
                drop(lease);
            })
            .detach();
        }
    }

    fn render_status(&self, cx: &App) -> impl IntoElement {
        let (text, is_error): (SharedString, bool) = if let Some(notice) = &self.notice {
            (notice.clone(), true)
        } else if self.is_opening {
            ("Connecting…".into(), false)
        } else if self.state.is_running() {
            if self.state.is_cancel_requested() {
                ("Cancelling…".into(), false)
            } else {
                ("Running…".into(), false)
            }
        } else {
            match self.state.last_run() {
                None => (
                    "Choose a target and run a statement with the Run button.".into(),
                    false,
                ),
                Some(run) => describe_run(run),
            }
        };

        div()
            .id("console-status")
            .text_sm()
            .truncate()
            .text_color(if is_error {
                cx.theme().danger
            } else {
                cx.theme().muted_foreground
            })
            .role(Role::Status)
            .aria_label(text.clone())
            .child(text)
            .test_support()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransactionCommand {
    Begin,
    Commit,
    Rollback,
}

fn describe_run(run: &qwerio_app::console::FinishedRun) -> (SharedString, bool) {
    let duration = format!("{} ms", run.duration.as_millis());

    match &run.outcome {
        RunOutcome::Cancelled => (format!("Cancelled after {duration}").into(), false),
        RunOutcome::Failed(error) => {
            let mut text = error.message.clone();

            if let Some(hint) = &error.hint {
                text.push_str(" — ");
                text.push_str(hint);
            }

            (text.into(), true)
        }
        RunOutcome::Succeeded => {
            let summary = run.summary.as_ref();
            let text = match summary {
                Some(summary) if summary.truncation.is_some() => format!(
                    "{} rows shown; the rest was not loaded to stay within memory limits · {duration}",
                    summary.rows_returned
                ),
                Some(summary) if summary.rows_affected.is_some() => format!(
                    "{} rows affected · {duration}",
                    summary.rows_affected.unwrap_or_default()
                ),
                Some(summary) => format!("{} rows · {duration}", summary.rows_returned),
                None => format!("Done · {duration}"),
            };

            (text.into(), false)
        }
    }
}

impl Render for SqlConsole {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_postgres = self.selected_engine(cx) == Some(EngineKind::Postgres);
        let transaction = self.state.transaction();
        let is_running = self.state.is_running();
        let access = self.access;

        let transaction_label: Option<(SharedString, bool)> = match transaction {
            TransactionState::Idle => None,
            TransactionState::Active => Some(("Transaction open".into(), false)),
            TransactionState::Failed => Some(("Transaction failed — roll back".into(), true)),
            TransactionState::Unknown => Some(("Transaction state unknown".into(), true)),
        };

        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().w_56().child(
                            Select::new(&self.connection_select)
                                .id("console-target")
                                .accessibility_label("Connection")
                                .small()
                                .placeholder("Choose connection"),
                        ),
                    )
                    .when(is_postgres, |this| {
                        this.child(div().w_32().child(Input::new(&self.database_input).small()))
                    })
                    .child(
                        ButtonGroup::new("access")
                            .small()
                            .outline()
                            .child(
                                Button::new("read-only")
                                    .label("Read-only")
                                    .selected(access == Some(AccessMode::ReadOnly)),
                            )
                            .child(
                                Button::new("read-write")
                                    .label("Read-write")
                                    .selected(access == Some(AccessMode::ReadWrite)),
                            )
                            .on_click(cx.listener(|this, selected: &Vec<usize>, _, cx| {
                                match selected.first() {
                                    Some(0) => this.set_access(AccessMode::ReadOnly, cx),
                                    Some(_) => this.set_access(AccessMode::ReadWrite, cx),
                                    None => {}
                                }
                            })),
                    )
                    .child(div().flex_1())
                    .when_some(transaction_label, |this, (label, is_warning)| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(if is_warning {
                                    cx.theme().danger
                                } else {
                                    cx.theme().warning
                                })
                                .child(label),
                        )
                    })
                    .child(
                        Button::new("begin")
                            .small()
                            .ghost()
                            .label("Begin")
                            .disabled(is_running || transaction.is_open())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.transaction_command(TransactionCommand::Begin, window, cx)
                            })),
                    )
                    .child(
                        Button::new("commit")
                            .small()
                            .ghost()
                            .label("Commit")
                            .disabled(is_running || transaction != TransactionState::Active)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.transaction_command(TransactionCommand::Commit, window, cx)
                            })),
                    )
                    .child(
                        Button::new("rollback")
                            .small()
                            .ghost()
                            .label("Roll Back")
                            .disabled(is_running || !transaction.is_open())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.transaction_command(TransactionCommand::Rollback, window, cx)
                            })),
                    )
                    .when(is_running, |this| {
                        this.child(
                            Button::new("cancel")
                                .small()
                                .outline()
                                .icon(IconName::CircleX)
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(DataTable::new(&self.results).stripe(true)),
            )
            .child(
                h_flex()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(self.render_status(cx)),
            )
    }
}
