# Builds release binaries and packages them: portable zip, and an installer when Inno Setup is installed.
# Usage: scripts\package.ps1 [-Sign]
#   -Sign  signs the executables with signtool; the certificate is picked by thumbprint from the
#          DRIFTWIN_SIGN_THUMBPRINT environment variable (store: current user, "My").
param([switch]$Sign)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

$ver = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches.Groups[1].Value
Write-Host "DriftWin $ver"

cargo build --release --workspace
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$bins = 'driftwin.exe', 'driftwin-guard.exe', 'driftwin-settings.exe', 'driftwinctl.exe'

if ($Sign) {
    $thumb = $env:DRIFTWIN_SIGN_THUMBPRINT
    if (-not $thumb) { throw 'set DRIFTWIN_SIGN_THUMBPRINT to the code signing certificate thumbprint' }
    $signtool = (Get-Command signtool.exe -ErrorAction SilentlyContinue).Source
    if (-not $signtool) { throw 'signtool.exe not found (install the Windows SDK)' }
    foreach ($b in $bins) {
        & $signtool sign /sha1 $thumb /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 "target\release\$b"
        if ($LASTEXITCODE -ne 0) { throw "signing $b failed" }
    }
}

New-Item -ItemType Directory -Force dist | Out-Null
$stage = "dist\DriftWin-$ver"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$stage\docs" | Out-Null
foreach ($b in $bins) { Copy-Item "target\release\$b" $stage }
Copy-Item config.example.toml, LICENSE, README.md $stage
Copy-Item docs\CONFIG.md "$stage\docs"
$zip = "dist\DriftWin-$ver-win-x64.zip"
Remove-Item $zip -Force -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Write-Host "portable: $zip"

$iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) { $iscc = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" }
if (Test-Path $iscc) {
    & $iscc "/DAppVersion=$ver" installer\driftwin.iss
    if ($LASTEXITCODE -ne 0) { throw 'ISCC failed' }
    Write-Host "installer: dist\DriftWin-$ver-setup.exe"
} else {
    Write-Host 'Inno Setup not found: installer skipped (winget install JRSoftware.InnoSetup)'
}
