#!/usr/bin/env nu

# Build a local macOS app bundle without Xcode or the Zig implementation.
def main [
    --release # Build an optimized executable (debug is the default).
    --offline # Use only dependencies already present in Cargo's cache.
] {
    if $nu.os-info.name != "macos" {
        error make {msg: "This bundle script currently targets macOS."}
    }
    let root = ($env.FILE_PWD | path join ../.. | path expand)
    let profile = if $release { "release" } else { "debug" }
    let target = ($root | path join target)
    let output = ($target | path join $profile)
    let stage = ($output | path join rustty-bundle)
    let app = ($stage | path join Rustty.app)
    let contents = ($app | path join Contents)
    let resources = ($contents | path join Resources)
    let data = ($resources | path join rustty)
    let source = ($root | path join crates rustty-app resources)
    let flags = (if $release { [--release] } else { [] })
        | append (if $offline { [--offline] } else { [] })

    do --capture-errors {
        ^cargo build --manifest-path ($root | path join Cargo.toml) --target-dir $target --locked -p rustty-app --bin rustty ...$flags
    }
    rm --recursive --force $stage
    mkdir ($contents | path join MacOS) $data
    cp ($output | path join rustty) ($contents | path join MacOS rustty)

    let version = (open ($root | path join Cargo.toml) | get workspace.package.version | split row '-' | first)
    open --raw ($source | path join Info.plist)
        | str replace --all '@VERSION@' $version
        | save ($contents | path join Info.plist)
    'APPL????' | save --raw ($contents | path join PkgInfo)

    # Copy the directory, including zsh's hidden .zshenv bootstrap.
    cp --recursive ($root | path join src shell-integration) ($data | path join shell-integration)
    cp --recursive ($source | path join themes) ($data | path join themes)
    cp ($source | path join ghostty.terminfo) ($data | path join ghostty.terminfo)
    mkdir ($data | path join terminfo)
    do --capture-errors { ^tic -x -o ($data | path join terminfo) ($data | path join ghostty.terminfo) }

    let notices = ($resources | path join licenses)
    mkdir $notices
    cp ($root | path join LICENSE) ($notices | path join Ghostty-MIT.txt)
    cp ($source | path join iTerm2-Color-Schemes-LICENSE.txt) $notices
    cp ($root | path join crates rustty-font resources JetBrainsMono-OFL.txt) $notices
    cp ($root | path join crates rustty-font resources NerdFontsSymbols-LICENSE.txt) $notices
    cp ($source | path join README.md) ($notices | path join Resources.md)

    # Reuse Ghostty's checked-in macOS artwork at the sizes iconutil requires.
    let iconset = ($stage | path join Rustty.iconset)
    let icon = ($root | path join macos Assets.xcassets AppIconImage.imageset macOS-AppIcon-1024px.png)
    mkdir $iconset
    for size in [16 32 128 256 512] {
        for scale in [1 2] {
            let pixels = $size * $scale
            let suffix = if $scale == 2 { '@2x' } else { '' }
            let name = $"icon_($size)x($size)($suffix).png"
            do --capture-errors { ^sips -z $pixels $pixels $icon --out ($iconset | path join $name) } | ignore
        }
    }
    do --capture-errors { ^iconutil -c icns $iconset -o ($resources | path join Rustty.icns) }
    do --capture-errors { ^plutil -lint ($contents | path join Info.plist) }
    do --capture-errors { ^codesign --force --sign - $app }
    do --capture-errors { ^codesign --verify --strict $app }

    let bundle = ($output | path join Rustty.app)
    # Leave the previous successful bundle intact until the replacement passes
    # resource compilation and signature verification.
    rm --recursive --force $bundle
    mv $app $bundle
    rm --recursive --force $stage
    print $bundle
}
