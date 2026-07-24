[CmdletBinding()]
param(
    [ValidateRange(1, 4)]
    [int]$Jobs = 2
)

$ErrorActionPreference = "Stop"
(Get-Process -Id $PID).PriorityClass = "BelowNormal"

function Invoke-Checked {
    param(
        [Parameter(Mandatory)]
        [string]$Command,
        [Parameter(ValueFromRemainingArguments)]
        [string[]]$Arguments
    )

    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE"
    }
}

$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$nativeRoot = Join-Path $projectRoot "native\webrtc-audio-processing"
$lock = Get-Content (Join-Path $nativeRoot "webrtc-lock.json") -Raw |
    ConvertFrom-Json
$webrtcRoot = Join-Path $projectRoot ".webrtc"
$depotTools = Join-Path $webrtcRoot "depot_tools"
$sourceRoot = Join-Path $webrtcRoot "checkout\src"
$outputRoot = Join-Path $sourceRoot "out\xmic-release"

if (-not (Test-Path (Join-Path $sourceRoot ".git"))) {
    throw "WebRTC checkout is missing; run scripts/bootstrap-webrtc.ps1 first"
}

$sourceRevision = (& git -C $sourceRoot rev-parse HEAD).Trim()
$depsBlob = (& git -C $sourceRoot hash-object DEPS).Trim()
$depotToolsRevision = (& git -C $depotTools rev-parse HEAD).Trim()
if ($sourceRevision -ne $lock.sourceRevision) {
    throw "WebRTC revision mismatch: $sourceRevision"
}
if ($depsBlob -ne $lock.depsBlob) {
    throw "WebRTC DEPS blob mismatch: $depsBlob"
}
if ($depotToolsRevision -ne $lock.depotToolsRevision) {
    throw "depot_tools revision mismatch: $depotToolsRevision"
}

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vsPath = (& $vswhere -latest -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -requires Microsoft.VisualStudio.Component.Windows11SDK.26100 `
    -property installationPath).Trim()
if (-not $vsPath) {
    throw "VS 2022 C++ tools with Windows SDK 10.0.26100.0 are required"
}
$windowsSdk = "${env:ProgramFiles(x86)}\Windows Kits\10"
$debuggerDll = Join-Path $windowsSdk "Debuggers\x64\dbghelp.dll"
if (-not (Test-Path $debuggerDll)) {
    throw "Debugging Tools for Windows are required: $debuggerDll"
}

$overlayRoot = [IO.Path]::GetFullPath((Join-Path $sourceRoot "xmic_webrtc_dsp"))
$sourcePrefix = [IO.Path]::GetFullPath($sourceRoot).TrimEnd('\') + '\'
if (-not $overlayRoot.StartsWith(
        $sourcePrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Invalid WebRTC overlay path: $overlayRoot"
}
if (Test-Path $overlayRoot) {
    Remove-Item -LiteralPath $overlayRoot -Recurse -Force
}
New-Item -ItemType Directory -Force $overlayRoot | Out-Null
Copy-Item (Join-Path $nativeRoot "BUILD.gn") $overlayRoot
Copy-Item (Join-Path $nativeRoot "include") $overlayRoot -Recurse
Copy-Item (Join-Path $nativeRoot "src") $overlayRoot -Recurse

$rootBuildPath = Join-Path $sourceRoot "BUILD.gn"
$rootBuildStatus = (& git -C $sourceRoot status --porcelain -- BUILD.gn) -join ""
if ($rootBuildStatus) {
    throw "WebRTC root BUILD.gn has local changes; refusing to overwrite it"
}
$rootBuildOriginal = [IO.File]::ReadAllText($rootBuildPath)
$rootDependency = 'deps = [ ":webrtc" ]'
$rootDependencyWithOverlay =
    'deps = [ ":webrtc", "//xmic_webrtc_dsp" ]'
if (-not $rootBuildOriginal.Contains($rootDependency)) {
    throw "WebRTC root default dependency declaration changed"
}
$rootBuildPatched =
    $rootBuildOriginal.Replace($rootDependency, $rootDependencyWithOverlay)

$env:PATH = "$depotTools;$env:PATH"
$env:DEPOT_TOOLS_UPDATE = "0"
$env:DEPOT_TOOLS_WIN_TOOLCHAIN = "0"
$env:DEPOT_TOOLS_METRICS = "0"
$env:GYP_MSVS_VERSION = "2022"
$env:GYP_MSVS_OVERRIDE_PATH = $vsPath
$env:vs2022_install = $vsPath
$env:WINDOWSSDKDIR = $windowsSdk

$gnArgs = @(
    "is_debug=false"
    "is_component_build=false"
    "rtc_include_tests=false"
    "rtc_build_examples=false"
    "rtc_build_tools=false"
    "rtc_enable_protobuf=false"
    "use_remoteexec=false"
    "use_siso=false"
    "symbol_level=0"
) -join " "

$locationPushed = $false
try {
    [IO.File]::WriteAllText($rootBuildPath, $rootBuildPatched)
    Push-Location $sourceRoot
    $locationPushed = $true
    Invoke-Checked -Command "gn.bat" -Arguments @(
        "gen", $outputRoot, "--args=$gnArgs")
    Invoke-Checked -Command "autoninja.bat" -Arguments @(
        "-C", $outputRoot, "-j", "$Jobs",
        "xmic_webrtc_dsp:xmic_webrtc_dsp")
} finally {
    if ($locationPushed) {
        Pop-Location
    }
    [IO.File]::WriteAllText($rootBuildPath, $rootBuildOriginal)
}

$artifactRoot = Join-Path $projectRoot "artifacts\webrtc\win-x64"
New-Item -ItemType Directory -Force $artifactRoot | Out-Null
Copy-Item (Join-Path $outputRoot "xmic_webrtc_dsp.dll") $artifactRoot -Force
Copy-Item (Join-Path $outputRoot "xmic_webrtc_dsp.dll.lib") $artifactRoot -Force

Write-Output "WebRTC DSP built at $artifactRoot"
