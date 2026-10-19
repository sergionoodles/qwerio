# Qwerio implementation plan

Status: draft for review. This document proposes the implementation; it does not
authorize scaffolding, dependency installation, or release builds.

## 1. Product and first-iteration scope

Build a desktop database workspace that behaves like a file manager. Users browse
database objects, open tables with a double-click, follow relationships, and use
back/forward navigation. User-created SQL, Markdown, and text files live beside
database objects in the same explorer.

The first iteration includes:

- PostgreSQL and SQLite as equally supported database engines.
- Linux, macOS, and Windows support; Linux/Omarchy and macOS are the primary design
  references. Windows remains a release requirement.
- Connection management, schema discovery, table/view browsing, column filters,
  sorting, record details, and declared foreign-key navigation.
- Mixed database/file navigation, workspace folders, file editing, and associations
  between files and database objects.
- SQL editing and execution, bounded result loading, cancellation, query errors,
  and explicit transaction controls.
- Tabs, one optional split, restored navigation state, light/dark themes, keyboard
  operation, and useful accessibility semantics.

Defer visual row editing, schema designers, migrations, SSH tunnels, cloud sync,
plugins, AI chat, and additional engines. SQL execution can modify data when the
user explicitly opens a writable session; browsing starts read-only.

## 2. Proposed technical foundation

- **Language/build:** stable Rust and a Cargo workspace. Resolve compatible
  versions during Phase 0, record the toolchain, and generate the application
  lockfile through Cargo. There are no installed versions in this empty workspace.
- **UI:** GPUI through Longbridge's `gpui-kit`, including its supported matching
  GPUI snapshot. Do not combine unrelated GPUI distributions or the separately
  maintained `gpuikit` crate.
- **PostgreSQL:** `tokio-postgres`, with a certificate-validating TLS connector.
- **SQLite:** `rusqlite` on dedicated blocking workers, using a consistent bundled
  SQLite build if dependency review approves that configuration.
- **Background execution:** Tokio for PostgreSQL/network tasks; messages between
  application services and GPUI. Never run blocking database/filesystem operations
  or expensive decoding on the UI thread.
- **Persistence:** ordinary workspace files and a versioned TOML manifest; a
  separate application-owned SQLite database for machine-local history and state.
- **Platform integration:** small adapters for credentials, paths, file dialogs,
  window behavior, system appearance, and native menus where available.

GPUI draws the main interface. The goal is a consistent visual identity with
platform-appropriate behavior, rather than automatic use of OS-native widgets.
The current Kit setup lists macOS 15+, Windows 10+, and Linux with working Vulkan
drivers; validate the selected release and actual machines before promising a
minimum OS/GPU baseline. [GPUI Kit installation][kit-install]

## 3. Module boundaries from the first implementation

Start with six Cargo crates, each with focused internal modules:

- **`qwerio-core`:** engine-neutral database contracts, resource identities,
  connection descriptors, values, filters, result events, capabilities, and errors.
  No GPUI, filesystem access, concrete drivers, or credentials implementation.
- **`qwerio-postgres`:** PostgreSQL connections, catalog discovery, dialect SQL,
  value decoding, sessions, cancellation, and PostgreSQL integration tests.
- **`qwerio-sqlite`:** SQLite file opening, metadata, dialect SQL, worker lifecycle,
  sessions, interruption, and SQLite integration tests.
- **`qwerio-workspace`:** manifest persistence, document operations, file watching,
  document/object associations, and resolving resources into explorer entries.
- **`qwerio-app`:** use cases, connection/session registry, navigation history,
  background jobs, local application state, and event delivery. Its tests run
  without a graphical environment.
- **`qwerio-desktop`:** startup/composition, GPUI views, component adapters,
  application commands, themes, and OS integration modules.

Both database adapters depend on core, never on one another. Workspace depends on
core. Application services consume core interfaces and workspace services; desktop
startup registers the concrete database adapters. UI views never import database
drivers or construct SQL. Keep dependency injection explicit; an internal registry
is sufficient, without a plugin framework or separate service process.

### 3.1 Database contracts

