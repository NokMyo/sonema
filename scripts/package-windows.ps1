$ErrorActionPreference = "Stop"

$workspace = Split-Path -Parent $PSScriptRoot
$output = Join-Path $workspace "release"
$bundle = Join-Path $output "Febius-Sonema-0.1.0-windows-x64"

Push-Location $workspace
try {
    cargo test --locked --workspace
    cargo build --locked -p sonema-app --release
    $notices = Join-Path $output "THIRD-PARTY-NOTICES.txt"
    & (Join-Path $PSScriptRoot "generate-third-party-notices.ps1") -Destination $notices
    New-Item -ItemType Directory -Force -Path $bundle | Out-Null
    Copy-Item "target/release/sonema.exe" (Join-Path $bundle "Febius Sonema.exe")
    Copy-Item "README.md" $bundle
    Copy-Item "LICENSE.md" $bundle
    Copy-Item $notices $bundle
    $archive = Join-Path $output "Febius-Sonema-0.1.0-windows-x64.zip"
    if (Test-Path $archive) { Remove-Item $archive }
    Compress-Archive -Path (Join-Path $bundle "*") -DestinationPath $archive
    Write-Host "Created $archive"
}
finally {
    Pop-Location
}
