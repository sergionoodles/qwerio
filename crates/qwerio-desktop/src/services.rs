//! Application services shared by every window.

use std::sync::Arc;

use gpui_kit::{App, Global};
use qwerio_app::AppServices;

pub struct Services {
    pub app: Arc<AppServices>,
}

impl Global for Services {}

impl Services {
    pub fn app(cx: &App) -> Arc<AppServices> {
        Arc::clone(&cx.global::<Self>().app)
    }
}
