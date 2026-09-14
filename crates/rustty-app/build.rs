fn main() {
    #[cfg(windows)]
    windows_resources();
}

#[cfg(windows)]
fn windows_resources() {
    use std::{env, fs, path::PathBuf};
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let manifest = root.join("resources/windows.manifest");
    let icon = root.join("../../dist/windows/ghostty.ico");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed={}", icon.display());
    let escape = |path: &std::path::Path| path.to_string_lossy().replace('\\', "/");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let major = env::var("CARGO_PKG_VERSION_MAJOR").unwrap();
    let minor = env::var("CARGO_PKG_VERSION_MINOR").unwrap();
    let patch = env::var("CARGO_PKG_VERSION_PATCH").unwrap();
    let resource = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("rustty.rc");
    fs::write(
        &resource,
        format!(
            r#"
LANGUAGE 0, 0
1 ICON "{}"
1 24 "{}"
1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
FILEOS 0x40004
FILETYPE 1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "FileDescription", "Rustty terminal"
      VALUE "FileVersion", "{version}"
      VALUE "OriginalFilename", "rustty.exe"
      VALUE "ProductName", "Rustty"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#,
            escape(&icon),
            escape(&manifest)
        ),
    )
    .expect("write Windows resources");
    embed_resource::compile_for(&resource, ["rustty"], embed_resource::NONE)
        .manifest_required()
        .expect("compile Windows icon and DPI manifest");
}
