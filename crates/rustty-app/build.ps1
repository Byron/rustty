# Build a portable Windows application. Registration is explicit and per-user.
[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$Offline,
    [switch]$Register,
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repository = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$targetRoot = Join-Path $repository 'target'
$profile = if ($Release) { 'release' } else { 'debug' }
$triple = 'x86_64-pc-windows-msvc'
$buildOutput = Join-Path (Join-Path $targetRoot $triple) $profile
$output = Join-Path $targetRoot $profile
$bundle = Join-Path $output 'Rustty'
$stage = Join-Path $output ('rustty-stage-' + [Guid]::NewGuid().ToString('N'))
$previous = Join-Path $output ('rustty-previous-' + [Guid]::NewGuid().ToString('N'))

function Assert-BuildPath([string]$Candidate) {
    $absolute = [IO.Path]::GetFullPath($Candidate)
    $allowed = [IO.Path]::GetFullPath($targetRoot).TrimEnd('\') + '\'
    if (!$absolute.StartsWith($allowed, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to change a path outside the build directory: $absolute"
    }
    # Do not follow a user-created directory junction out of the build tree.
    $parent = $absolute
    while ($parent -and $parent.Length -ge $targetRoot.Length) {
        if (Test-Path -LiteralPath $parent) {
            $item = Get-Item -LiteralPath $parent -Force
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
                throw "Refusing to modify a build directory through a junction: $parent"
            }
        }
        $parent = [IO.Path]::GetDirectoryName($parent)
    }
}

function Remove-BuildPath([string]$Candidate) {
    Assert-BuildPath $Candidate
    if (Test-Path -LiteralPath $Candidate) {
        Remove-Item -LiteralPath $Candidate -Recurse -Force
    }
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
            Remove-BuildPath $download
        }
    }
    if ((Get-FileHash -LiteralPath $package -Algorithm SHA256).Hash -ne $sha256) {
        throw "Cached ConPTY package failed its SHA-256 check: $package"
    }
    return $package
}

function Install-ConPty([string]$Package, [string]$Destination) {
    Assert-BuildPath $Destination
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
        '--locked', '-p', 'rustty-app', '--bin', 'rustty')
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

$executable = Join-Path $buildOutput 'rustty.exe'
if (!(Test-Path -LiteralPath $executable -PathType Leaf)) {
    throw "Build the Windows executable first: $executable"
}
Assert-BuildPath $stage
Assert-BuildPath $bundle
Assert-BuildPath $previous
$installed = $false
try {
    $resources = Join-Path $stage 'resources'
    $licenses = Join-Path $resources 'licenses'
    New-Item -ItemType Directory -Path $licenses -Force | Out-Null
    Install-ConPty $conptyPackage (Join-Path $resources 'conpty')
    Copy-Item -LiteralPath $executable -Destination (Join-Path $stage 'rustty.exe')
    $symbols = Join-Path $buildOutput 'rustty.pdb'
    if (Test-Path -LiteralPath $symbols) {
        Copy-Item -LiteralPath $symbols -Destination $stage
    }
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/themes') -Destination $resources -Recurse -Force
    Copy-Item -LiteralPath (Join-Path $repository 'src/shell-integration') -Destination $resources -Recurse -Force
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/ghostty.terminfo') -Destination $resources
    Copy-Item -LiteralPath (Join-Path $repository 'dist/windows/ghostty.ico') -Destination (Join-Path $resources 'rustty.ico')
    # Git's autocrlf also affects extensionless scripts. The bundle always
    # contains UTF-8/LF integration, including hidden zsh startup files.
    $utf8 = [Text.UTF8Encoding]::new($false)
    Get-ChildItem -LiteralPath (Join-Path $resources 'shell-integration') -File -Recurse -Force | ForEach-Object {
        $content = [IO.File]::ReadAllText($_.FullName)
        [IO.File]::WriteAllText($_.FullName, $content.Replace("`r`n", "`n"), $utf8)
    }
    Copy-Item -LiteralPath (Join-Path $repository 'LICENSE') -Destination (Join-Path $licenses 'Ghostty-MIT.txt')
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/iTerm2-Color-Schemes-LICENSE.txt') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/ConPTY-LICENSE.txt') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../rustty-font/resources/JetBrainsMono-OFL.txt') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../rustty-font/resources/NerdFontsSymbols-LICENSE.txt') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/README.md') -Destination (Join-Path $licenses 'Resources.md')
    @'
Run rustty.exe to open the terminal.
Keep the resources folder with rustty.exe; it contains the required ConPTY runtime.
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
        Move-Item -LiteralPath $bundle -Destination $previous
    }
    try {
        Move-Item -LiteralPath $stage -Destination $bundle
        $installed = $true
    } catch {
        if (Test-Path -LiteralPath $previous) {
            Move-Item -LiteralPath $previous -Destination $bundle
        }
        throw
    }
    try {
        Remove-BuildPath $previous
    } catch {
        # Windows can rename a loaded executable or DLL but cannot delete it.
        # The new bundle is already installed; keep the old runtime for sessions
        # that still use it instead of reporting this successful build as failed.
        Write-Warning "Rustty was built successfully, but the previous bundle could not be removed: $previous. Close applications using it before deleting that directory. $($_.Exception.Message)"
    }
} finally {
    Remove-BuildPath $stage
}

if ($Register -and $installed) {
    & (Join-Path $bundle 'rustty.exe') --register
    if ($LASTEXITCODE -ne 0) { throw "Registration failed with exit code $LASTEXITCODE" }
}
Write-Output $bundle
