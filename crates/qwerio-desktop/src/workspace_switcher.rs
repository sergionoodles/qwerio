//! The workspace switcher dialog: pick a recent workspace, open another
//! folder, or create a new workspace.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, ParentElement as _, SharedString, Styled as _, WeakEntity, Window, div, px};
use qwerio_app::local_state::RecentWorkspace;

use crate::workbench::Workbench;

/// A recent workspace as a full-width row: its name above its path.
pub fn recent_workspace_button(id_prefix: &str, workspace: &RecentWorkspace, cx: &App) -> Button {
    let id = SharedString::from(format!("{id_prefix}-{}", workspace.path.display()));

    Button::new(id)
        .ghost()
        .w_full()
        .justify_start()
        .accessibility_label(workspace.name.clone())
        .child(
            v_flex()
                .w_full()
                .items_start()
                .text_left()
                .child(workspace.name.clone())
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(workspace.path.display().to_string()),
                ),
        )
}

/// Opens the switcher; choices are carried out by `workbench`.
pub fn open(
    workbench: WeakEntity<Workbench>,
    recent: Vec<RecentWorkspace>,
    current: Option<PathBuf>,
    window: &mut Window,
    cx: &mut App,
) {
    window.open_dialog(cx, move |dialog, _, cx| {
        let rows = recent.iter().map(|workspace| {
            let is_current = current.as_deref() == Some(workspace.path.as_path());
            let path = workspace.path.clone();
            let workbench = workbench.clone();

            recent_workspace_button("switch", workspace, cx)
                .selected(is_current)
                .on_click(move |_, window, cx| {
                    window.close_dialog(cx);

                    if !is_current {
                        open_path(&workbench, path.clone(), window, cx);
                    }
                })
        });

        let open_folder = {
            let workbench = workbench.clone();
            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                prompt(&workbench, false, window, cx)
            }
        };
        let new_workspace = {
            let workbench = workbench.clone();
            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                prompt(&workbench, true, window, cx)
            }
        };

        dialog
            .title("Switch Workspace")
            .w(px(480.))
            .child(
                v_flex()
                    .gap_0p5()
                    .max_h(px(360.))
                    .when(recent.is_empty(), |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No recent workspaces."),
                        )
                    })
                    .children(rows),
            )
            .footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("switch-open-folder")
                            .outline()
                            .icon(IconName::FolderOpen)
                            .label("Open Folder…")
                            .on_click(open_folder),
                    )
                    .child(
                        Button::new("switch-new-workspace")
                            .outline()
                            .icon(IconName::Plus)
                            .label("New Workspace…")
                            .on_click(new_workspace),
                    )
                    .child(div().flex_1())
                    .child(DialogClose::new().child(Button::new("switch-cancel").label("Cancel"))),
            )
    });
}

fn open_path(workbench: &WeakEntity<Workbench>, path: PathBuf, window: &mut Window, cx: &mut App) {
    workbench
        .update(cx, |workbench, cx| {
            workbench.open_path(path, None, window, cx)
        })
        .ok();
}

fn prompt(workbench: &WeakEntity<Workbench>, create: bool, window: &mut Window, cx: &mut App) {
    window.close_dialog(cx);
    workbench
        .update(cx, |workbench, cx| {
            workbench.prompt_and_open(create, window, cx)
        })
        .ok();
}
