<#
Builds mage-agent, installs it next to mage.exe, and registers it as a Buzz
custom harness. Re-run after `just reset` (which wipes custom_harnesses).

  .\install.ps1                          # dev Buzz (xyz.block.buzz.app.dev)
  .\install.ps1 -MageSource ..\buzz\.scratch\mage
  .\install.ps1 -App prod            # installed Buzz (xyz.block.buzz.app)
#>
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Programs\mage'),
    [string]$MageSource,
    [ValidateSet('dev', 'prod')]
    [string]$App = 'dev'
)
$ErrorActionPreference = 'Stop'

cargo build --release --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

New-Item -ItemType Directory -Force $InstallDir | Out-Null
$installed = Join-Path $InstallDir 'mage-agent.exe'
foreach ($old in Get-ChildItem $InstallDir -Filter 'mage-agent.*.old') {
    try { Remove-Item $old.FullName -Force -ErrorAction Stop } catch {}
}
$built = Join-Path $PSScriptRoot 'target\release\mage-agent.exe'
try {
    Copy-Item $built $installed -Force -ErrorAction Stop
} catch {
    # A running agent locks the exe, but Windows still allows renaming it.
    Move-Item $installed (Join-Path $InstallDir "mage-agent.$(Get-Date -Format yyyyMMddHHmmss).old")
    Copy-Item $built $installed
}

if ($MageSource) {
    foreach ($file in 'mage.exe', 'config.ini', 'presets.ini') {
        $source = Join-Path $MageSource $file
        if (Test-Path $source) { Copy-Item $source $InstallDir -Force }
    }
}
if (-not (Test-Path (Join-Path $InstallDir 'mage.exe'))) {
    Write-Warning "mage.exe is not in $InstallDir; pass -MageSource or copy it there with config.ini and presets.ini."
}

$appId = if ($App -eq 'dev') { 'xyz.block.buzz.app.dev' } else { 'xyz.block.buzz.app' }
$harnessDir = Join-Path $env:APPDATA "$appId\custom_harnesses"
New-Item -ItemType Directory -Force $harnessDir | Out-Null
$definition = [ordered]@{
    id          = 'mage'
    label       = 'Mage'
    command     = (Join-Path $InstallDir 'mage-agent.exe')
    args        = @()
    env         = @{}
    installHint = 'Run install.ps1 from the mage-agent repository.'
}
$json = $definition | ConvertTo-Json
$harnessFile = Join-Path $harnessDir 'mage.json'
[System.IO.File]::WriteAllText($harnessFile, $json, (New-Object System.Text.UTF8Encoding $false))

Write-Host "Installed $InstallDir\mage-agent.exe"
Write-Host "Registered $harnessFile"
