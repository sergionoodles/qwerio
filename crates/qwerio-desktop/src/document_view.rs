//! Editing one document: SQL (with its console), Markdown (with preview), or
//! plain text. Saves are atomic and refuse to overwrite external changes;
//! unsaved edits are kept as drafts in local state until saved or discarded.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use qwerio_app::local_state::Draft;
use qwerio_app::navigation::ViewState;
use qwerio_core::resource::ObjectRef;
use qwerio_workspace::WorkspaceError;
use qwerio_workspace::document::{
    DiskState, DocumentKind, Fingerprint, SaveExpectation, disk_state, read_document, save_document,
};
use qwerio_workspace::explorer::{Location, ScratchId};
use qwerio_workspace::manifest::{DocumentId, ExecutionBinding};

use crate::actions::{
    BeginTransaction, CancelStatement, CommitTransaction, RollbackTransaction, RunStatement,
    SQL_DOCUMENT_CONTEXT, Save, TogglePreview,
};
use crate::content::ContentEvent;
use crate::sql_console::{ConsoleEvent, SqlConsole, TransactionCommand};
use crate::workspace_model::WorkspaceModel;

/// Pause after the last edit before the draft is written to local state.
const DRAFT_DELAY: Duration = Duration::from_millis(1500);

#[derive(Clone, Debug, PartialEq)]
enum LoadState {
    Loading,
    Ready,
    Failed(SharedString),
}

#[derive(Clone, Debug, PartialEq)]
enum Conflict {
    /// The file changed on disk while there were unsaved edits.
    ChangedOnDisk,
    /// The file was removed or moved by another program.
    DeletedOnDisk,
    /// A draft from an earlier session was based on another version.
    StaleDraft,
}

/// What a document view edits: a saved workspace document or a temporary
/// query that exists only in this session until saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentSource {
    Saved(DocumentId),
    Scratch(ScratchId),
}

impl DocumentSource {
    fn key(self) -> String {
        match self {
            Self::Saved(document) => format!("document-{document}"),
            Self::Scratch(scratch) => format!("scratch-{scratch:?}"),
        }
    }
}

