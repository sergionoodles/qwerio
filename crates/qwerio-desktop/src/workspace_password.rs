//! Password choices for creation, unlocking, and independent exports.
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{WindowExt as _, h_flex, v_flex};
use gpui_kit::{App, AppContext as _, ParentElement as _, Styled as _, Window, px};
use qwerio_core::connection::Secret;
use std::cell::RefCell;
use std::rc::Rc;

use crate::theme::glass;

type Accept = Box<dyn FnOnce(Option<Secret>, &mut Window, &mut App)>;

pub fn open(
    title: &str,
    optional: bool,
    window: &mut Window,
    cx: &mut App,
    accept: impl FnOnce(Option<Secret>, &mut Window, &mut App) + 'static,
) {
    let input = cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder("Password")
    });
    let confirmation = cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder("Confirm password")
    });
    let accept: Rc<RefCell<Option<Accept>>> = Rc::new(RefCell::new(Some(Box::new(accept))));
    let title = title.to_owned();
    glass::open_dialog(window, cx, move |dialog, _, _| {
        let (password, confirm, callback) =
            (input.clone(), confirmation.clone(), Rc::clone(&accept));
        let encrypted = Button::new("workspace-password-accept")
            .primary()
            .label(if optional { "Encrypt" } else { "Unlock" })
            .on_click(move |_, window, cx| {
                let text = password.read(cx).value().to_string();
                if text.is_empty() {
                    return;
                }
                if optional && text != confirm.read(cx).value().as_str() {
                    window.push_notification(
                        gpui_kit::component::notification::Notification::error(
                            "Passwords do not match",
                        ),
                        cx,
                    );
                    return;
                }
                window.close_dialog(cx);
                if let Some(callback) = callback.borrow_mut().take() {
                    callback(Some(Secret::new(text)), window, cx);
                }
            });
        let callback = Rc::clone(&accept);
        let without = Button::new("workspace-without-password")
            .outline()
            .label("Without Password")
            .on_click(move |_, window, cx| {
                window.close_dialog(cx);
                if let Some(callback) = callback.borrow_mut().take() {
                    callback(None, window, cx);
                }
            });
        let body = v_flex().gap_3()
            .child(if optional { "Workspaces include documents and saved connection passwords. Encrypt this file with a password, or continue without one. Passwords cannot be recovered." } else { "Enter the password for this workspace." })
            .child(Input::new(&input).id("workspace-password"));
        let body = if optional {
            body.child(Input::new(&confirmation).id("workspace-password-confirm"))
        } else {
            body
        };
        let footer = h_flex().gap_2().justify_end().child(
            DialogClose::new().child(
                Button::new("workspace-password-cancel")
                    .outline()
                    .label("Cancel"),
            ),
        );
        let footer = if optional {
            footer.child(without)
        } else {
            footer
        };
        dialog
            .title(title.clone())
            .w(px(480.))
            .child(body)
            .footer(footer.child(encrypted))
    });
}
