//! Bundled branding and the default component and Lucide icons Qwerio uses.

use std::borrow::Cow;

use gpui_kit::component::ThemeMode;
use gpui_kit::{AssetSource, Result, SharedString};

/// White and orange mark, for dark surfaces; also the official logo.
pub const LOGO_ON_DARK_PATH: &str = "branding/logo.png";
/// The same mark with its white half inverted to a dark tone, for light surfaces.
pub const LOGO_ON_LIGHT_PATH: &str = "branding/logo-light.png";
pub const POSTGRES_LOGO_PATH: &str = "engines/postgresql.svg";
pub const SQLITE_LOGO_PATH: &str = "engines/sqlite.svg";

/// Monochrome engine marks from Simple Icons (CC0), tinted like other icons.
const BUNDLED: [(&str, &[u8]); 4] = [
    (
        LOGO_ON_DARK_PATH,
        include_bytes!("../../../resources/logo_512.png"),
    ),
    (
        LOGO_ON_LIGHT_PATH,
        include_bytes!("../../../resources/logo_light_512.png"),
    ),
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
        ShieldCheck,
        ShieldUser,
        UserPlus,
        Code,
        X
    ]
);

/// The app logo variant that stays legible on the given theme's surfaces.
pub fn logo_path(mode: ThemeMode) -> &'static str {
    if mode.is_dark() {
        LOGO_ON_DARK_PATH
    } else {
        LOGO_ON_LIGHT_PATH
    }
}

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
    fn permission_icons_load() {
        use gpui_kit::assets::IconName;

        for icon in [
            IconName::ShieldCheck,
            IconName::ShieldUser,
            IconName::UserPlus,
            IconName::Code,
        ] {
            let path = icon.path();
            let bytes = AppAssets.load(&path).expect("load").expect("bundled icon");
            assert!(bytes.starts_with(b"<svg"), "{path} is not an SVG");
        }
    }

    #[test]
    fn logo_matches_the_theme_mode() {
        assert_eq!(logo_path(ThemeMode::Dark), LOGO_ON_DARK_PATH);
        assert_eq!(logo_path(ThemeMode::Light), LOGO_ON_LIGHT_PATH);
    }

    #[test]
    fn bundled_logos_load_and_are_listed_under_branding() {
        let logos: [(&str, &[u8]); 2] = [
            (
                LOGO_ON_DARK_PATH,
                include_bytes!("../../../resources/logo_512.png"),
            ),
            (
                LOGO_ON_LIGHT_PATH,
                include_bytes!("../../../resources/logo_light_512.png"),
            ),
        ];
        let branding = AppAssets.list("branding/").expect("list branding");
        let icons = AppAssets.list("icons/").expect("list icons");

        for (path, expected) in logos {
            let bytes = AppAssets
                .load(path)
                .expect("load logo")
                .expect("bundled logo");
            assert_eq!(bytes.as_ref(), expected, "{path}");
            assert!(branding.iter().any(|listed| listed == path), "{path}");
            assert!(!icons.iter().any(|listed| listed == path), "{path}");
        }
    }
}
