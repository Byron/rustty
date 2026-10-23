# Build a portable Windows application, optionally installing it on the Desktop.
# Registration is explicit and per-user.
[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$Offline,
    [switch]$Register,
    [switch]$SkipBuild,
    [switch]$Install,
    [string]$InstallDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
# Indirect launches from PowerShell 7 (for example, through just) can pass its
# PSModulePath to Windows PowerShell 5.1. Use this host's built-in hash helper.
Import-Module (Join-Path $PSHOME 'Modules/Microsoft.PowerShell.Utility')

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$targetRoot = Join-Path $repository 'target'
$profile = if ($Release) { 'release' } else { 'debug' }
$triple = 'x86_64-pc-windows-msvc'
$buildOutput = Join-Path (Join-Path $targetRoot $triple) $profile
$output = Join-Path $targetRoot $profile
$bundle = Join-Path $output 'Rustty'
if ($InstallDirectory -and !$Install) {
    throw '-InstallDirectory requires -Install.'
}
if ($Install) {
    if (!$InstallDirectory) {
        # Honor Windows' configured Desktop location instead of assuming that
        # it is directly under the user's profile directory.
        $desktop = [Environment]::GetFolderPath([Environment+SpecialFolder]::DesktopDirectory)
        if (!$desktop) { throw 'Windows did not report a Desktop directory. Supply -InstallDirectory.' }
        $InstallDirectory = Join-Path $desktop 'Rustty'
    }
    $bundle = [IO.Path]::GetFullPath($InstallDirectory).TrimEnd('\', '/')
    $output = [IO.Path]::GetDirectoryName($bundle)
    if (!$output -or ![IO.Path]::GetFileName($bundle)) {
        throw '-InstallDirectory must name an application folder, not a filesystem root.'
    }
}
$bundleName = [IO.Path]::GetFileName($bundle)
$stage = Join-Path $output ('.' + $bundleName + '-install.' + [Guid]::NewGuid().ToString('N'))
$previous = Join-Path $output ($bundleName + '.previous.' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + [Guid]::NewGuid().ToString('N'))

function Assert-ManagedPath([string]$Candidate, [string]$Root) {
    $absolute = [IO.Path]::GetFullPath($Candidate)
    $allowed = [IO.Path]::GetFullPath($Root).TrimEnd('\') + '\'
    if (!$absolute.StartsWith($allowed, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to change a path outside ${Root}: $absolute"
    }
    # Check the root's ancestors too: the explicitly selected parent must not
    # redirect staging, publication or cleanup through a directory junction.
    $parent = $absolute
    while ($parent) {
        $item = Get-Item -LiteralPath $parent -Force -ErrorAction SilentlyContinue
        if ($null -ne $item -and ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Refusing to modify a path through a junction or symbolic link: $parent"
        }
        $parent = [IO.Path]::GetDirectoryName($parent)
    }
}

function Assert-ManagedTree([string]$Candidate, [string]$Root) {
    Assert-ManagedPath $Candidate $Root
    # Windows PowerShell 5.1 can follow nested junctions during recursive
    # removal. Inspect each directory ourselves without traversing any links.
    if (Test-Path -LiteralPath $Candidate -PathType Container) {
        $pending = [Collections.Generic.Stack[string]]::new()
        $pending.Push($Candidate)
        while ($pending.Count) {
            foreach ($item in Get-ChildItem -LiteralPath $pending.Pop() -Force) {
                if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                    throw "Refusing to modify a bundle containing a junction or symbolic link: $($item.FullName)"
                }
                if ($item.PSIsContainer) { $pending.Push($item.FullName) }
            }
        }
    }
}

function Assert-BuildPath([string]$Candidate) {
    Assert-ManagedPath $Candidate $targetRoot
}

function Remove-ManagedPath([string]$Candidate, [string]$Root) {
    Assert-ManagedTree $Candidate $Root
    if (Test-Path -LiteralPath $Candidate) {
        Remove-Item -LiteralPath $Candidate -Recurse -Force
    }
}

function Move-BundleDirectory([string]$Source, [string]$Destination) {
    Assert-ManagedTree $Source $output
    Assert-ManagedPath $Destination $output
    if ([IO.Path]::GetDirectoryName($Source) -ne [IO.Path]::GetDirectoryName($Destination)) {
        throw 'Bundle publication must stay within its staging directory.'
    }
    # Unlike Move-Item, Rename-Item fails if the destination already exists;
    # it cannot accidentally nest the stage inside a concurrently created app.
    Rename-Item -LiteralPath $Source -NewName ([IO.Path]::GetFileName($Destination))
}

Assert-ManagedPath $stage $output
Assert-ManagedTree $bundle $output
Assert-ManagedPath $previous $output
if (Test-Path -LiteralPath $bundle -PathType Leaf) {
    throw "The bundle destination is a file, not a directory: $bundle"
}

# The in-box ConPTY can forward DEC 2026's end marker before its buffered
# screen/cursor update. The current standalone runtime preserves byte order.
function Get-ConPtyPackage {
    $version = '1.24.260710001'
    $name = "microsoft.windows.console.conpty.$version.nupkg"
    $cache = Join-Path $targetRoot 'conpty'
    $package = Join-Path $cache $name
    $sha256 = '175640566A3B59C4B132070EE96C2C77E5AB7EDD2E92732A5EB3610BBF63D90E'
    Assert-BuildPath $package
    if (!(Test-Path -LiteralPath $package -PathType Leaf)) {
        if ($Offline) {
            throw "ConPTY $version is not cached. Run build.ps1 once without -Offline to download it."
        }
        New-Item -ItemType Directory -Path $cache -Force | Out-Null
        $download = Join-Path $cache ([Guid]::NewGuid().ToString('N') + '.download')
        Assert-BuildPath $download
        try {
            $uri = "https://api.nuget.org/v3-flatcontainer/microsoft.windows.console.conpty/$version/$name"
            Invoke-WebRequest -Uri $uri -OutFile $download -TimeoutSec 60
            if ((Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash -ne $sha256) {
                throw 'Downloaded ConPTY package failed its SHA-256 check.'
            }
            Move-Item -LiteralPath $download -Destination $package
        } finally {
            Remove-ManagedPath $download $targetRoot
        }
    }
    if ((Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash -ne $sha256) {
        throw "Cached ConPTY package failed its SHA-256 check: $package"
    }
    return $package
}

function Install-ConPty([string]$Package, [string]$Destination) {
    Assert-ManagedPath $Destination $output
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($Package)
    try {
        # Extract only the pinned x64 runtime, without trusting archive paths.
        foreach ($path in @('runtimes/win-x64/native/conpty.dll', 'build/native/runtimes/x64/OpenConsole.exe')) {
            $entry = $archive.GetEntry($path)
            if ($null -eq $entry) { throw "ConPTY package is missing $path" }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, (Join-Path $Destination $entry.Name))
        }
    } finally {
        $archive.Dispose()
    }
}

$conptyPackage = Get-ConPtyPackage

if (!$SkipBuild) {
    $cargoArguments = @('build', '--manifest-path', (Join-Path $repository 'Cargo.toml'),
        '--target-dir', $targetRoot, '--target', $triple,
        '--locked', '-p', 'rustty-app', '--bin', 'rustty', '-p', 'tmuxx-cli', '--bin', 'tmuxx')
    if ($Release) { $cargoArguments += '--release' }
    if ($Offline) { $cargoArguments += '--offline' }
    # Explicit target keeps host proc-macros dynamic while the shipped executable
    # embeds its C runtime instead of requiring a separate VC++ redistributable.
    $savedFlags = $env:CARGO_ENCODED_RUSTFLAGS
    $flags = if ($null -ne $savedFlags) {
        @($savedFlags -split [char]31)
    } elseif ($env:RUSTFLAGS) {
        @($env:RUSTFLAGS -split '\s+' | Where-Object { $_ })
    } else { @() }
    $env:CARGO_ENCODED_RUSTFLAGS = ($flags + @('-C', 'target-feature=+crt-static')) -join [char]31
    try {
        & cargo @cargoArguments
        if ($LASTEXITCODE -ne 0) { throw "Cargo failed with exit code $LASTEXITCODE" }
    } finally {
        $env:CARGO_ENCODED_RUSTFLAGS = $savedFlags
    }
}

foreach ($name in @('rustty', 'tmuxx')) {
    $executable = Join-Path $buildOutput "$name.exe"
    if (!(Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "Build the Windows executable first: $executable"
    }
}
$installed = $false
$backedUp = $false
try {
    $resources = Join-Path $stage 'resources'
    $licenses = Join-Path $resources 'licenses'
    New-Item -ItemType Directory -Path $licenses -Force | Out-Null
    Install-ConPty $conptyPackage (Join-Path $resources 'conpty')
    foreach ($name in @('rustty', 'tmuxx')) {
        Copy-Item -LiteralPath (Join-Path $buildOutput "$name.exe") -Destination $stage
        $symbols = Join-Path $buildOutput "$name.pdb"
        if (Test-Path -LiteralPath $symbols) {
            Copy-Item -LiteralPath $symbols -Destination $stage
        }
    }
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/themes') -Destination $resources -Recurse -Force
    Copy-Item -LiteralPath (Join-Path $repository 'src/shell-integration') -Destination $resources -Recurse -Force
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/ghostty.terminfo') -Destination $resources
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/icons/rustty.ico') -Destination (Join-Path $resources 'rustty.ico')
    # Git's autocrlf also affects extensionless scripts. The bundle always
    # contains UTF-8/LF integration, including hidden zsh startup files.
    $utf8 = [Text.UTF8Encoding]::new($false)
    Get-ChildItem -LiteralPath (Join-Path $resources 'shell-integration') -File -Recurse -Force | ForEach-Object {
        $content = [IO.File]::ReadAllText($_.FullName)
        [IO.File]::WriteAllText($_.FullName, $content.Replace("`r`n", "`n"), $utf8)
    }
    Copy-Item -LiteralPath (Join-Path $repository 'LICENSE') -Destination (Join-Path $licenses 'Ghostty-MIT.txt')
    Copy-Item -LiteralPath (Join-Path $repository 'tmuxx/COPYING') -Destination (Join-Path $licenses 'tmuxx-COPYING.txt')
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/iTerm2-Color-Schemes-LICENSE.txt') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/ConPTY-LICENSE.txt') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../rustty-font/resources/JetBrainsMono-OFL.txt') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../rustty-font/resources/NerdFontsSymbols-LICENSE.txt') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/README.md') -Destination (Join-Path $licenses 'Resources.md')
    @'
Run rustty.exe to open the terminal.
Run tmuxx.exe for console commands or --native-stdio SSH access.
Keep rustty.exe, tmuxx.exe, and resources together; they share the required ConPTY runtime.
Run rustty.exe --register to add this location to the Start Menu and enable toast activation.
Run rustty.exe --unregister before removing or moving the registered application.
Configuration: %APPDATA%\Rustty\rustty.txt
Workspace state: %LOCALAPPDATA%\Rustty\workspace.json
Only the default shell command is read from Windows Terminal; Rustty command overrides win.
Renderer: automatic hardware GPU selection with CPU fallback at startup.
Use --renderer=software or --renderer=gpu to force a backend, or set renderer in rustty.txt and restart.
'@ | Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Encoding UTF8

    # Keep the last complete bundle recoverable until staging succeeds.
    if (Test-Path -LiteralPath $bundle) {
        try {
            Move-BundleDirectory $bundle $previous
            $backedUp = $true
        } catch {
            throw "Could not move the existing bundle; it is unchanged at $bundle. A process may be using this folder as its working directory or holding a file open. Leave that directory or close the process and retry. $($_.Exception.Message)"
        }
    }
    try {
        Move-BundleDirectory $stage $bundle
        $installed = $true
    } catch {
        $publishFailure = $_
        if ($backedUp) {
            try {
                Move-BundleDirectory $previous $bundle
            } catch {
                throw "Publishing the bundle failed, and the previous bundle could not be restored. It is retained at $previous. Publish error: $publishFailure. Restore error: $_"
            }
        }
        throw $publishFailure
    }
    if ($Install) {
        if ($backedUp) { Write-Host "Previous bundle: $previous" }
    } else {
        try {
            Remove-ManagedPath $previous $output
        } catch {
            # Windows can rename a loaded executable or DLL but cannot delete it.
            Write-Warning "Rustty was built successfully, but the previous bundle could not be removed: $previous. Close applications using it before deleting that directory. $($_.Exception.Message)"
        }
    }
} finally {
    Remove-ManagedPath $stage $output
}

if ($Register -and $installed) {
    & (Join-Path $bundle 'rustty.exe') --register
    if ($LASTEXITCODE -ne 0) { throw "Registration failed with exit code $LASTEXITCODE" }
}
Write-Output $bundle
