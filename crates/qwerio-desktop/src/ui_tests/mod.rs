//! Headless UI tests: the real workbench in a GPUI test window, driven by
//! clicks and keystrokes, against a temporary workspace and SQLite database.

mod about_flows;
mod connection_flows;
mod console_flows;
mod document_flows;
mod edit_flows;
mod explorer_flows;
#[cfg(not(target_os = "macos"))]
mod menu_flows;
mod row_flows;
mod schema_flows;
mod settings_flows;
mod workspace_files;
mod workspace_flows;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, ElementId, Entity, Point, SharedString,
    TestAppContext, Window, WindowBounds, WindowOptions, px, size,
};
use qwerio_app::AppServices;
use qwerio_app::credentials::MemoryCredentialStore;
use qwerio_app::local_state::LocalState;
use qwerio_app::registry::DriverRegistry;
use qwerio_core::connection::{ConnectionOptions, EngineKind, SqliteOptions};
use qwerio_sqlite::SqliteDriver;
use qwerio_workspace::Workspace;
use qwerio_workspace::manifest::Placement;
use tempfile::TempDir;

use crate::services::Services;
use crate::workbench::Workbench;

/// Real time allowed for database and file work started by the UI.
const UI_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(5);

pub const CONNECTION_NAME: &str = "Shop data";
pub const NOTE_FILE: &str = "notes/readme.md";
pub const NOTE_TEXT: &str = "# Readme\n";

/// A workspace with one bound SQLite connection and one tracked note.
pub struct Fixture {
    _directory: TempDir,
    pub root: PathBuf,
    pub database: PathBuf,
    pub services: Arc<AppServices>,
    pub connection_key: SharedString,
    pub note_key: SharedString,
}

impl Fixture {
    pub fn new() -> Self {
        let directory = TempDir::new().expect("temporary directory");
        let database = directory.path().join("shop.sqlite");
        seed_database(&database);

        let root = directory.path().join("workspace");
        std::fs::create_dir(&root).expect("workspace folder");

        let services = Arc::new(test_services());
        services
            .local_state
            .set_setting(
                "default_workspace_path",
                &directory.path().join("Default.qwerio"),
            )
            .expect("default path");
        let mut workspace = Workspace::create(&root, "Shop").expect("workspace");
        let connection = workspace
            .add_connection(CONNECTION_NAME, EngineKind::Sqlite)
            .expect("connection alias");
        services
            .bind_connection(
                workspace.manifest(),
                connection,
                ConnectionOptions::Sqlite(SqliteOptions::new(&database)),
                None,
            )
            .expect("bind connection");

        let (note, _) = workspace
            .create_document(Path::new(NOTE_FILE), Placement::Root, NOTE_TEXT)
            .expect("note");

        Self {
            _directory: directory,
            root,
            database,
            services,
            connection_key: format!("connection:{connection}").into(),
            note_key: format!("document:{note}").into(),
        }
    }

    /// Records the workspace as the most recently opened one.
    pub fn record_recent(&self) {
        self.services
            .local_state
            .record_recent_workspace(&self.root, "Shop", SystemTime::now())
            .expect("recent workspace");
    }
}

fn seed_database(path: &Path) {
    qwerio_sqlite::create_database_file(path).expect("create database");
    rusqlite::Connection::open(path)
        .and_then(|connection| {
            connection.execute_batch(
                "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
                 CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER REFERENCES customers (id), total NUMERIC);
                 INSERT INTO customers VALUES (1, 'Ada'), (2, 'Grace'), (3, 'Linus');
                 INSERT INTO orders VALUES (1, 1, 10.5), (2, 2, 7), (3, 1, 3.25);",
            )
        })
        .expect("seed database");
}

fn test_services() -> AppServices {
    let mut drivers = DriverRegistry::new();
    drivers.register(Arc::new(SqliteDriver::new()));
    static NETWORK_RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> =
        std::sync::OnceLock::new();
    let runtime = NETWORK_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("network runtime")
    });
    drivers.register(Arc::new(qwerio_postgres::PostgresDriver::new(
        runtime.handle().clone(),
    )));
    drivers.register(Arc::new(qwerio_mysql::MysqlDriver::new(
        runtime.handle().clone(),
    )));
    drivers.register(Arc::new(qwerio_mysql::MysqlDriver::mariadb(
        runtime.handle().clone(),
    )));

    AppServices::new(
        drivers,
        LocalState::in_memory().expect("local state"),
        Arc::new(MemoryCredentialStore::default()),
    )
}