Define behavior and ownership before implementing either adapter. The following
names are proposed application interfaces, not existing library APIs:

- **`DatabaseDriver`:** validate a typed connection descriptor and open a session.
- **`DatabaseSession`:** retain connection identity, execute requests, cancel active
  work, expose transaction state, and close cleanly.
- **`DatabaseCatalog`:** list namespaces, tables/views, columns, keys, relationships,
  and available engine-specific metadata.
- **`TableBrowser`:** translate structured filters/sorting into parameterized SQL
  and fetch a bounded page using an opaque continuation token.
- **`DatabaseCapabilities`:** describe namespaces, writable access, transaction
  behavior, cancellation, and supported browser filter operators.

Use typed PostgreSQL and SQLite connection options. PostgreSQL databases and
schemas are separate levels. A SQLite file is a database; present its main
namespace without inventing a PostgreSQL-style schema hierarchy. The UI renders
the resource hierarchy returned by the adapter.

Make requested access mode and actual enforcement explicit in session metadata.
Distinguish UI intent, transaction restrictions, and credential/file permissions;
application state alone is not a database authorization boundary.

Keep raw user SQL engine-specific. Do not translate arbitrary SQL between engines
or expose an ORM as the database-manager interface. Bind filter values and quote
identifiers inside each adapter. Shared code owns filter meaning; adapters own
placeholder syntax, quoting, NULL handling, ordering, and metadata queries.

### 3.2 Result and value contracts

- Preserve ordered column descriptors and positional rows. Duplicate output column
  names must survive; a map keyed by column name is insufficient.
- Represent NULL, empty strings, integers, floating-point values, text, and blobs
  distinctly. Preserve exact decimals and large integers without float coercion.
- Retain database type names and engine-specific information, with explicit
  display/raw fallbacks for unsupported PostgreSQL types. SQLite values carry
  per-cell storage types, even when a column declares another type.
- Distinguish canonical values, display formatting, truncation, and exports. A
  preview must never silently become the source for copying or saving full data.
- Deliver ordered events for result metadata, row batches, statement completion,
  errors, cancellation, and overall completion. Include request/result IDs and
  transaction state. Keep the shape capable of multiple result sets even though
  full script execution is deferred from the first iteration.
- Bound queues and retained results by bytes as well as row count. Stop fetching
  when the consumer pauses or closes; surface partial results and truncation.
- Ignore late events for superseded requests. Cancellation is best-effort and
  distinct from successful completion; reconcile session state afterward.

### 3.3 Session and engine differences

- Give each SQL console a retained physical connection. BEGIN, subsequent queries,
  COMMIT, and ROLLBACK must share that connection. Never silently reconnect an
  interrupted transaction or resume its statements automatically.
- Use separate short-lived/read-only browsing work so a long console query does
  not block unrelated navigation. Bound connections and worker counts.
- PostgreSQL exposes its real schema/catalog and permission limitations. Preserve
  composite keys, quoted identifiers, and foreign-key column ordering.
- SQLite opens existing files without an implicit CREATE flag. Read-only mode uses
  the driver's actual read-only open flags. File creation is a separate action.
- SQLite owns each connection on one worker; use an interrupt handle for active
  work and discard cancelled jobs that have not started. Track autocommit state,
  busy/locked errors, and per-connection foreign-key enforcement explicitly.
- Keep application-state SQLite storage separate from user SQLite databases.

These choices reflect the drivers' different streaming and execution models.
[PostgreSQL driver][postgres-driver] · [SQLite driver][sqlite-driver]

## 4. Workspace and navigation model

- Real documents remain ordinary `.sql`, `.md`, and `.txt` files inside a
  user-selected workspace folder. Virtual database entries are never directories
  that exist inside the database server or user database file.
- A versioned workspace manifest stores logical connection IDs, document IDs,
  relative file paths, virtual placement, and database-object associations.
- Store an object's connection ID, database/namespace, kind, and qualified name.
  Adapter-native identifiers may help reconciliation, but cannot guarantee stable
  identity across restores or external schema changes.
- Allow workspace-wide files, user folders, and references to several database
  objects. Moving a virtual entry changes organization, not the database schema.
  Renaming a document renames a file; renaming a table is an explicit SQL operation.
