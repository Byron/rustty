# List recipes without building or installing anything.
default:
    @just --list

# Build and install a signed release bundle, retaining the previous application.
[macos]
rustty-install-macos:
    #!/usr/bin/env bash
    set -euo pipefail
    nu crates/rustty-app/build.nu --release --sign 'Rustty Local Signing'
    app='/Applications/Rustty.app'
    stage=$(mktemp -d '/Applications/.Rustty-install.XXXXXX')
    trap 'rm -rf "$stage"' EXIT
    ditto target/release/Rustty.app "$stage/Rustty.app"
    codesign --verify --deep --strict "$stage/Rustty.app"
    backup=''
    if [[ -e "$app" || -L "$app" ]]; then
        backup="/Applications/Rustty.previous.$(date +%Y%m%d-%H%M%S)-$$.app"
        [[ ! -e "$backup" && ! -L "$backup" ]]
        mv "$app" "$backup"
    fi
    if ! mv "$stage/Rustty.app" "$app"; then
        if [[ -n "$backup" ]]; then
            mv "$backup" "$app"
        fi
        exit 1
    fi
    printf 'Installed %s\n' "$app"
    if [[ -n "$backup" ]]; then
        printf 'Previous bundle: %s\n' "$backup"
    fi
