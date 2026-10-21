//! Qwerio desktop entry point: composes services, registers the database
//! adapters, and opens the first window.

mod about_dialog;
mod actions;
mod assets;
mod connection_dialog;
mod content;
mod document_view;
mod explorer_view;
mod filter_panel;
mod grid;
mod label_dialog;
mod platform;
mod schema_dialog;
mod schema_graph;
mod services;
mod settings_dialog;
mod shell;
mod sql_console;
mod table_view;
mod theme;
#[cfg(test)]
mod ui_tests;
mod workbench;
mod workspace_model;
mod workspace_password;
mod workspace_switcher;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use qwerio_app::AppServices;
use qwerio_app::local_state::LocalState;
use qwerio_app::registry::DriverRegistry;
use qwerio_postgres::PostgresDriver;
use qwerio_sqlite::SqliteDriver;

use crate::assets::AppAssets;
use crate::platform::keychain::KeychainCredentialStore;
use crate::platform::paths::AppPaths;
use crate::services::Services;

/// Worker threads for database network I/O.
const DATABASE_THREADS: usize = 2;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("Qwerio could not start: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let paths = AppPaths::discover().ok_or("no home directory was found")?;
    let Some(_instance) = platform::instance::AppInstance::acquire(&paths.state_directory)
        .map_err(|error| format!("application instance lock: {error}"))?
    else {
        eprintln!("Qwerio is already running.");
        return Ok(());
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(DATABASE_THREADS)
        .thread_name("qwerio-database")
        .enable_all()
        .build()
        .map_err(|error| format!("database runtime: {error}"))?;

    std::fs::create_dir_all(&paths.state_directory)
        .map_err(|error| format!("{}: {error}", paths.state_directory.display()))?;
    let local_state =
        LocalState::open(&paths.state_database()).map_err(|error| error.to_string())?;

    let mut drivers = DriverRegistry::new();
    drivers.register(Arc::new(SqliteDriver::new()));
    drivers.register(Arc::new(PostgresDriver::new(runtime.handle().clone())));

    let services = Services {
        app: Arc::new(AppServices::new(
            drivers,
            local_state,
            Arc::new(KeychainCredentialStore),
        )),
    };

    let (application, arguments) = platform::application::application(std::env::args_os())?;
    let initial_workspace = arguments.into_iter().nth(1).map(PathBuf::from);

    application.with_assets(AppAssets).run(move |cx| {
        gpui_kit::init(cx);
        theme::init(cx);
        actions::bind_keys(cx);
        explorer_view::bind_keys(cx);
        actions::set_menus(cx);
        cx.set_global(services);

        cx.on_action(|_: &actions::Quit, cx| cx.quit());

        let sessions = Arc::clone(&Services::app(cx).sessions);
        cx.on_app_quit(move |_| {
            let sessions = Arc::clone(&sessions);
            async move { sessions.close_all().await }
        })
        .detach();

        workbench::open_window(initial_workspace, cx);
        cx.activate(true);
    });

    drop(runtime);
    Ok(())
}
