//! Bundled branding and the default component and Lucide icons Qwerio uses.

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

pub const LOGO_PATH: &str = "branding/logo.png";
pub const POSTGRES_LOGO_PATH: &str = "engines/postgresql.svg";
pub const SQLITE_LOGO_PATH: &str = "engines/sqlite.svg";

/// Monochrome engine marks from Simple Icons (CC0), tinted like other icons.
const BUNDLED: [(&str, &[u8]); 3] = [
    (LOGO_PATH, include_bytes!("../../../resources/logo_512.png")),
    (
        POSTGRES_LOGO_PATH,
        include_bytes!("../../../resources/engines/postgresql.svg"),
    ),
    (
        SQLITE_LOGO_PATH,
        include_bytes!("../../../resources/engines/sqlite.svg"),
    ),
];

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
        Network,
        ZoomIn,
        ZoomOut,
        NotebookPen,
        Pencil,
        Trash,
        Save,
        Split,
        ScrollText,
        StickyNote,
        CornerDownRight,
        ArrowUpRight,
        ArrowLeftRight,
        Tag,
        Scale,
        X
    ]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = BUNDLED.iter().find(|(bundled, _)| *bundled == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }

        if let Some(bytes) = QwerioIcons.load(path)? {
            return Ok(Some(bytes));
        }

        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(QwerioIcons.list(path)?);
        paths.extend(
            BUNDLED
                .iter()
                .filter(|(bundled, _)| bundled.starts_with(path))
                .map(|(bundled, _)| SharedString::from(*bundled)),
        );
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_logos_load() {
        for path in [POSTGRES_LOGO_PATH, SQLITE_LOGO_PATH] {
            let bytes = AppAssets.load(path).expect("load").expect("bundled logo");
            assert!(bytes.starts_with(b"<svg"));
        }
    }

    #[test]
    fn bundled_logo_loads_and_is_listed_under_branding() {
        let bytes = AppAssets
            .load(LOGO_PATH)
            .expect("load logo")
            .expect("bundled logo");
        assert_eq!(
            bytes.as_ref(),
            include_bytes!("../../../resources/logo_512.png")
        );
        assert!(
            AppAssets
                .list("branding/")
                .expect("list branding")
                .iter()
                .any(|path| path == LOGO_PATH)
        );
        assert!(
            !AppAssets
                .list("icons/")
                .expect("list icons")
                .iter()
                .any(|path| path == LOGO_PATH)
        );
    }
}
