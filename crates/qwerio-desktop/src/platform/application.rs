//! Where Qwerio runs: a desktop window, or inside the terminal with `--tui`.

use std::ffi::OsString;

use gpui_kit::Application;

/// The application to run, and the command-line arguments left for Qwerio.
///
/// `--tui` (or `GPUI_TUI=1`) runs Qwerio inside a kitty-compatible terminal. It takes over the
/// terminal right away and fails if the terminal cannot show images.
#[cfg(feature = "tui")]
pub fn application(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<(Application, Vec<OsString>), String> {
    let (use_terminal, arguments) = gpui_tui::take_tui_flag(arguments);
    if !use_terminal {
        return Ok((gpui_kit::application(), arguments));
    }
    let application = gpui_tui::try_application(gpui_tui::Options::from_env())
        .map_err(|error| format!("terminal: {error:#}"))?;
    Ok((application, arguments))
}

/// The desktop application. `--tui` is refused rather than opened as a workspace path.
#[cfg(not(feature = "tui"))]
pub fn application(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<(Application, Vec<OsString>), String> {
    const TUI_FLAG: &str = "--tui";

    let arguments: Vec<OsString> = arguments.into_iter().collect();
    if arguments
        .iter()
        .skip(1)
        .any(|argument| argument == TUI_FLAG)
    {
        return Err(format!(
            "this build cannot run in the terminal; rebuild with `--features tui` to use {TUI_FLAG}"
        ));
    }
    Ok((gpui_kit::application(), arguments))
}