- Use logical connection aliases so a shared workspace can bind to a machine-local
  PostgreSQL endpoint or SQLite path. Credentials and machine-specific endpoints
  belong in local settings/credential storage, not the shareable manifest.
- A SQL document can remember an execution binding independently of its visual
  placement. Always show the actual execution target; placement alone never
  silently changes where an existing query runs.
- Treat breadcrumbs as structural location. Back/forward records traversal,
  including relationship navigation. Store each tab's filters, sort, selection,
  and scroll anchor; restore loaded results only if still valid.
- Save documents atomically and detect external modifications. Preserve unsaved
  drafts, handle deleted/moved files, and offer conflict resolution instead of
  overwriting a newer external version.
- Constrain document operations to the selected workspace, including symlink/path
  traversal checks. Missing objects/documents remain repairable associations.

## 5. Visual and platform direction

### 5.1 A modern desktop workspace

Use a calm, compact workbench with a clear resource hierarchy and generous editing
space. Tables and companion files share the explorer's visual language, while
distinct icons and subtitles identify live objects, SQL, Markdown, and text.

- **Layout:** a resizable left sidebar, a compact toolbar with breadcrumbs and
  navigation, and a tabbed content area. Put contextual filters in the sidebar;
  navigation and filters can collapse independently. Record metadata opens in an
  optional inspector. Query results open below their editor. Start with one
  optional split rather than a fully configurable docking system.
- **Browsing:** offer a detailed list and a compact icon view. Double-click opens;
  Enter opens the selection; context menus and the command palette expose actions.
- **Typography:** prefer each platform's UI font with tested fallbacks; use a
  configurable monospace face for SQL and data where alignment is helpful.
  Never rely on Nerd Font/private-use glyphs for essential icons.
- **Initial dark palette:** canvas `#1D222B`, panel `#252C36`, separator `#3A4655`,
  text `#E4E9EF`, secondary text `#A8B4C2`, and selection/action accent `#79B8CE`.
  These are starting design tokens, not fixed colors inside components. Supply a
  deliberate light palette and separate semantic/error/focus tokens.
- **Density:** start around 28–32 logical-pixel table rows and 13–14 logical-pixel
  UI text, with comfortable density and font/zoom controls. Validate on real displays.
- **Feedback:** visible focus, clear modified-file state, distinct NULL rendering,
  useful empty/error states, and restrained motion that respects reduced motion.
- **Accessibility:** target readable text contrast, keyboard access to every core
  action, and named/structured controls through the toolkit's accessibility layer.

Keep chrome quiet; the distinctive element is the mixed database/document browser,
not decorative cards, oversized headings, gradients, or permanent status panels.
Review colors, typography, hierarchy, and density against real table/SQL/Markdown
content before implementing the full shell.

### 5.2 Linux and Omarchy

- Treat native Wayland on Hyprland as a primary target; also test X11 fallback and
  a mainstream non-Omarchy Wayland desktop. Omarchy is an Arch/Hyprland environment,
  not a separate UI toolkit. [Omarchy manual][omarchy-manual]
- Work well in narrow tiled windows: collapse secondary panels, preserve the main
  content area, support keyboard resizing/navigation, and avoid excessive minimum
  window dimensions or fixed-position dialogs.
- Use a stable application ID, desktop entry, icons, file-open integration, XDG
  config/data/state/cache locations, and compositor-appropriate decorations.
  Avoid duplicate title bars/borders and unnecessary in-app window controls.
- Do not claim Super-based shortcuts that conflict with the window manager.
  Validate clipboard, input methods, fractional scaling, multi-monitor transitions,
  file dialogs/portals, focus restoration, and minimize/restore behavior.
- Offer **System**, **Light**, **Dark**, and an opt-in **Omarchy** theme source.
  Map a palette into semantic tokens rather than executing theme scripts. Support a
  configurable palette path and safe fallback when Omarchy is absent or changes.