pub struct DocumentView {
    model: Entity<WorkspaceModel>,
    source: DocumentSource,
    kind: DocumentKind,
    editor: Entity<EditorState>,
    console: Option<Entity<SqlConsole>>,
    load: LoadState,
    saved_text: SharedString,
    fingerprint: Option<Fingerprint>,
    is_dirty: bool,
    is_saving: bool,
    conflict: Option<Conflict>,
    is_previewing: bool,
    draft_task: Option<Task<()>>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ContentEvent> for DocumentView {}

impl DocumentView {
    /// `suggested_target` pre-selects a connection for temporary queries.
    pub fn new(
        model: Entity<WorkspaceModel>,
        source: DocumentSource,
        suggested_target: Option<ExecutionBinding>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let kind = match source {
            DocumentSource::Saved(document) => model
                .read(cx)
                .document_path(document)
                .as_deref()
                .and_then(DocumentKind::from_path)
                .unwrap_or(DocumentKind::Text),
            DocumentSource::Scratch(_) => DocumentKind::Sql,
        };
        let saved_document = match source {
            DocumentSource::Saved(document) => Some(document),
            DocumentSource::Scratch(_) => None,
        };

        let editor = cx.new(|cx| {
            let editor = EditorState::new(window, cx)
                .line_number(kind == DocumentKind::Sql)
                .soft_wrap(kind != DocumentKind::Sql);

            match kind {
                DocumentKind::Sql => editor.language("sql"),
                DocumentKind::Markdown => editor.language("markdown"),
                DocumentKind::Text => editor,
            }
        });

        let console = (kind == DocumentKind::Sql).then(|| {
            cx.new(|cx| {
                SqlConsole::new(model.clone(), saved_document, suggested_target, window, cx)
            })
        });

        let mut subscriptions = vec![
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.on_edit(cx);
                }
            }),
        ];

        if let Some(console) = &console {
            subscriptions.push(cx.subscribe_in(
                console,
                window,
                |this, _, event: &ConsoleEvent, window, cx| match event {
                    ConsoleEvent::SelectRange(range) => {
                        let range = range.clone();
                        this.editor.update(cx, |editor, cx| {
                            editor.set_selected_range(range, cx);
                            editor.focus(window, cx);
                        });
                    }
                },
            ));
            subscriptions.push(cx.subscribe(console, |_, _, event: &ContentEvent, cx| {
                cx.emit(event.clone())
            }));
        }

        let mut view = Self {
            model,
            source,
            kind,
            editor,
            console,
            load: LoadState::Loading,
            saved_text: SharedString::default(),
            fingerprint: None,
            is_dirty: false,
            is_saving: false,
            conflict: None,
            is_previewing: false,
            draft_task: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };

        view.load(true, window, cx);
        view
    }

    pub fn location(&self) -> Location {
        match self.source {
            DocumentSource::Saved(document) => Location::Document(document),
            DocumentSource::Scratch(scratch) => Location::Scratch(scratch),
        }
    }

    fn saved_document(&self) -> Option<DocumentId> {
        match self.source {
            DocumentSource::Saved(document) => Some(document),
            DocumentSource::Scratch(_) => None,
        }
    }

    pub fn title(&self, cx: &App) -> SharedString {
        let name = match self.source {
            DocumentSource::Saved(document) => self
                .model
                .read(cx)
                .manifest()
                .document(document)
                .map(|entry| entry.file_name().to_owned())
                .unwrap_or_else(|| "Document".to_owned()),
            DocumentSource::Scratch(_) => "Untitled query".to_owned(),
        };

        if self.is_dirty {
            format!("{name} •").into()
        } else {
            name.into()
        }
    }

    pub fn status_text(&self, cx: &App) -> SharedString {
        let modified = if self.is_dirty { "Modified" } else { "Saved" };

        match &self.console {
            Some(console) => format!("{modified} · {}", console.read(cx).status_text(cx)).into(),
            None => format!("{modified} · {}", self.kind.display_name()).into(),
        }
    }

    pub fn needs_confirmation_to_close(&self, cx: &App) -> bool {
        self.console
            .as_ref()
            .is_some_and(|console| console.read(cx).needs_confirmation_to_close())
    }

    /// Loads the file. On first load, an unsaved draft from an earlier
    /// session is restored.
    fn load(&mut self, restore_draft: bool, window: &mut Window, cx: &mut Context<Self>) {
        let DocumentSource::Saved(document) = self.source else {
            self.load = LoadState::Ready;
            return;
        };

        let Some(path) = self.model.read(cx).document_path(document) else {
            self.load = LoadState::Failed("This document is no longer in the workspace.".into());
            return;
        };

        let model = self.model.read(cx);
        let local_state = Arc::clone(&model.services().local_state);
        let workspace = model.manifest().id;

        let read = cx.background_spawn(async move {
            let loaded =
                read_document(&path).map_err(|error| SharedString::from(error.to_string()))?;
            let draft = if restore_draft {
                local_state
                    .drafts(workspace)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|draft| draft.document == document)
            } else {
                None
            };

            Ok::<_, SharedString>((loaded, draft))
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = read.await;

            this.update_in(cx, |this, window, cx| {
                match result {
                    Ok((loaded, draft)) => {
                        this.saved_text = loaded.text.clone().into();
                        this.fingerprint = Some(loaded.fingerprint);
                        this.load = LoadState::Ready;

                        let text = match &draft {
                            Some(draft) if draft.text != loaded.text => {
                                this.is_dirty = true;

                                if draft.base == Some(loaded.fingerprint) {
                                    window.push_notification(
                                        Notification::info(
                                            "Restored unsaved changes from your last session",
                                        ),
                                        cx,
                                    );
                                } else {
                                    this.conflict = Some(Conflict::StaleDraft);
                                }

                                draft.text.clone()
                            }
                            _ => {
                                this.is_dirty = false;
                                this.conflict = None;
                                loaded.text
                            }
                        };

                        this.editor
                            .update(cx, |editor, cx| editor.set_value(text, window, cx));

                        // Focus landed on the loading placeholder; hand it to the editor.
                        if this.focus_handle.is_focused(window) {
                            let editor = this.editor.focus_handle(cx);
                            window.focus(&editor, cx);
                        }
                    }
                    Err(error) => this.load = LoadState::Failed(error),
                }

                cx.emit(ContentEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn on_edit(&mut self, cx: &mut Context<Self>) {
        let text = self.editor.read(cx).value();
        let is_dirty = text != self.saved_text;

        if is_dirty != self.is_dirty {
            self.is_dirty = is_dirty;
            cx.emit(ContentEvent::Changed);
        }

        self.schedule_draft(cx);
        cx.notify();
    }

    /// Keeps unsaved text in local state so it survives crashes and restarts.
    fn schedule_draft(&mut self, cx: &mut Context<Self>) {
        // Temporary queries are not kept across sessions.
        let Some(document) = self.saved_document() else {
            return;
        };

        let model = self.model.read(cx);
        let local_state = Arc::clone(&model.services().local_state);
        let workspace = model.manifest().id;

        self.draft_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_DELAY).await;

            let Ok((text, base, is_dirty)) = this.update(cx, |this, cx| {
                (
                    this.editor.read(cx).value().to_string(),
                    this.fingerprint,
                    this.is_dirty,
                )
            }) else {
                return;
            };

            cx.background_spawn(async move {
                // Drafts are a safety net; failing to store one must not
                // interrupt editing.
                let _ = if is_dirty {
                    local_state.save_draft(
                        workspace,
                        &Draft {
                            document,
                            text,
                            base,
                        },
                        SystemTime::now(),
                    )
                } else {
                    local_state.discard_draft(workspace, document)
                };
            })
            .await;
        }));
    }

    pub fn save(
        &mut self,
        expectation: Option<SaveExpectation>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.is_saving || self.load != LoadState::Ready {
            return;
        }

        let document = match self.source {
            DocumentSource::Saved(document) => document,
            DocumentSource::Scratch(scratch) => {
                if self.needs_confirmation_to_close(cx) {
                    window.push_notification(
                        Notification::warning(
                            "Commit or roll back the open transaction before saving the query.",
                        ),
                        cx,
                    );
                    return;
                }

                let text = self.editor.read(cx).value().to_string();
                cx.emit(ContentEvent::SaveScratch { scratch, text });
                return;
            }
        };

        let Some(path) = self.model.read(cx).document_path(document) else {
            return;
        };

        let expectation = expectation.unwrap_or(match self.fingerprint {
            Some(fingerprint) => SaveExpectation::Unchanged(fingerprint),
            None => SaveExpectation::New,
        });
        let text = self.editor.read(cx).value();
        let model = self.model.read(cx);
        let local_state = Arc::clone(&model.services().local_state);
        let workspace = model.manifest().id;
        let saved_text = text.clone();

        self.is_saving = true;
        cx.notify();

        let save = cx.background_spawn(async move {
            let fingerprint = save_document(&path, &text, expectation)?;
            // The draft is obsolete once the file holds the text.
            let _ = local_state.discard_draft(workspace, document);
            Ok::<_, WorkspaceError>(fingerprint)
        });

        cx.spawn_in(window, async move |this, cx| {
            let result = save.await;

            this.update_in(cx, |this, window, cx| {
                this.is_saving = false;

                match result {
                    Ok(fingerprint) => {
                        this.fingerprint = Some(fingerprint);
                        this.saved_text = saved_text;
                        this.is_dirty = this.editor.read(cx).value() != this.saved_text;
                        this.conflict = None;
                        this.draft_task = None;
                    }
                    Err(WorkspaceError::Conflict {
                        on_disk: Some(_), ..
                    }) => this.conflict = Some(Conflict::ChangedOnDisk),
                    Err(WorkspaceError::Conflict { on_disk: None, .. }) => {
                        this.conflict = Some(Conflict::DeletedOnDisk)
                    }
                    Err(error) => {
                        window.push_notification(
                            Notification::error(format!("Could not save: {error}")),
                            cx,
                        );
                    }
                }

                cx.emit(ContentEvent::Changed);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Called when the watcher reports a change to this document's file.
    pub fn file_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(document) = self.saved_document() else {
            return;
        };

        let (Some(path), Some(fingerprint)) = (
            self.model.read(cx).document_path(document),
            self.fingerprint,
        ) else {
            return;
        };

        let check = cx.background_spawn(async move { disk_state(&path, fingerprint) });

        cx.spawn_in(window, async move |this, cx| {
            let state = check.await;

            this.update_in(cx, |this, window, cx| match state {
                Ok(DiskState::Unchanged) => {}
                Ok(DiskState::Modified(_)) if !this.is_dirty => this.load(false, window, cx),
                Ok(DiskState::Modified(_)) => {
                    this.conflict = Some(Conflict::ChangedOnDisk);
                    cx.notify();
                }
                Ok(DiskState::Deleted) => {
                    this.conflict = Some(Conflict::DeletedOnDisk);
                    cx.notify();
                }
                Err(error) => {
                    window.push_notification(Notification::warning(error.to_string()), cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn reload_from_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(document) = self.saved_document() else {
            return;
        };

        self.conflict = None;
        self.is_dirty = false;
        let model = self.model.read(cx);
        let local_state = Arc::clone(&model.services().local_state);
        let workspace = model.manifest().id;

        cx.background_spawn(async move {
            let _ = local_state.discard_draft(workspace, document);
        })
        .detach();

        self.load(false, window, cx);
    }

    fn keep_draft(&mut self, cx: &mut Context<Self>) {
        self.conflict = None;
        cx.notify();
    }

    fn run_statement(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(console) = self.console.clone() else {
            return;
        };

        let editor = self.editor.read(cx);
        let range = editor.selected_range();
        let (sql, start) = if range.is_empty() {
            (editor.value().to_string(), 0)
        } else {
            (editor.selected_text().to_string(), range.start)
        };

        console.update(cx, |console, cx| console.run(sql, start, window, cx));
    }

    fn transaction(
        &mut self,
        command: TransactionCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(console) = &self.console {
            console.update(cx, |console, cx| {
                console.transaction_command(command, window, cx)
            });
        }
    }

    pub fn shut_down(&mut self, cx: &mut Context<Self>) {
        if let Some(console) = &self.console {
            console.update(cx, |console, cx| console.shut_down(cx));
        }
    }

    fn render_conflict(&self, conflict: &Conflict, cx: &mut Context<Self>) -> impl IntoElement {
        let (message, actions): (&str, Vec<Button>) = match conflict {
            Conflict::ChangedOnDisk => (
                "This file was changed by another program. Saving would overwrite those changes.",
                vec![
                    Button::new("reload")
                        .small()
                        .outline()
                        .label("Discard My Changes")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.reload_from_disk(window, cx)),
                        ),
                    Button::new("overwrite")
                        .small()
                        .danger()
                        .label("Overwrite")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.save(Some(SaveExpectation::Overwrite), window, cx)
                        })),
                ],
            ),
            Conflict::DeletedOnDisk => (
                "This file was moved or deleted outside Qwerio.",
                vec![
                    Button::new("recreate")
                        .small()
                        .outline()
                        .label("Save to Recreate")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.save(Some(SaveExpectation::Overwrite), window, cx)
                        })),
                    Button::new("remove")
                        .small()
                        .ghost()
                        .label("Remove from Workspace")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(document) = this.saved_document() {
                                cx.emit(ContentEvent::RemoveDocument {
                                    document,
                                    delete_file: false,
                                });
                            }
                        })),
                ],
            ),
            Conflict::StaleDraft => (
                "Unsaved changes from your last session were made to an older version of this file.",
                vec![
                    Button::new("keep-draft")
                        .small()
                        .outline()
                        .label("Keep My Changes")
                        .on_click(cx.listener(|this, _, _, cx| this.keep_draft(cx))),
                    Button::new("discard-draft")
                        .small()
                        .ghost()
                        .label("Use the File")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.reload_from_disk(window, cx)),
                        ),
                ],
            ),
        };

        h_flex()
            .flex_wrap()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().warning.opacity(0.12))
            .child(div().flex_1().min_w_48().text_sm().child(message))
            .children(actions)
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_running = self
            .console
            .as_ref()
            .is_some_and(|console| console.read(cx).is_running());

        h_flex()
            .px_2()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(self.kind == DocumentKind::Sql, |this| {
                this.child(
                    Button::new("run")
                        .small()
                        .primary()
                        .icon(IconName::Play)
                        .label("Run")
                        .loading(is_running)
                        .tooltip(
                            "Run the selection, or the whole document when nothing is selected",
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.run_statement(window, cx)),
                        ),
                )
            })
            .when(self.kind == DocumentKind::Markdown, |this| {
                this.child(
                    Button::new("preview")
                        .small()
                        .ghost()
                        .icon(IconName::Eye)
                        .label("Preview")
                        .toggled(self.is_previewing)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.is_previewing = !this.is_previewing;
                            cx.notify();
                        })),
                )
            })
            .children(self.render_links_menu(cx))
            .child(div().flex_1())
            .child(
                Button::new("save")
                    .small()
                    .ghost()
                    .label(if self.is_dirty { "Save" } else { "Saved" })
                    .loading(self.is_saving)
                    .on_click(cx.listener(|this, _, window, cx| this.save(None, window, cx))),
            )
    }

    /// Tables this document is linked to: open one, or remove a link (also
    /// how links to dropped tables are repaired).
    fn render_links_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let document = self.saved_document()?;
        let links: Vec<ObjectRef> = self
            .model
            .read(cx)
            .manifest()
            .document(document)
            .map(|entry| entry.associations.clone())
            .unwrap_or_default();
        let this = cx.entity().downgrade();
        let label = if links.is_empty() {
            "No Linked Tables".to_owned()
        } else {
            format!("Linked Tables ({})", links.len())
        };

        Some(
            Button::new("links")
                .small()
                .ghost()
                .icon(Icon::new(gpui_kit::assets::IconName::Link))
                .label(label)
                .dropdown_caret(true)
                .dropdown_menu(move |menu, _, _| {
                    let open_items = links.iter().fold(menu, |menu, object| {
                        let this = this.clone();
                        let location = Location::Object(object.clone());
                        menu.item(PopupMenuItem::new(object.qualified_name()).on_click(
                            move |_, _, cx| {
                                let location = location.clone();
                                this.update(cx, |_, cx| {
                                    cx.emit(ContentEvent::Navigate {
                                        location,
                                        view: ViewState::default(),
                                        new_tab: false,
                                    })
                                })
                                .ok();
                            },
                        ))
                    });

                    let with_separator = if links.is_empty() {
                        open_items.item(PopupMenuItem::label(
                            "Link tables from the explorer’s table menu",
                        ))
                    } else {
                        open_items.separator()
                    };

                    links.iter().fold(with_separator, |menu, object| {
                        let this = this.clone();
                        let object = object.clone();
                        menu.item(
                            PopupMenuItem::new(format!("Unlink {}", object.name)).on_click(
                                move |_, _, cx| {
                                    let object = object.clone();
                                    this.update(cx, |_, cx| {
                                        cx.emit(ContentEvent::Dissociate { document, object })
                                    })
                                    .ok();
                                },
                            ),
                        )
                    })
                }),
        )
    }

    fn render_body(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        match &self.load {
            LoadState::Loading => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(Spinner::new())
                .into_any_element(),
            LoadState::Failed(error) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .p_6()
                .child(
                    div()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child("The document could not be opened"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(error.clone()),
                )
                .into_any_element(),
            LoadState::Ready if self.is_previewing => div()
                .id("markdown-preview")
                .size_full()
                .overflow_y_scroll()
                .p_4()
                .child(TextView::markdown(
                    SharedString::from(format!("preview-{}", self.source.key())),
                    self.editor.read(cx).value(),
                ))
                .into_any_element(),
            LoadState::Ready => {
                let editor = Editor::new(&self.editor).size_full().bordered(false);

                match &self.console {
                    Some(console) => v_resizable(SharedString::from(format!(
                        "sql-split-{}",
                        self.source.key()
                    )))
                    .child(resizable_panel().child(editor))
                    .child(
                        resizable_panel()
                            .size(px(280.))
                            .size_range(px(120.)..px(2000.))
                            .child(console.clone()),
                    )
                    .into_any_element(),
                    None => editor.into_any_element(),
                }
            }
        }
    }
}

