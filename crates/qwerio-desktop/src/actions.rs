//! Application commands. Every toolbar button, menu item, palette entry, and
//! shortcut dispatches one of these actions. Bindings use `secondary`
//! (Command on macOS, Control elsewhere) and never claim Super, which tiling
//! window managers such as Hyprland reserve.

use gpui_kit::{App, KeyBinding, Menu, MenuItem, actions};

actions!(
    qwerio,
    [
        OpenWorkspace,
        NewWorkspace,
        ImportWorkspace,
        ExportWorkspace,
        CloseWorkspace,
        NewConnection,
        NewDatabase,
        NewTable,
        NewColumn,
        NewSqlDocument,
        NewTemporaryQuery,
        NewNote,
        NewFolder,
        GoBack,
        GoForward,
        GoToWorkspaceRoot,
        CloseTab,
        NewTab,
        NextTab,
        PreviousTab,
        ToggleSplit,
        FocusOtherPane,
        ToggleSidebar,
        ToggleInspector,
        FocusExplorerFilter,
        OpenCommandPalette,
        Refresh,
        Save,
        EditCell,
        AddRow,
        DuplicateRow,
        DeleteRow,
        RunStatement,
        CancelStatement,
        BeginTransaction,
        CommitTransaction,
        RollbackTransaction,
        TogglePreview,
        AddToFavorites,
        ToggleExplorerLayout,
        ShowRelationshipGraph,
        IncreaseFontSize,
        DecreaseFontSize,
        ResetFontSize,
        OpenSettings,
        About,
        Quit,
    ]
);

/// Key context of the workbench root.
pub const WORKBENCH_CONTEXT: &str = "Workbench";

/// Key context of a SQL document.
pub const SQL_DOCUMENT_CONTEXT: &str = "SqlDocument";

/// Key context of gpui-component's data table, which has no Enter binding.
pub const DATA_TABLE_CONTEXT: &str = "DataTable";

pub fn bind_keys(cx: &mut App) {
    let workbench = Some(WORKBENCH_CONTEXT);
    let sql = Some(SQL_DOCUMENT_CONTEXT);
    let data_table = Some(DATA_TABLE_CONTEXT);

    cx.bind_keys([
        KeyBinding::new("secondary-o", OpenWorkspace, workbench),
        KeyBinding::new("secondary-shift-n", NewConnection, workbench),
        KeyBinding::new("secondary-n", NewSqlDocument, workbench),
        KeyBinding::new("secondary-alt-n", NewTemporaryQuery, workbench),
        KeyBinding::new("secondary-t", NewTab, workbench),
        KeyBinding::new("secondary-w", CloseTab, workbench),
        KeyBinding::new("ctrl-tab", NextTab, workbench),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, workbench),
        KeyBinding::new("secondary-\\", ToggleSplit, workbench),
        KeyBinding::new("secondary-k secondary-o", FocusOtherPane, workbench),
        KeyBinding::new("secondary-b", ToggleSidebar, workbench),
        KeyBinding::new("secondary-i", ToggleInspector, workbench),
        KeyBinding::new("secondary-shift-f", FocusExplorerFilter, workbench),
        KeyBinding::new("secondary-shift-p", OpenCommandPalette, workbench),
        KeyBinding::new("secondary-k secondary-p", OpenCommandPalette, workbench),
        KeyBinding::new("f5", Refresh, workbench),
        KeyBinding::new("secondary-s", Save, workbench),
        KeyBinding::new("secondary-=", IncreaseFontSize, workbench),
        KeyBinding::new("secondary--", DecreaseFontSize, workbench),
        KeyBinding::new("secondary-0", ResetFontSize, workbench),
        KeyBinding::new("secondary-,", OpenSettings, workbench),
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-enter", RunStatement, sql),
        KeyBinding::new("secondary-.", CancelStatement, sql),
        KeyBinding::new("secondary-shift-v", TogglePreview, workbench),
        KeyBinding::new("enter", EditCell, data_table),
        KeyBinding::new("f2", EditCell, data_table),
        KeyBinding::new("alt-insert", AddRow, data_table),
        KeyBinding::new("secondary-d", DuplicateRow, data_table),
        KeyBinding::new("delete", DeleteRow, data_table),
    ]);

    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-[", GoBack, workbench),
            KeyBinding::new("cmd-]", GoForward, workbench),
            KeyBinding::new("cmd-up", GoToWorkspaceRoot, workbench),
        ]);
    } else {
        cx.bind_keys([
            KeyBinding::new("alt-left", GoBack, workbench),
            KeyBinding::new("alt-right", GoForward, workbench),
            KeyBinding::new("alt-home", GoToWorkspaceRoot, workbench),
        ]);
    }
}

/// Native application menus (used where the platform provides a menu bar).
/// Call after [`bind_keys`] so shortcuts appear next to items.
pub fn set_menus(cx: &mut App) {
    cx.set_menus(application_menus());
}

/// Shared by native menu bars and the in-window application menu.
pub fn application_menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Qwerio".into(),
            items: vec![
                MenuItem::action("About Qwerio", About),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit Qwerio", Quit),
            ],
            disabled: false,
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New Workspace…", NewWorkspace),
                MenuItem::action("Open Workspace…", OpenWorkspace),
                MenuItem::action("Import Workspace…", ImportWorkspace),
                MenuItem::action("Export Workspace…", ExportWorkspace),
                MenuItem::separator(),
                MenuItem::action("New Connection…", NewConnection),
                MenuItem::action("New Database…", NewDatabase),
                MenuItem::action("New Table…", NewTable),
                MenuItem::action("New Column…", NewColumn),
                MenuItem::action("New Temporary Query", NewTemporaryQuery),
                MenuItem::action("New SQL Document", NewSqlDocument),
                MenuItem::action("New Note", NewNote),
                MenuItem::action("New Folder", NewFolder),
                MenuItem::separator(),
                MenuItem::action("Save", Save),
                MenuItem::action("Close Tab", CloseTab),
                MenuItem::action("Close Workspace", CloseWorkspace),
            ],
            disabled: false,
        },
        Menu {
            name: "Go".into(),
            items: vec![
                MenuItem::action("Back", GoBack),
                MenuItem::action("Forward", GoForward),
                MenuItem::action("Workspace", GoToWorkspaceRoot),
                MenuItem::separator(),
                MenuItem::action("Command Palette…", OpenCommandPalette),
            ],
            disabled: false,
        },
        Menu {
            name: "Query".into(),
            items: vec![
                MenuItem::action("Run Statement", RunStatement),
                MenuItem::action("Cancel", CancelStatement),
                MenuItem::separator(),
                MenuItem::action("Begin Transaction", BeginTransaction),
                MenuItem::action("Commit", CommitTransaction),
                MenuItem::action("Roll Back", RollbackTransaction),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Toggle Sidebar", ToggleSidebar),
                MenuItem::action("Toggle Inspector", ToggleInspector),
                MenuItem::action("Split", ToggleSplit),
                MenuItem::action("Markdown Preview", TogglePreview),
                MenuItem::separator(),
                MenuItem::action("Bigger Text", IncreaseFontSize),
                MenuItem::action("Smaller Text", DecreaseFontSize),
                MenuItem::action("Actual Size", ResetFontSize),
            ],
            disabled: false,
        },
    ]
}