- Verify the active Omarchy release's palette location/schema during implementation.
  Current upstream stages `colors.toml` under the active theme state directory;
  watch its parent/replacements because theme switching replaces that directory.
  Treat this integration as a versioned adapter, not a permanent platform API.
  [Omarchy theme switching source][omarchy-theme]

### 5.3 macOS and Windows

- macOS: appropriate application menus and Command shortcuts, standard window
  controls, a usable title-bar drag region, Retina rendering, system appearance,
  Keychain-backed credentials, and expected open/save/close behavior.
- Windows: standard window controls, Control shortcuts, correct per-monitor DPI,
  file associations, credential storage, and accessible keyboard operation.
- Prefer native menus where supported. The Kit documents AppKit and Win32 menu
  adapters; verify behavior instead of assuming every popup is native.
  [Native extensions][kit-native]
- Test shared components on all three OSes. Platform styling must not fork the
  database/application layers or become three independent UI implementations.

## 6. Delivery phases

Each phase ends with its acceptance checks. Keep edits small enough to review;
complete relevant automated checks before the final manual desktop check.

### Phase 0 — Validate the stack and platform baseline

**Work**

- Confirm the proposed Rust/Longbridge GPUI Kit stack before scaffolding. Resolve
  current compatible releases and inspect their licenses and feature flags.
- Run a minimal feasibility spike with a resource list, resizable panels, dynamic
  columns, editable SQL, and Markdown content on Linux, macOS, and Windows.
- Validate the lowest supported macOS version and Linux GPU/driver requirements.
  Exercise Hyprland/Wayland early; do not postpone it until release packaging.
- Prototype both database driver paths: PostgreSQL connection/TLS/stream/cancel and
  SQLite existing-file/read-only/worker/interrupt behavior.
- Verify native single-statement preparation rejects extra statements before any
  execution, including harmless trailing comments and SQL containing semicolons.
- Establish a benchmark fixture and reference hardware. Proposed targets are
  responsive scrolling at 60 Hz and bounded results with no growth after reaching
  the configured result budget; measure before finalizing budgets.

**Exit criteria**

- A real window renders and accepts input on all three operating systems.
- Dynamic grid and editor behavior are adequate to build on; platform blockers
  and actual minimum requirements are recorded in this plan.
- Both driver paths can execute and interrupt a test query without freezing UI.
- Stack changes prompted by the spike are resolved before feature implementation.

### Phase 1 — Establish module contracts and a minimal shell

**Work**

- Create the six-crate workspace and enforce the dependency boundaries above.
- Define driver/session/catalog/browser contracts, typed connection options,
  capabilities, dynamic values, resource IDs, and bounded result events.
- Add a driver registry and a background-task boundary. Define connection/session
  lifecycle, event ordering, shutdown, and cancellation semantics.
- Create a minimal GPUI shell with real loading, empty, and error states.
- Set up formatting, linting, unit tests, and a build/check matrix for all OSes.

**Exit criteria**

- Core and application tests run without GPUI or a live database.
- Boundary tests cover duplicate result names, NULL/empty distinction, exact
  numbers, filter semantics, and stale/cancelled result delivery.
- The app can register both adapters without conditional logic in views.

### Phase 2 — Implement PostgreSQL and SQLite together

**Work**

- Implement connection validation, metadata discovery, tables/views, columns,
  primary/composite keys, and declared foreign keys for both adapters.
- Implement structured filters, sorting, bounded page fetches, query execution,
  errors, cancellation, transaction state, and orderly session cleanup.
- PostgreSQL: support database/schema hierarchy, TLS validation, quoted names,
  permissions, known value types, and explicit unknown-type fallback.
- SQLite: support existing files, dynamic values, read-only/writable sessions,
  worker interruption, busy timeouts, metadata pragmas, and foreign-key settings.
- Prefer keyset paging when a deterministic compatible ordering exists; append a
  unique tie-breaker. Handle composite keys and NULL ordering explicitly. Use
  bounded offset paging when no suitable key/order exists, and explain its weaker
  stability under concurrent changes. Never assume a view has a primary key.
- Use disposable local PostgreSQL fixtures and temporary SQLite files. Never run
  integration tests or fixture setup against user/shared databases.

**Exit criteria**

