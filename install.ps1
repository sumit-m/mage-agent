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
# Buzz's message-box panel. Values travel by control id; mage-agent maps them
# to mage flags. Presets come from the installed presets.ini.
function Read-Presets([string]$path) {
    $presets = [ordered]@{}
    $current = $null
    if (-not (Test-Path $path)) { return $presets }
    foreach ($line in Get-Content $path) {
        $line = $line.Trim()
        if ($line -match '^\[(.+)\]$') {
            $current = $Matches[1].Trim()
            $presets[$current] = @{}
        } elseif ($current -and $line -match '^([a-z_]+)\s*=\s*(.*)$') {
            $presets[$current][$Matches[1]] = $Matches[2].Trim()
        }
    }
    return $presets
}
$presetOptions = @()
$presets = Read-Presets (Join-Path $InstallDir 'presets.ini')
foreach ($name in $presets.Keys) {
    $values = $presets[$name]
    $sets = [ordered]@{}
    if ($values['model_id']) { $sets['model'] = $values['model_id'] }
    if ($values['aspect_ratio']) { $sets['aspect'] = $values['aspect_ratio'] }
    if ($values['resolution']) { $sets['res'] = $values['resolution'] }
    $presetOptions += [ordered]@{ value = $name; label = $name; sets = $sets }
}
$controls = @(
    [ordered]@{
        id = 'model'; kind = 'list'; label = 'Model'; icon = 'sparkles'; default = 'mango-v4'; sidecarKey = 'model_id'
        options = @(
            [ordered]@{ value = 'mango-v4'; label = 'Mango 4' },
            [ordered]@{ value = 'mango-v3-pro'; label = 'Mango 3' },
            [ordered]@{ value = 'mango-v2'; label = 'Mango 2' })
    },
    [ordered]@{
        id = 'aspect'; kind = 'shapes'; label = 'Aspect ratio'; default = 'portrait'; sidecarKey = 'aspect_ratio'
        defaultByMode = @{ edit = 'auto' }
        options = @(
            [ordered]@{ value = 'auto'; label = 'Auto'; modes = @('edit') },
            [ordered]@{ value = 'portrait'; label = 'Portrait'; size = @(896, 1152) },
            [ordered]@{ value = 'landscape'; label = 'Landscape'; size = @(1216, 832) },
            [ordered]@{ value = 'square'; label = 'Square'; size = @(1024, 1024) },
            [ordered]@{ value = 'cinema'; label = 'Cinema'; size = @(1344, 768) },
            [ordered]@{ value = 'computer'; label = 'Computer'; size = @(1152, 896) })
    },
    [ordered]@{
        id = 'res'; kind = 'segmented'; label = 'Resolution'; default = '1K'; sidecarKey = 'resolution'
        defaultByMode = @{ edit = 'auto' }
        options = @(
            [ordered]@{ value = 'auto'; label = 'Auto'; modes = @('edit') },
            [ordered]@{ value = '1K'; label = '1K'; scale = 1 },
            [ordered]@{ value = '2K'; label = '2K'; scale = 2 })
    }
)
if ($presetOptions.Count -gt 0) {
    $controls += [ordered]@{ id = 'preset'; kind = 'preset'; label = 'Preset'; icon = 'wand'; options = $presetOptions }
}
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
    # mage-agent rebuilds the Preset menu here from presets.ini when it starts.
    env         = @{ MAGE_HARNESS_FILE = $harnessFile }
    installHint = 'Run install.ps1 from the mage-agent repository.'
    composer    = [ordered]@{
        modes    = @(
            [ordered]@{ id = 'generate'; label = 'Generate' },
            [ordered]@{ id = 'edit'; label = 'Edit'; requiresImage = $true; autoOnImage = $true })
        controls = $controls
        # Buzz runs `mage-agent --refresh-presets` before each read of the
        # panel, which rebuilds the Preset menu from presets.ini.
        refresh  = @('--refresh-presets')
    }
}
$json = $definition | ConvertTo-Json -Depth 10
[System.IO.File]::WriteAllText($harnessFile, $json, (New-Object System.Text.UTF8Encoding $false))

$env:MAGE_HARNESS_FILE = $harnessFile
& (Join-Path $InstallDir 'mage-agent.exe') --refresh-assets
if ($LASTEXITCODE -ne 0) {
    Write-Warning 'Character and Reference menus are empty until mage can list your assets; they refresh when opened.'
}

Write-Host "Installed $InstallDir\mage-agent.exe"
Write-Host "Registered $harnessFile"
