param(
    [Parameter(Mandatory = $true)]
    [string]$Destination
)

$ErrorActionPreference = "Stop"

$metadata = cargo metadata --format-version 1 --locked | ConvertFrom-Json
$sections = [System.Collections.Generic.List[string]]::new()
$sections.Add("FEBIUS SONEMA - THIRD-PARTY SOFTWARE NOTICES`n")
$sections.Add("Generated from the exact dependency set in Cargo.lock.`n")

$packages = $metadata.packages |
    Where-Object { $null -ne $_.source } |
    Sort-Object name, version

foreach ($package in $packages) {
    $header = "--------------------------------------------------------------------------------`n" +
        "$($package.name) $($package.version)`n" +
        "Declared license: $($package.license)`n" +
        "Repository: $($package.repository)`n"
    $sections.Add($header)

    $manifestDirectory = Split-Path -Parent $package.manifest_path
    $licenseFiles = @()
    if ($package.license_file) {
        $licensePath = if ([System.IO.Path]::IsPathRooted($package.license_file)) {
            $package.license_file
        } else {
            Join-Path $manifestDirectory $package.license_file
        }
        if (Test-Path -LiteralPath $licensePath) {
            $licenseFiles += Get-Item -LiteralPath $licensePath
        }
    }
    $licenseFiles += Get-ChildItem -LiteralPath $manifestDirectory -File |
        Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)(\.|-|$)' }
    $licenseFiles = $licenseFiles | Sort-Object FullName -Unique

    if ($licenseFiles.Count -eq 0) {
        $sections.Add("No standalone license file was included in the published crate.`n")
        continue
    }
    foreach ($licenseFile in $licenseFiles) {
        $sections.Add("`n[$($licenseFile.Name)]`n")
        $sections.Add((Get-Content -LiteralPath $licenseFile.FullName -Raw))
        $sections.Add("`n")
    }
}

$parent = Split-Path -Parent $Destination
New-Item -ItemType Directory -Force -Path $parent | Out-Null
$encoding = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText($Destination, ($sections -join "`n"), $encoding)
Write-Host "Created $Destination"
