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
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../rustty-font/resources/JetBrainsMono-OFL.txt') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot '../rustty-font/resources/NerdFontsSymbols-LICENSE.txt') -Destination $licenses
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'resources/README.md') -Destination (Join-Path $licenses 'Resources.md')
    @'
Run rustty.exe to open the terminal.
Run rustty.exe --register to add this location to the Start Menu and enable toast activation.
Run rustty.exe --unregister before removing or moving the registered application.
Configuration: %APPDATA%\Rustty\rustty.txt
Workspace state: %LOCALAPPDATA%\Rustty\workspace.json
Only the default shell command is read from Windows Terminal; Rustty command overrides win.
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
