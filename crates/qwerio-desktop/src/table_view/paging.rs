//! Page delivery waits for active cell validation and any in-flight save.
use super::TableGridDelegate;
use gpui_kit::Context;
use gpui_kit::component::table::TableState;
use qwerio_app::table::PageFetch;

impl TableGridDelegate {
    pub(super) fn fetch(&mut self, fetch: PageFetch, cx: &mut Context<TableState<Self>>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        cx.spawn(async move |table, cx| {
            let result = session.fetch_page(fetch.request).await;
            table
                .update(cx, |table, cx| {
                    let delegate = table.delegate_mut();
                    if delegate.edit_session.is_saving() || !delegate.commit_edit(cx) {
                        delegate.edit_session.defer_page(fetch.ticket, result);
                        return;
                    }
                    let columns_before = delegate.browser.columns().len();
                    delegate.browser.apply_page(fetch.ticket, result);
                    if table.delegate().browser.columns().len() != columns_before {
                        table.refresh(cx);
                    }
                    cx.notify();
                })
                .ok();
        })
        .detach();
    }

    pub(super) fn apply_deferred_pages(&mut self, cx: &mut Context<TableState<Self>>) {
        if self.editing.is_some() || self.edit_session.is_saving() {
            return;
        }
        self.edit_session.apply_deferred_pages(&mut self.browser);
        cx.notify();
    }

    pub(super) fn restart(&mut self, fetch: Option<PageFetch>, cx: &mut Context<TableState<Self>>) {
        if let Some(fetch) = fetch {
            self.fetch(fetch, cx);
        }
        cx.notify();
    }
}