- The same engine-neutral contract suite passes for both engines, supplemented by
  engine-specific tests rather than mocked driver implementations.
- Cover unusual identifiers, duplicate aliases, composite relationships, nullable
  values, blobs, exact numbers, empty results, permissions/errors, and cancellation.
- No accidental SQLite file creation; an active transaction remains on its session.
- Results remain bounded when the consumer is slow, stops, or disconnects.

### Phase 3 — Build workspace persistence and navigation

**Work**

- Implement workspace creation/opening, the versioned manifest, local connection
  bindings, credential references, and machine-local state storage.
- Implement SQL/Markdown/text documents, folders, atomic saves, external-change
  detection, draft recovery, and object/document associations.
- Merge virtual database resources and real files into a common explorer model.
- Implement breadcrumbs, back/forward, favorites, per-tab state, and missing-object
  repair. Preserve connection context across workspace and environment changes.

**Exit criteria**

- A workspace displays tables and files together for both engines, survives restart,
  and can be shared without transferring credentials or machine-local state.
- A file can reference multiple tables; a workspace-level query needs no table.
- Tests cover manifest upgrades, path escapes, external edits, unresolved bindings,
  object rename/drop, navigation restoration, and recovery after interrupted saves.

### Phase 4 — Deliver the explorer and visual system

**Work**

- Implement the token/theme system, sidebar, breadcrumbs, tabs, one split,
  connection form/file picker, resource list/icon views, and optional inspector.
- Connect real workspace/database resources; avoid a separate mock-only UI model.
- Add double-click/Enter opening, context menus, command palette, focus handling,
  and contextual sidebar filtering. Distinguish filtering visible objects from
  filtering records inside a table.
- Support tiled/narrow windows, saved layout, platform shortcuts, accessible names,
  and keyboard paths. Keep connection target and file modification state visible.

**Exit criteria**

- Users can open a workspace, connect to either engine, browse mixed resources,
  open a document/table, and return with history using mouse or keyboard.
- Review the actual shell manually on Omarchy/Hyprland, macOS, and Windows for
  density, text rendering, window behavior, and focus. Visual approval is a gate
  before expanding the interface; screenshots alone do not establish usability.

### Phase 5 — Deliver table browsing and relationship exploration

**Work**

- Adapt the grid to runtime columns and typed cells; implement server-side
  sorting/filtering, incremental pages, bounded caching, and read-only row details.
- Add sidebar conditions, NULL-aware operators, column visibility, copy/export,
  and on-demand large-value inspection with explicit size limits.
- Build relationship links from declared metadata, including composite keys.
  Follow referenced and referencing records while preserving tab/history state.
- Debounce filters, cancel obsolete requests, and invalidate continuation tokens
  whenever the connection, table, sort, filters, or metadata revision changes.

**Exit criteria**

- The same browse/filter/relationship flow works for PostgreSQL and SQLite.
- Sorting/filtering applies to the complete database query, not just loaded rows.
- Tests cover deterministic page boundaries, NULL/composite filters, missing keys,
  empty relationships, slow results, refresh, and stale-request races.
- Large fixture tables remain usable without loading the whole table or running
  an exact COUNT on every navigation. Copy/export preserves actual values and
  clearly distinguishes loaded rows from a separately requested full export.

### Phase 6 — Deliver documents and the SQL workbench

**Work**

- Add SQL highlighting, metadata-based table/column suggestions, plain-text editing,
  and Markdown preview. Rich semantic SQL completion can follow later.
- Support temporary query tabs and saved SQL documents, an explicit execution
  target, run-selection/run-document actions, cancellation, result grids, and
  source-linked errors when the engine provides positions.
- First iteration runs one statement per request. Use PostgreSQL's prepared/
  extended-query path and SQLite's native preparation/tail validation to reject
  extra statements before execution. Verify the selected driver versions enforce
  this behavior before enabling run-document; never split SQL on semicolons or
  execute a first statement before validating the rest. Full scripts and multiple
  result sets are a later feature.
