fn main() {
    const RESOURCE_PATH: &str = "qwerio.rc";
    const ICON_PATH: &str = "../../packaging/icons/qwerio.ico";

    println!("cargo:rerun-if-changed={RESOURCE_PATH}");
    println!("cargo:rerun-if-changed={ICON_PATH}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // GPUI loads resource ID 1 for the window and taskbar icon.
    embed_resource::compile_for(RESOURCE_PATH, ["qwerio"], embed_resource::NONE)
        .manifest_required()
        .expect("compile the Qwerio executable icon");
}