/// Initializes the app the way `main` does, with test services.
pub fn init_app(services: Arc<AppServices>, cx: &mut TestAppContext) {
    // SQLite, file watching, and local state run on real threads, which the
    // deterministic scheduler otherwise rejects.
    cx.executor().allow_parking();

    cx.update(|cx| {
        gpui_kit::init(cx);
        // Dialogs then appear at their final position on the first frame,
        // so clicks never land on a surface that is still sliding in.
        cx.set_reduce_motion(true);
        crate::theme::init(cx);
        crate::actions::bind_keys(cx);
        crate::explorer_view::bind_keys(cx);
        cx.set_global(Services { app: services });
    });
}

/// Opens a workbench window at a fixed desktop size.
pub fn open_workbench(
    workspace: Option<PathBuf>,
    cx: &mut TestAppContext,
) -> (AnyWindowHandle, Entity<Workbench>) {
    cx.update(|cx| {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds {
                origin: Point::default(),
                size: size(px(1280.), px(800.)),
            })),
            ..WindowOptions::default()
        };

        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| Workbench::new(workspace, window, cx))
        })
        .expect("open test window")
    })
}

/// Renders frames until `predicate` holds. Database and file work runs on
/// real threads, so this waits in real time rather than on the test clock.
pub fn wait_until(
    handle: AnyWindowHandle,
    cx: &mut TestAppContext,
    description: &str,
    mut predicate: impl FnMut(&mut Window, &mut App) -> bool,
) {
    let deadline = Instant::now() + UI_TIMEOUT;

    loop {
        cx.run_until_parked();

        let is_ready = cx
            .update_window(handle, |_, window, cx| {
                gpui_kit::test::TestWindowExt::render_frame(window, cx);
                predicate(window, cx)
            })
            .expect("test window is open");

        if is_ready {
            return;
        }

        assert!(
            Instant::now() < deadline,
            "timed out after {UI_TIMEOUT:?} waiting until {description}"
        );
        std::thread::sleep(POLL_INTERVAL);
        // Keeps animations and debounce timers moving with real time.
        cx.executor().advance_clock(POLL_INTERVAL);
    }
}

/// The accessibility label of `id`, if it was painted in the last frame.
pub fn label_of(window: &Window, id: impl Into<ElementId>) -> Option<SharedString> {
    gpui_kit::test::TestWindowExt::try_find(window, id)
        .and_then(|element| element.label().map(SharedString::new))
}

/// Opens a Select and moves `steps` options down from the current one
/// (from before the first when nothing is chosen), then commits.
pub fn choose_option(
    handle: AnyWindowHandle,
    cx: &mut TestAppContext,
    select: &'static str,
    steps: usize,
) {
    interact(handle, cx, |window, cx| {
        use gpui_kit::test::TestWindowExt as _;

        window.within(select).click("input", cx);

        // Inside dialogs the click only focuses; Down opens from the keyboard.
        if window.find(select).expanded() != Some(true) {
            window.press("down", cx);
        }

        for _ in 0..steps {
            window.press("down", cx);
        }

        window.press("enter", cx);
    });
}

/// Opens the new connection wizard and moves past its engine step by
/// picking the card `engine` (such as `"engine-sqlite"`).
pub fn start_new_connection(
    handle: AnyWindowHandle,
    cx: &mut TestAppContext,
    engine: &'static str,
) {
    use gpui_kit::test::TestWindowExt as _;

    interact(handle, cx, |window, cx| window.click("add-connection", cx));
    wait_until(handle, cx, "the engine step opens", |window, _| {
        window.try_find(engine).is_some()
    });
    interact(handle, cx, |window, cx| {
        window.click(engine, cx);
        window.click("connection-continue", cx);
    });
    wait_until(handle, cx, "the details step opens", |window, _| {
        window.try_find("connection-name").is_some()
    });
}

/// Runs `interact` against the window, then lets the UI settle.
pub fn interact<R>(
    handle: AnyWindowHandle,
    cx: &mut TestAppContext,
    interact: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    let result = cx
        .update_window(handle, |_, window, cx| {
            gpui_kit::test::TestWindowExt::render_frame(window, cx);
            interact(window, cx)
        })
        .expect("test window is open");
    cx.run_until_parked();
    result
}
