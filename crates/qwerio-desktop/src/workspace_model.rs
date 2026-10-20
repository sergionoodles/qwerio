//! The open workspace of one window: its manifest, catalog cache, object
//! metadata cache, and file watching. Filesystem and database work runs off
//! the UI thread; results update this model, which notifies its views.

mod files;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, SharedString, Task};
use qwerio_app::schema::apply_schema_change;
use qwerio_app::services::ResolvedConnection;
use qwerio_app::table_edits::save_row_edits;
use qwerio_app::workspace_store::WorkspaceStore;
use qwerio_app::{AppError, AppServices};
use qwerio_core::capabilities::DatabaseCapabilities;
use qwerio_core::catalog::{ForeignKeyInfo, ObjectDetails};
use qwerio_core::ddl::SchemaChange;
use qwerio_core::driver::DatabaseSession;
use qwerio_core::edit::{RowEditRequest, SavedRow};
use qwerio_core::ids::ConnectionId;
use qwerio_core::resource::{CatalogEntry, CatalogPath, ObjectRef};
use qwerio_workspace::explorer::{ExplorerEntry, ExplorerInput, Location, list_entries};
use qwerio_workspace::manifest::{DocumentId, Manifest};
use qwerio_workspace::paths::from_portable;
use qwerio_workspace::watch::{WorkspaceChange, WorkspaceWatcher};
use qwerio_workspace::{Workspace, WorkspaceResult};

use crate::services::Services;

/// Delay that groups bursts of file events into one refresh.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CatalogKey {
    connection: ConnectionId,
    path: CatalogPath,
}

#[derive(Clone, Debug)]
pub enum CatalogLoad<T> {
    Loading,
    Loaded(T),
    Failed(SharedString),
}

