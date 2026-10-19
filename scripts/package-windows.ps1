# Packages a release build as Qwerio-<version>-windows-x64.msi and a portable
# Qwerio-<version>-windows-x64.zip. Neither is code-signed, so SmartScreen
# warns on first launch until the user chooses "Run anyway".
#
# Requires the WiX Toolset v5 CLI on PATH:
#   dotnet tool install --global wix --version 5.0.2
#
# Usage: pwsh scripts/package-windows.ps1 -Binary <qwerio.exe> -Output <directory>
param(
    [Parameter(Mandatory)] [string] $Binary,
    [Parameter(Mandatory)] [string] $Output
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$metadata = cargo metadata --manifest-path "$root/Cargo.toml" --no-deps --format-version 1 | ConvertFrom-Json
$version = ($metadata.packages | Where-Object name -eq 'qwerio-desktop').version
$binaryPath = (Resolve-Path $Binary).Path
$name = "Qwerio-$version-windows-x64"

New-Item -ItemType Directory -Force -Path $Output | Out-Null

wix build "$root/packaging/windows/qwerio.wxs" `
    -arch x64 `
    -d "Version=$version" `
    -d "BinaryPath=$binaryPath" `
    -d "IconPath=$root/packaging/icons/qwerio.ico" `
    -o "$Output/$name.msi"
if ($LASTEXITCODE -ne 0) { throw "wix build failed with exit code $LASTEXITCODE" }

$staging = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid())
New-Item -ItemType Directory -Path "$staging/Qwerio" | Out-Null
try {
    Copy-Item $binaryPath "$staging/Qwerio/qwerio.exe"
    Compress-Archive -Path "$staging/Qwerio" -DestinationPath "$Output/$name.zip" -Force
}
finally {
    Remove-Item -Recurse -Force $staging
}
