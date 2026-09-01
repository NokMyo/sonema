$ErrorActionPreference = "Stop"

$workspace = Split-Path -Parent $PSScriptRoot
$output = Join-Path $workspace "release"
$bundle = Join-Path $output "Febius-Sonema-0.1.0-windows-x64"

Push-Location $workspace
try {
    cargo test --workspace
    cargo build -p sonema-app --release
    New-Item -ItemType Directory -Force -Path $bundle | Out-Null
    Copy-Item "target/release/sonema.exe" (Join-Path $bundle "Febius Sonema.exe")
    Copy-Item "README.md" $bundle
    Copy-Item "LICENSE.md" $bundle
    $archive = Join-Path $output "Febius-Sonema-0.1.0-windows-x64.zip"
    if (Test-Path $archive) { Remove-Item $archive }
    Compress-Archive -Path (Join-Path $bundle "*") -DestinationPath $archive
    Write-Host "Created $archive"
}
finally {
    Pop-Location
}