/// The state of a container listing, for empty, loading, and error views.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListingStatus {
    Ready,
    Loading,
    Failed(SharedString),
    /// The connection has no endpoint or file on this machine.
    Unbound(ConnectionId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceEvent {
    ManifestChanged,
    SaveFailed(SharedString),
    DocumentChanged(DocumentId),
    DocumentRemoved(DocumentId),
    ConnectionChanged(ConnectionId),
}

pub struct WorkspaceModel {
    services: Arc<AppServices>,
    storage: Arc<Mutex<WorkspaceStore>>,
    manifest: Arc<Manifest>,
    root: PathBuf,
    is_file_workspace: bool,
    is_encrypted: bool,
    catalogs: HashMap<CatalogKey, CatalogLoad<Arc<[CatalogEntry]>>>,
    relationships: HashMap<CatalogKey, CatalogLoad<Arc<[ForeignKeyInfo]>>>,
    details: HashMap<ObjectRef, Arc<ObjectDetails>>,
    untracked: Arc<[String]>,
    missing: HashSet<DocumentId>,
    bound: HashSet<ConnectionId>,
    _watcher: Option<WorkspaceWatcher>,
    _watch_task: Option<Task<()>>,
}

impl EventEmitter<WorkspaceEvent> for WorkspaceModel {}

/// Everything loaded off the UI thread when opening a workspace.
struct Opened {
    storage: WorkspaceStore,
    untracked: Vec<String>,
    missing: HashSet<DocumentId>,
    bound: HashSet<ConnectionId>,
}

/// Human-readable text for application errors shown in the UI.
pub fn error_text(error: &AppError) -> SharedString {
    match error {
        AppError::Database(error) => {
            let mut text = error.message.clone();

            if let Some(detail) = &error.detail {
                text.push_str(" — ");
                text.push_str(detail);
            }

            text.into()
        }
        other => other.to_string().into(),
    }
}

impl WorkspaceModel {
    pub fn open_with_password(
        root: PathBuf,
        create_name: Option<String>,
        password: Option<qwerio_core::connection::Secret>,
        cx: &mut App,
    ) -> Task<Result<Entity<Self>, SharedString>> {
        let services = Services::app(cx);
        let background_services = Arc::clone(&services);

        let load = cx.background_spawn(async move {
            let mut workspace = match create_name {
                Some(name)
                    if root
                        .extension()
                        .is_some_and(|extension| extension == "qwerio") =>
                {
                    Workspace::create_file(&root, &name, password.as_ref())
                }
                Some(name) => Workspace::create(&root, &name),
                None if root.is_file() => Workspace::open_file(&root, password.as_ref()),
                None => Workspace::open(&root),
            }
            .map_err(|error| SharedString::from(error.to_string()))?;

            let definitions = background_services
                .local_state
                .setting::<Vec<String>>(crate::settings_dialog::labels::LABELS_SETTING)
                .map_err(|error| error_text(&error))?
                .unwrap_or_default();
            for label in definitions {
                if !workspace.manifest().labels.contains(&label) {
                    workspace
                        .define_label(&label)
                        .map_err(|error| SharedString::from(error.to_string()))?;
                }
            }
            let untracked = workspace.untracked_documents().unwrap_or_default();
            let missing = missing_documents(&workspace);
            let storage = WorkspaceStore::new(workspace, background_services);
            let bound = storage
                .bound_connections()
                .map_err(|error| error_text(&error))?;

            Ok::<_, SharedString>(Opened {
                storage,
                untracked,
                missing,
                bound,
            })
        });

        cx.spawn(async move |cx| {
            let opened = load.await?;

            Ok(cx.new(|cx| {
                let root = opened.storage.workspace().root().to_path_buf();
                let manifest = Arc::new(opened.storage.workspace().manifest().clone());
                let mut model = Self {
                    services,
                    is_file_workspace: opened.storage.workspace().is_file_workspace(),
                    is_encrypted: opened.storage.workspace().is_encrypted(),
                    storage: Arc::new(Mutex::new(opened.storage)),
                    manifest,
                    root,
                    catalogs: HashMap::new(),
                    relationships: HashMap::new(),
                    details: HashMap::new(),
                    untracked: opened.untracked.into(),
                    missing: opened.missing,
                    bound: opened.bound,
                    _watcher: None,
                    _watch_task: None,
                };
                model.start_watching(cx);
                model
            }))
        })
    }

    pub fn manifest(&self) -> &Arc<Manifest> {
        &self.manifest
    }

    pub fn root(&self) -> &PathBuf {
        &self.root
    }

    pub fn services(&self) -> &Arc<AppServices> {
        &self.services
    }

    pub fn is_bound(&self, connection: ConnectionId) -> bool {
        self.bound.contains(&connection)
    }

    pub fn document_path(&self, document: DocumentId) -> Option<PathBuf> {
        let entry = self.manifest.document(document)?;
        Some(self.root.join(from_portable(&entry.path)))
    }

    /// Explorer entries at a container location, plus the listing status.
    pub fn entries(&self, location: &Location) -> (Vec<ExplorerEntry>, ListingStatus) {
        let (catalog, status) = match location {
            Location::Container { connection, path } => {
                if !self.bound.contains(connection) {
                    (None, ListingStatus::Unbound(*connection))
                } else {
                    let key = CatalogKey {
                        connection: *connection,
                        path: path.clone(),
                    };

                    match self.catalogs.get(&key) {
                        Some(CatalogLoad::Loaded(entries)) => {
                            (Some(Arc::clone(entries)), ListingStatus::Ready)
                        }
                        Some(CatalogLoad::Failed(error)) => {
                            (None, ListingStatus::Failed(error.clone()))
                        }
                        Some(CatalogLoad::Loading) | None => (None, ListingStatus::Loading),
                    }
                }
            }
            _ => (None, ListingStatus::Ready),
        };

        let unbound: HashSet<ConnectionId> = self
            .manifest
            .connections
            .iter()
            .map(|connection| connection.id)
            .filter(|connection| !self.bound.contains(connection))
            .collect();

        let entries = list_entries(&ExplorerInput {
            manifest: &self.manifest,
            location,
            catalog: catalog.as_deref(),
            untracked: &self.untracked,
            missing_documents: &self.missing,
            unbound_connections: &unbound,
        });

        let entries = entries
            .into_iter()
            .map(|mut entry| {
                let target = match &entry.target {
                    qwerio_workspace::explorer::EntryTarget::Open(location) => {
                        qwerio_workspace::labels::LabelTarget::from_location(location)
                    }
                    _ => None,
                };
                if let Some(assignment) = target.and_then(|target| {
                    self.manifest
                        .label_assignments
                        .iter()
                        .find(|assignment| assignment.target == target)
                }) {
                    let labels = assignment.labels.join(", ");
                    entry.detail =
                        Some(entry.detail.map_or_else(
                            || labels.clone(),
                            |detail| format!("{detail} · {labels}"),
                        ));
                }
                entry
            })
            .collect();
        (entries, status)
    }

    /// Loads a container's catalog once; `force` reloads it.
    pub fn ensure_catalog(
        &mut self,
        connection: ConnectionId,
        path: CatalogPath,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.bound.contains(&connection) {
            return;
        }

        let key = CatalogKey {
            connection,
            path: path.clone(),
        };

        let should_load =
            force || matches!(self.catalogs.get(&key), None | Some(CatalogLoad::Failed(_)));

        if !should_load || matches!(self.catalogs.get(&key), Some(CatalogLoad::Loading)) {
            return;
        }

        self.catalogs.insert(key.clone(), CatalogLoad::Loading);
        cx.notify();

        let session = self.browse_session(connection, path.database.clone(), cx);

        cx.spawn(async move |this, cx| {
            let result = match session.await {
                Ok(session) => session
                    .list(&path)
                    .await
                    .map_err(|error| SharedString::from(error.message)),
                Err(error) => Err(error),
            };

            this.update(cx, |this, cx| {
                let load = match result {
                    Ok(entries) => CatalogLoad::Loaded(entries.into()),
                    Err(error) => CatalogLoad::Failed(error),
                };
                this.catalogs.insert(key, load);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The foreign keys declared in a container; `Loading` until
    /// [`Self::ensure_relationships`] finishes.
    pub fn relationships(
        &self,
        connection: ConnectionId,
        path: &CatalogPath,
    ) -> CatalogLoad<Arc<[ForeignKeyInfo]>> {
        let key = CatalogKey {
            connection,
            path: path.clone(),
        };

        self.relationships
            .get(&key)
            .cloned()
            .unwrap_or(CatalogLoad::Loading)
    }

    /// Loads a container's foreign keys once; `force` reloads them.
    pub fn ensure_relationships(
        &mut self,
        connection: ConnectionId,
        path: CatalogPath,
        force: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.bound.contains(&connection) {
            return;
        }

        let key = CatalogKey {
            connection,
            path: path.clone(),
        };
        let current = self.relationships.get(&key);
        let should_load = force || matches!(current, None | Some(CatalogLoad::Failed(_)));

        if !should_load || matches!(current, Some(CatalogLoad::Loading)) {
            return;
        }

        self.relationships.insert(key.clone(), CatalogLoad::Loading);
        cx.notify();

        let session = self.browse_session(connection, path.database.clone(), cx);

        cx.spawn(async move |this, cx| {
            let result = match session.await {
                Ok(session) => session
                    .relationships(&path)
                    .await
                    .map_err(|error| SharedString::from(error.message)),
                Err(error) => Err(error),
            };

            this.update(cx, |this, cx| {
                let load = match result {
                    Ok(keys) => CatalogLoad::Loaded(keys.into()),
                    Err(error) => CatalogLoad::Failed(error),
                };
                this.relationships.insert(key, load);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Drops cached catalog listings and metadata for a connection.
    pub fn forget_connection_cache(&mut self, connection: ConnectionId, cx: &mut Context<Self>) {
        self.catalogs.retain(|key, _| key.connection != connection);
        self.relationships
            .retain(|key, _| key.connection != connection);
        self.details
            .retain(|object, _| object.connection != connection);
        cx.notify();
    }

    /// Resolves a connection's descriptor and credentials off the UI thread.
    pub fn resolve(
        &self,
        connection: ConnectionId,
        cx: &App,
    ) -> Task<Result<ResolvedConnection, SharedString>> {
        let storage = Arc::clone(&self.storage);
        cx.background_spawn(async move {
            storage
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .resolve_connection(connection)
                .map_err(|error| error_text(&error))
        })
    }

    /// The shared read-only browsing session for a connection and database.
    pub fn browse_session(
        &self,
        connection: ConnectionId,
        database: Option<String>,
        cx: &App,
    ) -> Task<Result<Arc<dyn DatabaseSession>, SharedString>> {
        let resolve = self.resolve(connection, cx);
        let sessions = Arc::clone(&self.services.sessions);

        cx.spawn(async move |_| {
            let resolved = resolve.await?;
            sessions
                .browse(&resolved.descriptor, &resolved.credentials, database)
                .await
                .map_err(|error| SharedString::from(error.message))
        })
    }

    /// Object metadata, cached until refreshed.
    pub fn describe(
        &mut self,
        object: ObjectRef,
        force: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<Arc<ObjectDetails>, SharedString>> {
        if !force && let Some(details) = self.details.get(&object) {
            return Task::ready(Ok(Arc::clone(details)));
        }

        let session = self.browse_session(object.connection, object.database.clone(), cx);

        cx.spawn(async move |this, cx| {
            let session = session.await?;
            let details = session
                .describe(&object)
                .await
                .map(Arc::new)
                .map_err(|error| SharedString::from(error.message))?;

            this.update(cx, |this, _| {
                this.details.insert(object, Arc::clone(&details));
            })
            .ok();

            Ok(details)
        })
    }

    /// What a bound connection's engine supports; `None` when the connection
    /// is not set up on this computer.
    pub fn capabilities(&self, connection: ConnectionId) -> Option<&DatabaseCapabilities> {
        if !self.bound.contains(&connection) {
            return None;
        }

        let engine = self.manifest.connection(connection)?.engine;
        self.services.drivers.capabilities(engine)
    }

    /// Runs a schema change on its own writable session, then reloads the
    /// listing or metadata it changed.
    pub fn apply_schema_change(
        &self,
        connection: ConnectionId,
        change: SchemaChange,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), SharedString>> {
        let resolve = self.resolve(connection, cx);
        let sessions = Arc::clone(&self.services.sessions);

        cx.spawn(async move |this, cx| {
            let resolved = resolve.await?;
            apply_schema_change(&sessions, resolved, &change)
                .await
                .map_err(|error| error_text(&AppError::Database(error)))?;

            this.update(cx, |this, cx| {
                this.reload_changed_schema(connection, &change, cx)
            })
            .ok();

            Ok(())
        })
    }

    /// Saves staged row changes on their own writable session and returns the
    /// touched rows in request order.
    pub fn save_row_edits(
        &self,
        request: RowEditRequest,
        cx: &App,
    ) -> Task<Result<Vec<SavedRow>, SharedString>> {
        let resolve = self.resolve(request.details.object.connection, cx);
        let sessions = Arc::clone(&self.services.sessions);

        cx.spawn(async move |_| {
            let resolved = resolve.await?;
            save_row_edits(&sessions, resolved, request)
                .await
                .map_err(|error| error_text(&AppError::Database(error)))
        })
    }

    fn reload_changed_schema(
        &mut self,
        connection: ConnectionId,
        change: &SchemaChange,
        cx: &mut Context<Self>,
    ) {
        match change {
            SchemaChange::CreateDatabase { .. } => {
                self.ensure_catalog(connection, CatalogPath::root(), true, cx)
            }
            SchemaChange::CreateTable { container, .. } => {
                self.ensure_catalog(connection, container.clone(), true, cx)
            }
            SchemaChange::AddColumn { table, .. } => {
                self.details.remove(table);
                cx.notify();
            }
        }
    }

    /// Runs a workspace operation off the UI thread, then refreshes the
    /// manifest snapshot and notifies views.
    pub fn update_workspace<T, F>(
        &mut self,
        cx: &mut Context<Self>,
        operation: F,
    ) -> Task<Result<T, SharedString>>
    where
        T: Send + 'static,
        F: FnOnce(&mut Workspace) -> WorkspaceResult<T> + Send + 'static,
    {
        let storage = Arc::clone(&self.storage);

        let work = cx.background_spawn(async move {
            let mut storage = storage.lock().unwrap_or_else(PoisonError::into_inner);
            let output = storage
                .update(operation)
                .map_err(|error| SharedString::from(error.to_string()))?;
            let workspace = storage.workspace();
            let manifest = workspace.manifest().clone();
            let untracked = workspace.untracked_documents().unwrap_or_default();
            let missing = missing_documents(workspace);
            Ok::<_, SharedString>((output, manifest, untracked, missing))
        });

        cx.spawn(async move |this, cx| {
            let (output, manifest, untracked, missing) = match work.await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    this.update(cx, |_, cx| {
                        cx.emit(WorkspaceEvent::SaveFailed(error.clone()))
                    })
                    .ok();
                    return Err(error);
                }
            };

            this.update(cx, |this, cx| {
                this.apply_snapshot(manifest, untracked, missing, cx);
            })
            .ok();

            Ok(output)
        })
    }

    fn apply_snapshot(
        &mut self,
        manifest: Manifest,
        untracked: Vec<String>,
        missing: HashSet<DocumentId>,
        cx: &mut Context<Self>,
    ) {
        self.manifest = Arc::new(manifest);
        self.untracked = untracked.into();
        self.missing = missing;
        cx.emit(WorkspaceEvent::ManifestChanged);
        cx.notify();
    }

    /// Records that a connection was bound (or unbound) on this machine.
    pub fn set_bound(&mut self, connection: ConnectionId, bound: bool, cx: &mut Context<Self>) {
        if bound {
            self.bound.insert(connection);
        } else {
            self.bound.remove(&connection);
        }

        self.forget_connection_cache(connection, cx);
        cx.emit(WorkspaceEvent::ConnectionChanged(connection));
    }

    fn start_watching(&mut self, cx: &mut Context<Self>) {
        if self.is_file_workspace {
            return;
        }
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let watcher = WorkspaceWatcher::start(&self.root, move |change| {
            // The receiver only disappears when the window closes.
            let _ = sender.send(change);
        });

        let Ok(watcher) = watcher else {
            return;
        };

        self._watcher = Some(watcher);
        self._watch_task = Some(cx.spawn(async move |this, cx| {
            while let Some(first) = receiver.recv().await {
                cx.background_executor().timer(WATCH_DEBOUNCE).await;
                let mut changes = vec![first];

                while let Ok(change) = receiver.try_recv() {
                    changes.push(change);
                }

                let applied = this.update(cx, |this, cx| this.apply_changes(changes, cx));

                if applied.is_err() {
                    break;
                }
            }
        }));
    }

    fn apply_changes(&mut self, changes: Vec<WorkspaceChange>, cx: &mut Context<Self>) {
        let mut rescan = false;

        for change in changes {
            match change {
                WorkspaceChange::ManifestChanged | WorkspaceChange::Rescan => rescan = true,
                WorkspaceChange::DocumentChanged(path) => match self.document_for_path(&path) {
                    Some(document) => {
                        self.missing.remove(&document);
                        cx.emit(WorkspaceEvent::DocumentChanged(document));
                    }
                    None => rescan = true,
                },
                WorkspaceChange::DocumentRemoved(path) => match self.document_for_path(&path) {
                    Some(document) => {
                        self.missing.insert(document);
                        cx.emit(WorkspaceEvent::DocumentRemoved(document));
                    }
                    None => rescan = true,
                },
            }
        }

        if rescan {
            self.reload_from_disk(cx);
        }

        cx.notify();
    }

    fn document_for_path(&self, relative: &std::path::Path) -> Option<DocumentId> {
        let portable = qwerio_workspace::paths::portable(relative);
        self.manifest
            .document_by_path(&portable)
            .map(|entry| entry.id)
    }

    /// Re-reads the manifest (changed by another program, or by our own
    /// save) and rescans untracked documents.
    fn reload_from_disk(&mut self, cx: &mut Context<Self>) {
        let task = self.update_workspace(cx, |workspace| workspace.reload_manifest());

        cx.spawn(async move |_, _| {
            // A manifest that cannot be parsed keeps the last good snapshot;
            // saving later reports the conflict to the user.
            let _ = task.await;
        })
        .detach();
    }
}

fn missing_documents(workspace: &Workspace) -> HashSet<DocumentId> {
    workspace
        .manifest()
        .documents
        .iter()
        .filter(|document| {
            !workspace.is_file_workspace()
                && workspace
                    .document_path(document.id)
                    .map_or(true, |path| !path.is_file())
        })
        .map(|document| document.id)
        .collect()
}
