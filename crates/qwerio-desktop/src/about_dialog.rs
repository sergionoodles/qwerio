//! The About dialog: the app icon, name, and version from the package
//! manifest, with a link to the source repository.

use gpui_kit::assets::IconName as LucideIcon;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogClose;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, img, px,
};

use crate::assets::LOGO_PATH;

pub const APP_NAME: &str = "Qwerio";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const LICENSE: &str = env!("CARGO_PKG_LICENSE");
pub const REPOSITORY_URL: &str = env!("CARGO_PKG_REPOSITORY");
/// The summary the Linux AppStream metadata gives the app.
const TAGLINE: &str = "Browse databases beside your queries and notes";

const DIALOG_WIDTH: f32 = 380.;
const LOGO_SIZE: f32 = 88.;
const LOGO_TILE_RADIUS: f32 = 28.;

/// Opens the dialog; Escape, the close button, or Close dismiss it.
pub fn open(window: &mut Window, cx: &mut App) {
    window.open_dialog(cx, |dialog, _, cx| {
        dialog.w(px(DIALOG_WIDTH)).child(render_body(cx)).footer(
            h_flex().w_full().justify_end().child(
                DialogClose::new().child(Button::new("about-close").primary().label("Close")),
            ),
        )
    });
}

fn render_body(cx: &App) -> impl IntoElement {
    v_flex()
        .items_center()
        .gap_4()
        .pt_4()
        .child(render_logo(cx))
        .child(
            v_flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .id("about-name")
                        .role(Role::Heading)
                        .aria_label(APP_NAME)
                        .text_2xl()
                        .font_bold()
                        .child(APP_NAME)
                        .test_support(),
                )
                .child(
                    div()
                        .text_sm()
                        .text_center()
                        .text_color(cx.theme().muted_foreground)
                        .child(TAGLINE),
                ),
        )
        .child(render_details(cx))
}

/// The icon on a raised tile, the way app stores present it.
fn render_logo(cx: &App) -> impl IntoElement {
    div()
        .p_3()
        .rounded(px(LOGO_TILE_RADIUS))
        .bg(cx.theme().muted)
        .border_1()
        .border_color(cx.theme().border)
        .shadow_md()
        .child(
            img(LOGO_PATH)
                .id("about-logo")
                .role(Role::Image)
                .aria_label(format!("{APP_NAME} icon"))
                .size(px(LOGO_SIZE))
                .test_support(),
        )
}

fn render_details(cx: &App) -> impl IntoElement {
    v_flex()
        .w_full()
        .mt_2()
        .p_1()
        .gap_0p5()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(cx.theme().border)
        .child(detail_row(
            "about-version",
            LucideIcon::Tag,
            "Version",
            VERSION,
            cx,
        ))
        .child(detail_row(
            "about-license",
            LucideIcon::Scale,
            "License",
            LICENSE,
            cx,
        ))
        .child(
            Button::new("about-repository")
                .ghost()
                .w_full()
                .accessibility_label(format!("Open the {APP_NAME} repository: {REPOSITORY_URL}"))
                .tooltip(REPOSITORY_URL)
                .on_click(|_, _, cx| cx.open_url(REPOSITORY_URL))
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(Icon::new(IconName::Github).text_color(cx.theme().muted_foreground))
                        .child("Source")
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_color(cx.theme().link)
                                .child(display_url(REPOSITORY_URL).to_owned()),
                        )
                        .child(
                            Icon::new(LucideIcon::ArrowUpRight)
                                .small()
                                .text_color(cx.theme().link),
                        ),
                ),
        )
}

/// A label and its value on one line, readable as "Label: value".
fn detail_row(
    id: &'static str,
    icon: LucideIcon,
    label: &'static str,
    value: &'static str,
    cx: &App,
) -> impl IntoElement {
    h_flex()
        .id(id)
        .role(Role::Label)
        .aria_label(format!("{label}: {value}"))
        .w_full()
        .h_8()
        .px_3()
        .gap_2()
        .text_sm()
        .child(Icon::new(icon).text_color(cx.theme().muted_foreground))
        .child(label)
        .child(div().flex_1())
        .child(div().text_color(cx.theme().muted_foreground).child(value))
        .test_support()
}

/// A URL without its scheme, as people read and type it.
fn display_url(url: &str) -> &str {
    ["https://", "http://"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme))
        .unwrap_or(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_url_drops_the_scheme() {
        assert_eq!(
            display_url("https://github.com/sergionoodles/qwerio"),
            "github.com/sergionoodles/qwerio"
        );
        assert_eq!(display_url("http://example.com"), "example.com");
    }

    #[test]
    fn display_url_keeps_a_url_without_a_known_scheme() {
        assert_eq!(display_url("example.com/qwerio"), "example.com/qwerio");
    }
}
