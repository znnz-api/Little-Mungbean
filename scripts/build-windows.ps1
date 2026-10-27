# Windows build.
param(
    [string]$Version = 'v1.1',
    [string]$OutputPath = (Join-Path $PSScriptRoot "..\release\windows\Little-Mungbean-$Version.exe")
)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$destination = [System.IO.Path]::GetFullPath($OutputPath)
if (Test-Path -LiteralPath $destination) { throw "Refusing to overwrite existing release: $destination" }
$previousVersion = [Environment]::GetEnvironmentVariable('LITTLE_MUNGBEAN_WINDOWS_VERSION', 'Process')
Push-Location -LiteralPath $projectRoot
try {
    $env:LITTLE_MUNGBEAN_WINDOWS_VERSION = $Version
    & cargo build --locked --release --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw "Build failed: $LASTEXITCODE" }
    $targetRoot = if ($env:CARGO_TARGET_DIR) {
        [System.IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)
    } else { Join-Path $projectRoot 'target' }
    $binary = Join-Path $targetRoot 'x86_64-pc-windows-msvc\release\little-mungbean.exe'
    New-Item -ItemType Directory -Force -Path ([System.IO.Path]::GetDirectoryName($destination)) | Out-Null
    Copy-Item -LiteralPath $binary -Destination $destination
    Write-Host "Windows build: $destination"
} finally {
    [Environment]::SetEnvironmentVariable('LITTLE_MUNGBEAN_WINDOWS_VERSION', $previousVersion, 'Process')
    Pop-Location
}
