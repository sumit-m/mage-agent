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
    foreach ($file in 'mage.exe', 'config.ini', 'presets.ini', 'models.ini') {
        $source = Join-Path $MageSource $file
        if (Test-Path $source) { Copy-Item $source $InstallDir -Force }
    }
}
if (-not (Test-Path (Join-Path $InstallDir 'mage.exe'))) {
    Write-Warning "mage.exe is not in $InstallDir; pass -MageSource or copy it there with config.ini, presets.ini and models.ini."
}

$appId = if ($App -eq 'dev') { 'xyz.block.buzz.app.dev' } else { 'xyz.block.buzz.app' }
$harnessDir = Join-Path $env:APPDATA "$appId\custom_harnesses"
New-Item -ItemType Directory -Force $harnessDir | Out-Null
# Buzz's message-box panel. Values travel by control id; mage-agent maps them
# to mage flags. `--refresh-panel` adds the Model, Aspect ratio and Resolution
# menus from models.ini and the Preset menu from presets.ini, here and each
# time one of those menus opens.
$controls = @()
# Filled from `mage assets --json`: Buzz runs `mage-agent --refresh-assets`
# when either menu opens. A pick puts `@handle` in the prompt.
foreach ($menu in @(
        @{ id = 'character'; label = 'Character'; title = 'Choose Your Character'; icon = 'user-plus' },
        @{ id = 'reference'; label = 'Reference'; title = 'Choose Your Reference'; icon = 'shapes' })) {
    $controls += [ordered]@{
        id = $menu.id; kind = 'assets'; label = $menu.label; title = $menu.title; icon = $menu.icon
        refresh = @('--refresh-assets'); options = @()
    }
}
$controls += [ordered]@{
    id = 'repo'; kind = 'repoFolder'; label = 'Save to repository'; icon = 'git-branch'; defaultFolder = 'assets/'; options = @()
}

$harnessFile = Join-Path $harnessDir 'mage.json'
$definition = [ordered]@{
    id          = 'mage'
    label       = 'Mage'
    command     = (Join-Path $InstallDir 'mage-agent.exe')
    args        = @()
    # mage-agent also rebuilds those menus here when it starts.
    env         = @{ MAGE_HARNESS_FILE = $harnessFile }
    installHint = 'Run install.ps1 from the mage-agent repository.'
    composer    = [ordered]@{
        modes    = @(
            [ordered]@{ id = 'generate'; label = 'Generate' },
            [ordered]@{ id = 'edit'; label = 'Edit'; requiresImage = $true; autoOnImage = $true })
        controls = $controls
        refresh  = @('--refresh-panel')
    }
}
$json = $definition | ConvertTo-Json -Depth 10
[System.IO.File]::WriteAllText($harnessFile, $json, (New-Object System.Text.UTF8Encoding $false))

$env:MAGE_HARNESS_FILE = $harnessFile
& (Join-Path $InstallDir 'mage-agent.exe') --refresh-panel
if ($LASTEXITCODE -ne 0) {
    Write-Warning 'Model, Aspect ratio, Resolution and Preset menus could not be read from models.ini and presets.ini.'
}
& (Join-Path $InstallDir 'mage-agent.exe') --refresh-assets
if ($LASTEXITCODE -ne 0) {
    Write-Warning 'Character and Reference menus are empty until mage can list your assets; they refresh when opened.'
}

Write-Host "Installed $InstallDir\mage-agent.exe"
Write-Host "Registered $harnessFile"
