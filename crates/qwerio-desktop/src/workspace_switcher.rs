//! Native workspace file dialogs. Workspace selection uses the OS file picker.
use crate::platform::paths::AppPaths;
use crate::workbench::Workbench;
use gpui_kit::{App, PathPromptOptions, WeakEntity, Window};
use std::path::PathBuf;

pub(crate) fn choose_source(
    this: WeakEntity<Workbench>,
    allow_legacy: bool,
    window: &mut Window,
    cx: &mut App,
    selected: impl FnOnce(WeakEntity<Workbench>, PathBuf, &mut Window, &mut App) + 'static,
) {
    let prompt = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: allow_legacy,
        multiple: false,
        prompt: Some(
            if allow_legacy {
                "Import Workspace or Legacy Folder"
            } else {
                "Open Workspace"
            }
            .into(),
        ),
    });
    window
        .spawn(cx, async move |cx| {
            let result = prompt.await;
            cx.update(|window, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(path) = paths.into_iter().next() {
                        selected(this, path, window, cx);
                    }
                }
                Ok(Err(error)) => {
                    this.update(cx, |this, cx| {
                        this.report_error(error.to_string(), window, cx)
                    })
                    .ok();
                }
                _ => {}
            })
            .ok();
        })
        .detach();
}

pub(crate) fn choose_destination(
    this: WeakEntity<Workbench>,
    suggested: &str,
    window: &mut Window,
    cx: &mut App,
    selected: impl FnOnce(WeakEntity<Workbench>, PathBuf, &mut Window, &mut App) + 'static,
) {
    let directory = AppPaths::discover()
        .map(|paths| paths.data_directory)
        .unwrap_or_else(std::env::temp_dir);
    let prompt = cx.prompt_for_new_path(&directory, Some(suggested));
    window
        .spawn(cx, async move |cx| {
            let result = prompt.await;
            cx.update(|window, cx| match result {
                Ok(Ok(Some(path))) => selected(this, path.with_extension("qwerio"), window, cx),
                Ok(Err(error)) => {
                    this.update(cx, |this, cx| {
                        this.report_error(error.to_string(), window, cx)
                    })
                    .ok();
                }
                _ => {}
            })
            .ok();
        })
        .detach();
}
