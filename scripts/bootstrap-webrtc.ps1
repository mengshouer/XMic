[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
(Get-Process -Id $PID).PriorityClass = "BelowNormal"

$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$nativeRoot = Join-Path $projectRoot "native\webrtc-audio-processing"
$lock = Get-Content (Join-Path $nativeRoot "webrtc-lock.json") -Raw |
    ConvertFrom-Json
$webrtcRoot = Join-Path $projectRoot ".webrtc"
$depotTools = Join-Path $webrtcRoot "depot_tools"
$checkout = Join-Path $webrtcRoot "checkout"

$env:GIT_CONFIG_COUNT = "3"
$env:GIT_CONFIG_KEY_0 = "http.version"
$env:GIT_CONFIG_VALUE_0 = "HTTP/1.1"
$env:GIT_CONFIG_KEY_1 = "pack.threads"
$env:GIT_CONFIG_VALUE_1 = "2"
$env:GIT_CONFIG_KEY_2 = "checkout.workers"
$env:GIT_CONFIG_VALUE_2 = "2"

if (-not (Test-Path (Join-Path $depotTools ".git"))) {
    New-Item -ItemType Directory -Force $webrtcRoot | Out-Null
    & git clone $lock.depotToolsUrl $depotTools
    if ($LASTEXITCODE -ne 0) { throw "Failed to clone depot_tools" }
}
& git -C $depotTools checkout --detach $lock.depotToolsRevision
if ($LASTEXITCODE -ne 0) { throw "Failed to pin depot_tools" }

$env:PATH = "$depotTools;$env:PATH"
$env:DEPOT_TOOLS_UPDATE = "0"
$env:DEPOT_TOOLS_WIN_TOOLCHAIN = "0"
$env:DEPOT_TOOLS_METRICS = "0"
$env:GCLIENT_JOBS = "1"

New-Item -ItemType Directory -Force $checkout | Out-Null
if (-not (Test-Path (Join-Path $checkout ".gclient"))) {
    Push-Location $checkout
    try {
        & gclient.bat config $lock.sourceUrl
        if ($LASTEXITCODE -ne 0) { throw "Failed to configure gclient" }
    } finally {
        Pop-Location
    }
}

Push-Location $checkout
try {
    & gclient.bat sync --revision "src@$($lock.sourceRevision)" --nohooks `
        --no-history --force -j1
    if ($LASTEXITCODE -ne 0) { throw "Failed to sync WebRTC" }
    & gclient.bat runhooks -j1
    if ($LASTEXITCODE -ne 0) { throw "Failed to run WebRTC hooks" }
} finally {
    Pop-Location
}

Write-Output "WebRTC checkout ready at $checkout"