- Expose BEGIN/COMMIT/ROLLBACK on retained sessions. Show active/failed transaction
  state, explain cancellation effects, and rollback/close safely when abandoning
  a console. No automatic retries for statements that may have changed data.
- Keep generated browsing queries inside engine read-only transactions or SQLite
  read-only connections. Enable arbitrary console SQL only after the user selects
  its execution mode. SQLite can enforce file-level read-only access; PostgreSQL
  session/transaction preferences are not a security sandbox for arbitrary SQL
  from a privileged role. Require a restricted database role when PostgreSQL
  read-only execution must be guaranteed; do not promise enforcement through SQL
  keyword checks or a UI toggle alone.
- Save query history locally with clear retention controls; redact credentials
  from connection errors/logs and avoid treating SQL text as harmless telemetry.

**Exit criteria**

- Users can save a query beside a table, reopen it, run it on the intended engine,
  inspect results, and navigate between the query and related objects.
- Tests cover strings/comments containing semicolons, multiple-statement rejection
  with no statement executed, parameter errors, duplicate output names, cancellation,
  rollback, failed transactions, wrong/unavailable targets, and external document
  conflicts.
- No query executes merely because a document or restored workspace was opened.

### Phase 7 — Integrate themes, harden platforms, and prepare release

**Work**

- Finish system appearance and optional Omarchy palette integration, including
  live updates, fallback, explicit overrides, and contrast validation.
- Finish native menus/dialog integration, credential storage, clipboard, desktop
  entries, file associations, platform directories, and launch/open-file behavior.
- Run unit/contract/integration checks on all OSes and perform focused manual
  desktop acceptance checks. A browser is not the verification harness for GPUI.
- Profile realistic table/editor workloads, memory budgets, idle activity, and
  shutdown. Production/release builds and packaging require separate authorization.
- Prepare Linux/Arch-compatible distribution, macOS bundles/signing/notarization,
  and Windows packaging/signing. Verify the chosen packaging toolchain rather than
  assuming the GUI framework provides a complete release pipeline.

**Exit criteria**

- All first-iteration flows pass on Linux, macOS, and Windows, including both engines.
- Omarchy works in native Wayland, tiled layouts, theme changes, and scaled displays;
  the app also works without Omarchy. macOS menus/shortcuts/window behavior and
  Windows DPI/shortcuts are verified on actual target environments.
- Large results, cancelled operations, unavailable credentials, SQLite locks,
  network loss, unsaved files, and shutdown have clear recoverable behavior.
- No release is declared supported based solely on compilation or CI success.

### Later phase — AI assistance and expanded database features

- Add an AI module behind application-owned interfaces; keep GPUI, providers, and
  database drivers independent. Decide on a Rust library or separate service when
  actual requirements exist; add no AI runtime dependency in the first iteration.
- Expose explicit operations such as describing an object, reading selected notes,
  proposing SQL, and executing an approved bounded query through existing services.
- Use selected workspace/database context, visible tool activity, cancellable
  streaming, and deliberate control over which data leaves the machine.
- Follow with visual data editing, schema changes, full SQL scripts, connection
  tunnels, and additional adapters as separate scoped features with their tests.

## 7. Verification and implementation order

- Implement Phases 0–2 before building significant database UI. Both adapters must
  pass their shared contracts before a PostgreSQL-only UI becomes entrenched.
- Build workspace services in Phase 3; integrate the final resource/navigation
  model in Phase 4. Complete Phases 5–6 before first-iteration acceptance in Phase 7.
- Use Rust unit tests for pure filters, resource resolution, navigation, result
  formatting, and manifests. Use local driver contract/integration tests for
  database behavior and focused GPUI tests for application commands and state.
- Add regression tests before bug fixes. Avoid broad snapshot suites or tests
  asserting internal method calls. Keep platform-specific behavior in manual
  desktop checks where a headless test cannot establish it.
- During implementation run Cargo formatting, Clippy, relevant unit/contract tests,
  and development checks through the repository's established commands. Establish
  those commands during scaffolding; no application checks exist to run today.
- At each UI phase end, review only the flows delivered in that phase. Capture
  actual desktop observations and unresolved platform issues; do not substitute
  web browser checks for native application verification.

