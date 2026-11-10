# Packages a built Restrafe (game\target\release) into dist\Restrafe-<version>-windows-x64.zip.
#   pwsh scripts/package.ps1 -Version v0.1.0      (after: cd game; cargo build --release --bins --locked)
param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$release = Join-Path $root 'game\target\release'
$name = "Restrafe-$Version-windows-x64"
$dist = Join-Path $root 'dist'
$stage = Join-Path $dist $name
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force (Join-Path $stage 'assets') | Out-Null

Copy-Item (Join-Path $release 'restrafe.exe') (Join-Path $stage 'Restrafe.exe')
Copy-Item (Join-Path $release 'setup.exe') (Join-Path $stage 'setup.exe')
Copy-Item -Recurse (Join-Path $root 'game\assets\shaders') (Join-Path $stage 'assets\shaders')
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $stage 'LICENSE.txt')
Copy-Item (Join-Path $root 'scripts\README-download.txt') (Join-Path $stage 'README.txt')
# the py launcher when installed (a bare "python" can be Windows' Store placeholder), else python
$python = if (Get-Command py -ErrorAction SilentlyContinue) { 'py' } else { 'python' }
& $python (Join-Path $root 'scripts\third_party_notices.py') (Join-Path $stage 'THIRD_PARTY_NOTICES.txt')
if ($LASTEXITCODE -ne 0) { throw 'notices failed' }

$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $stage -DestinationPath $zip
Write-Host "Packaged $zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)"
