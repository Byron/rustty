# Build the explicit standalone owner with the same pinned ConPTY as Rustty.
[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$Offline,
    [switch]$SkipBuild,
    [string]$ConPtyPackage
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$rustRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
$targetRoot = Join-Path $rustRoot 'target'
$profile = if ($Release) { 'release' } else { 'debug' }
$triple = 'x86_64-pc-windows-msvc'
$output = Join-Path $targetRoot $profile
$bundle = Join-Path $output 'tmuxx'
$stage = Join-Path $output ('tmuxx-stage-' + [Guid]::NewGuid().ToString('N'))
$previous = Join-Path $output ('tmuxx-previous-' + [Guid]::NewGuid().ToString('N'))

function Assert-BuildPath([string]$Candidate) {
    $absolute = [IO.Path]::GetFullPath($Candidate)
    $allowed = [IO.Path]::GetFullPath($targetRoot).TrimEnd('\') + '\'
    if (!$absolute.StartsWith($allowed, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to change a path outside the build directory: $absolute"
    }
    $ancestor = $absolute
    while ($ancestor -and $ancestor.Length -ge $targetRoot.Length) {
        if ((Test-Path -LiteralPath $ancestor) -and
            ((Get-Item -LiteralPath $ancestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Refusing to modify a build directory through a junction: $ancestor"
        }
        $ancestor = [IO.Path]::GetDirectoryName($ancestor)
    }
}
function Remove-BuildPath([string]$Candidate) {
    Assert-BuildPath $Candidate
    if (Test-Path -LiteralPath $Candidate) { Remove-Item -LiteralPath $Candidate -Recurse -Force }
}

if (!$SkipBuild) {
    $cargoArguments = @('+1.95.0', 'build', '--manifest-path', (Join-Path $rustRoot 'Cargo.toml'),
        '--target-dir', $targetRoot, '--target', $triple, '--locked', '-p', 'tmuxx-cli')
    if ($Release) { $cargoArguments += '--release' }
    if ($Offline) { $cargoArguments += '--offline' }
    $savedFlags = $env:CARGO_ENCODED_RUSTFLAGS
    $flags = if ($null -ne $savedFlags) { @($savedFlags -split [char]31) }
        elseif ($env:RUSTFLAGS) { @($env:RUSTFLAGS -split '\s+' | Where-Object { $_ }) } else { @() }
    $env:CARGO_ENCODED_RUSTFLAGS = ($flags + @('-C', 'target-feature=+crt-static')) -join [char]31
    try {
        & cargo @cargoArguments
        if ($LASTEXITCODE -ne 0) { throw "Cargo failed with exit code $LASTEXITCODE" }
    } finally { $env:CARGO_ENCODED_RUSTFLAGS = $savedFlags }
}
$executable = Join-Path (Join-Path (Join-Path $targetRoot $triple) $profile) 'tmuxx.exe'
if (!(Test-Path -LiteralPath $executable -PathType Leaf)) { throw "Build the standalone executable first: $executable" }
$manifest = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'conpty.json') -Raw | ConvertFrom-Json
if (!$ConPtyPackage) {
    $ConPtyPackage = Join-Path $targetRoot "conpty/$($manifest.package).$($manifest.version).nupkg"
}
Assert-BuildPath $stage
Assert-BuildPath $bundle
Assert-BuildPath $previous
try {
    $resources = Join-Path $stage 'resources'
    $packageArguments = @((Join-Path $PSScriptRoot 'package.py'), '--package', $ConPtyPackage,
        '--destination', (Join-Path $resources 'conpty'))
    if ($Offline) { $packageArguments += '--offline' }
    & python @packageArguments
    if ($LASTEXITCODE -ne 0) { throw "ConPTY staging failed with exit code $LASTEXITCODE" }
    Copy-Item -LiteralPath $executable -Destination (Join-Path $stage 'tmuxx.exe')
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'ConPTY-LICENSE.txt') -Destination $resources
    @'
Keep resources with tmuxx.exe. Both ConPTY binaries are verified before pane creation.
Run tmuxx.exe --help for explicit standalone ownership and attachment commands.
SSH clients use a fixed tmuxx --native-stdio exec command without a PTY.
The bridge attaches to an existing per-user instance; it never starts an owner.
'@ | Set-Content -LiteralPath (Join-Path $stage 'README.txt') -Encoding UTF8
    if (Test-Path -LiteralPath $bundle) { [IO.Directory]::Move($bundle, $previous) }
    try { [IO.Directory]::Move($stage, $bundle) }
    catch {
        if (Test-Path -LiteralPath $previous) { [IO.Directory]::Move($previous, $bundle) }
        throw
    }
    try { Remove-BuildPath $previous }
    catch { Write-Warning "New bundle installed; an active owner still uses $previous. Close it before deleting that directory." }
} finally { Remove-BuildPath $stage }
Write-Output $bundle
