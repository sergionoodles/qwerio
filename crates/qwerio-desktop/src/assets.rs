//! Icons: the default component set plus the Lucide icons Qwerio uses.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

gpui_kit::assets::icon_assets!(
    pub QwerioIcons,
    [
        Database,
        Table,
        Table2,
        Sheet,
        FileCode,
        FilePlus,
        FolderPlus,
        Server,
        Plug,
        Unplug,
        KeyRound,
        Link,
        Funnel,
        ListFilter,
        Columns3,
        Lock,
        LockOpen,
        Download,
        LayoutGrid,
        List,
        NotebookPen,
        Pencil,
        Trash,
        Save,
        Split,
        ScrollText,
        StickyNote,
        CornerDownRight,
        ArrowUpRight,
        X
    ]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = QwerioIcons.load(path)? {
            return Ok(Some(bytes));
        }

        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(QwerioIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}