impl Focusable for DocumentView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.load == LoadState::Ready {
            return self.editor.read(cx).focus_handle(cx);
        }

        self.focus_handle.clone()
    }
}

impl Render for DocumentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("document-view")
            .key_context(SQL_DOCUMENT_CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .min_w_0()
            .min_h_0()
            .on_action(cx.listener(|this, _: &Save, window, cx| this.save(None, window, cx)))
            .on_action(
                cx.listener(|this, _: &RunStatement, window, cx| this.run_statement(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CancelStatement, _, cx| {
                if let Some(console) = &this.console {
                    console.update(cx, |console, cx| console.cancel(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &BeginTransaction, window, cx| {
                this.transaction(TransactionCommand::Begin, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CommitTransaction, window, cx| {
                this.transaction(TransactionCommand::Commit, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RollbackTransaction, window, cx| {
                this.transaction(TransactionCommand::Rollback, window, cx)
            }))
            .on_action(cx.listener(|this, _: &TogglePreview, _, cx| {
                if this.kind == DocumentKind::Markdown {
                    this.is_previewing = !this.is_previewing;
                    cx.notify();
                }
            }))
            .child(self.render_toolbar(cx))
            .when_some(self.conflict.clone(), |this, conflict| {
                this.child(self.render_conflict(&conflict, cx))
            })
            .child(div().flex_1().min_h_0().child(self.render_body(cx)))
    }
}