The first iteration is complete only when the mixed workspace, both database
adapters, relationship browsing, editable documents, SQL workbench, bounded results,
and platform acceptance checks all pass. AI is outside that completion gate.

## 8. Implementation record (2026-10-17)

### 8.1 Phase 0 findings (Linux only so far)

- **Toolchain:** Rust 1.98.1, pinned in `rust-toolchain.toml`; workspace
  `rust-version` 1.92 (GPUI Kit's documented minimum).
- **Stack as resolved:** `gpui-kit` 0.7.3 (Apache-2.0) with its pinned
  `gpui-pre` 0.3.9 snapshot and the `tree-sitter-sql`/`tree-sitter-markdown`
  features; `tokio-postgres` 0.7.18 (MIT/Apache-2.0) with
  `tokio-postgres-rustls` 0.14, rustls 0.23 (ring provider), and
  `rustls-platform-verifier` 0.7.1; `rusqlite` 0.40.2 (MIT) with bundled
  SQLite 3.53.2; `keyring` 4.2 (Keychain, Credential Manager, Secret Service);
  `notify` 8.2; `directories` 6.0. No stack change was needed.
- **Window:** the GPUI shell opens and runs on Linux under niri (native
  Wayland) without errors. Hyprland/Omarchy, X11, macOS, and Windows are not
  yet exercised; minimum OS/GPU requirements remain unverified.
- **Single statements:** both drivers reject extra statements before running
  anything. PostgreSQL's extended-query `Parse` refuses multiple commands;
  rusqlite 0.40 prepares the tail and fails with `MultipleStatement`.
  Trailing comments and semicolons inside strings are accepted. Contract tests
  prove no statement executes.
- **PostgreSQL transaction state:** `tokio-postgres` does not expose the
  protocol's transaction status byte. The adapter tracks BEGIN/COMMIT/ROLLBACK
  and re-checks with a `SAVEPOINT` probe (succeeds only inside a transaction;
  distinct SQLSTATEs when idle or failed) after transaction-control
  statements, procedure calls, and failures inside transactions.
- **PostgreSQL streaming:** outside explicit transactions, row-returning
  statements run in a portal inside a short transaction and are fetched in
  batches, so reaching the budget or closing the consumer stops the server.
  Inside a user transaction no wrapper transaction can be opened; rows stream
  and the rest of a truncated result is drained and discarded (memory stays
  bounded, network does not).
- **Cancellation:** both `sqlite3_interrupt` and PostgreSQL cancel requests
  are lost when they arrive before the statement starts. SQLite uses a
  per-job flag checked by the progress handler; PostgreSQL checks a
  cancel flag between protocol steps in addition to the server cancel.
- **Benchmark fixture:** `cargo run -p qwerio-sqlite --example paging_benchmark`
  (1M rows, debug build, this machine): first page 0.8 ms, 50 keyset pages
  31.5 ms, first page sorted by an unindexed column 324 ms, contains filter
  305 ms. Budgets still need an optimized build on reference hardware.
- **GPUI UI tests:** `gpui-kit`'s `test-support` feature failed to resolve
  only because the lockfile held `cocoa` 0.26.1 and `unicode-properties`
  0.1.4, while the optional `gpui-fast` crates pin `=0.26.0` and `=0.1.3`.
  Locking those two patch versions resolves it; `gpui-fast` is locked but
  never compiled. Test support is a dev-dependency only, so release builds
  exclude it. UI tests call `allow_parking()` because SQLite, file watching,
  and local state run on real threads, and wait in real time.

### 8.2 What exists

- Six crates with the boundaries of §3; the engine-neutral contract suite
  lives in `qwerio-core` behind the `contract-tests` feature, so no seventh
  crate was added.
- Core: values (exact decimals, distinct NULL/empty, per-cell SQLite storage),
  ordered request events with bounded channels and byte/row budgets, filters
  with shared meaning (NULL-aware `≠`), keyset paging with explicit NULL
  ordering and composite tie-breakers, bounded offset paging for key-less
  objects, opaque continuation tokens bound to metadata revisions,
  relationship targets for composite keys, CSV/TSV export, credential
  redaction.
- Adapters: catalog discovery, typed values, filters/sorting/paging, single-
  statement execution, cancellation, transactions, read-only sessions
  (SQLite read-only flags; PostgreSQL transaction default, reported as not a
  security boundary), TLS with certificate and host-name validation, no
  implicit SQLite file creation.
- Workspace: versioned manifest (newer versions refused), atomic saves with
  conflict detection, interrupted-save cleanup, path confinement including
  symlinks, folders, placements, associations, execution bindings, untracked
  files, file watching, explorer model, breadcrumbs.
- Application: driver/session registries (shared read-only browse sessions,
  bounded console sessions), local state (bindings, recent workspaces, UI
  state, drafts, history with retention), navigation history, tabs with one
  split, table-browser and console state machines with stale-result rejection.
- Desktop: welcome screen, workspace shell (toolbar, back/forward,
  breadcrumbs, resizable sidebar with places and contextual filters, tabs,
  one split, status bar, command palette), explorer list and icon views,
  connection dialog, table grid with server-side sort/filter, incremental
  pages, inspector, relationship navigation, copy of loaded rows and full
  CSV export, documents with atomic save, conflicts, drafts and Markdown
  preview, SQL console with explicit target and access mode, temporary query
  tabs, Qwerio light/dark themes, System/Light/Dark/Omarchy theme sources
  with live Omarchy updates, keychain credentials, XDG paths, a Linux desktop
  entry, and native menus where supported.

- Headless UI tests (`crates/qwerio-desktop/src/ui_tests`) drive the real
  workbench through clicks and keystrokes: opening a recent workspace,
  keyboard table browsing with server-side sorting, adding a SQLite
  connection, running a query, rejecting a multi-statement script without
  running it, editing and saving a note, back/forward, and the split. Explorer
  rows, grid cells, form fields, and status text gained roles and accessible
  names for this.
- `.github/workflows/build-apps.yml` builds unsigned packages on `v*` tags
  and on demand: a Linux AppImage (`scripts/package-linux.sh`, Ubuntu 22.04
  glibc), a universal macOS DMG with an ad-hoc signed app
  (`scripts/package-macos.sh`), and a Windows MSI (WiX 5) plus portable zip
  (`scripts/package-windows.ps1`). Tags also create a draft release. The icon
  in `packaging/icons` is a placeholder; `scripts/generate-icons.sh`
  regenerates its PNG and ICO renditions.

### 8.3 Checks

`scripts/check.sh` runs `cargo fmt --check`, Clippy with warnings denied, and
every test; PostgreSQL tests run against a disposable local cluster started by
`scripts/with-test-postgres.sh` (localhost only, generated password, throwaway
TLS CA). PostgreSQL tests fail with a clear message when run without it.

### 8.4 Open before first-iteration acceptance

- Manual desktop review on Omarchy/Hyprland, X11, a mainstream Wayland
  desktop, macOS, and Windows (the Phase 4 visual gate), including fractional
  scaling, input methods, file portals, and window decorations.
- macOS/Windows builds have not been compiled or run on those platforms;
  the release workflow has not run yet.
- Code signing and notarization (Developer ID, Windows Authenticode) are not
  set up; packages are unsigned. A homepage URL is missing from the AppStream
  metadata.
- In the connection dialog, a click focuses the engine Select without
  opening it (it opens from the keyboard); the same Select opens on click
  outside dialogs. Not yet investigated.
- Accessibility semantics beyond what GPUI Kit components provide are not
  verified with a screen reader.

## References

References describe current upstream behavior; recheck selected versions in Phase 0.

[kit-install]: https://gpui-kit.com/docs/installation/
[kit-native]: https://gpui-kit.com/docs/native-extension/
[postgres-driver]: https://docs.rs/tokio-postgres/latest/tokio_postgres/struct.Client.html
[sqlite-driver]: https://docs.rs/rusqlite/latest/rusqlite/struct.Connection.html
[omarchy-manual]: https://omarchy.org/manual/
[omarchy-theme]: https://github.com/basecamp/omarchy/blob/master/bin/omarchy-theme-set
