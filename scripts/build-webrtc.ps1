[CmdletBinding()]
param(
    [ValidateRange(1, 4)]
    [int]$Jobs = 2
)

$ErrorActionPreference = "Stop"
if ($PSVersionTable.PSVersion.Major -lt 7) {
    throw "WebRTC builds require PowerShell 7 or newer"
}
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

function Write-Utf8Json {
    param(
        [Parameter(Mandatory)]
        [object]$Value,
        [Parameter(Mandatory)]
        [string]$Path
    )

    $encoding = [Text.UTF8Encoding]::new($false)
    $json = $Value | ConvertTo-Json -Depth 10
    $json = $json.Replace("`r`n", "`n").TrimEnd() + "`n"
    [IO.File]::WriteAllText($Path, $json, $encoding)
}

function Get-LowercaseSha256 {
    param(
        [Parameter(Mandatory)]
        [string]$Path
    )

    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.
        ToLowerInvariant()
}

function Publish-StagedArtifactSet {
    param(
        [Parameter(Mandatory)]
        [string]$StagedArtifactRoot,
        [Parameter(Mandatory)]
        [string]$StagedLockPath,
        [Parameter(Mandatory)]
        [string]$ArtifactRoot,
        [Parameter(Mandatory)]
        [string]$LockPath
    )

    $transactionId = [Guid]::NewGuid().ToString("N")
    $artifactBackup = Join-Path (Split-Path -Parent $ArtifactRoot) `
        ".win-x64-backup-$transactionId"
    $lockBackup = "$LockPath.backup-$transactionId"
    $artifactBackedUp = $false
    $lockBackedUp = $false
    $artifactPublished = $false
    $lockPublished = $false
    try {
        if (Test-Path -LiteralPath $ArtifactRoot) {
            Move-Item -LiteralPath $ArtifactRoot -Destination $artifactBackup
            $artifactBackedUp = $true
        }
        if (Test-Path -LiteralPath $LockPath) {
            Move-Item -LiteralPath $LockPath -Destination $lockBackup
            $lockBackedUp = $true
        }
        Move-Item -LiteralPath $StagedArtifactRoot -Destination $ArtifactRoot
        $artifactPublished = $true
        Move-Item -LiteralPath $StagedLockPath -Destination $LockPath
        $lockPublished = $true
    } catch {
        if ($lockPublished -and (Test-Path -LiteralPath $LockPath)) {
            Remove-Item -LiteralPath $LockPath -Force
        }
        if ($lockBackedUp -and (Test-Path -LiteralPath $lockBackup)) {
            Move-Item -LiteralPath $lockBackup -Destination $LockPath
        }
        if ($artifactPublished -and (Test-Path -LiteralPath $ArtifactRoot)) {
            Remove-Item -LiteralPath $ArtifactRoot -Recurse -Force
        }
        if ($artifactBackedUp -and (Test-Path -LiteralPath $artifactBackup)) {
            Move-Item -LiteralPath $artifactBackup -Destination $ArtifactRoot
        }
        throw
    }

    if ($artifactBackedUp) {
        Remove-Item -LiteralPath $artifactBackup -Recurse -Force
    }
    if ($lockBackedUp) {
        Remove-Item -LiteralPath $lockBackup -Force
    }
}

$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$nativeRoot = Join-Path $projectRoot "native\webrtc-audio-processing"
$lockPath = Join-Path $nativeRoot "webrtc-lock.json"
$lock = Get-Content $lockPath -Raw |
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

if ($lock.windowsSdkVersion -ne "10.0.26100.0") {
    throw "build-webrtc.ps1 must be updated for Windows SDK $($lock.windowsSdkVersion)"
}
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vsPath = (& $vswhere -latest -version "[17.0,18.0)" -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -requires Microsoft.VisualStudio.Component.Windows11SDK.26100 `
    -property installationPath).Trim()
if (-not $vsPath) {
    throw "VS 2022 C++ tools with Windows SDK $($lock.windowsSdkVersion) are required"
}
$visualStudioVersion = (& $vswhere -latest -version "[17.0,18.0)" -products * `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -requires Microsoft.VisualStudio.Component.Windows11SDK.26100 `
    -property installationVersion).Trim()
if (-not $visualStudioVersion) {
    throw "Failed to resolve the Visual Studio installation version"
}
$windowsSdk = "${env:ProgramFiles(x86)}\Windows Kits\10"
$windowsSdkLib = Join-Path $windowsSdk "Lib\$($lock.windowsSdkVersion)"
if (-not (Test-Path -LiteralPath $windowsSdkLib -PathType Container)) {
    throw "Windows SDK $($lock.windowsSdkVersion) is missing: $windowsSdkLib"
}
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
$env:WindowsSDKVersion = "$($lock.windowsSdkVersion)\"

$gnArgsList = @($lock.gnArgs)
if ($gnArgsList.Count -eq 0) {
    throw "webrtc-lock.json must contain the pinned GN arguments"
}
$gnArgs = $gnArgsList -join " "
$ninjaTarget = $lock.gnTarget -replace '^//', ''

$artifactRoot = Join-Path $projectRoot "artifacts\webrtc\win-x64"
$artifactParent = Split-Path -Parent $artifactRoot
New-Item -ItemType Directory -Force $artifactParent | Out-Null
$stagingRoot = Join-Path $artifactParent (
    ".win-x64-staging-" + [Guid]::NewGuid().ToString("N"))
$stagedArtifactRoot = Join-Path $stagingRoot "win-x64"
$stagedLockPath = Join-Path $stagingRoot "webrtc-lock.json"
New-Item -ItemType Directory -Force $stagedArtifactRoot | Out-Null

try {
    $locationPushed = $false
    try {
        [IO.File]::WriteAllText($rootBuildPath, $rootBuildPatched)
        Push-Location $sourceRoot
        $locationPushed = $true
        Invoke-Checked -Command "gn.bat" -Arguments @(
            "gen", $outputRoot, "--args=$gnArgs")
        Invoke-Checked -Command "autoninja.bat" -Arguments @(
            "-C", $outputRoot, "-j", "$Jobs",
            $ninjaTarget)
        Copy-Item `
            (Join-Path $outputRoot "xmic_webrtc_dsp.dll") `
            $stagedArtifactRoot
        $noticeScript = Join-Path $PSScriptRoot "collect-webrtc-notices.ps1"
        & $noticeScript `
            -SourceRoot $sourceRoot `
            -OutputRoot $outputRoot `
            -Destination (Join-Path $stagedArtifactRoot "WEBRTC_THIRD_PARTY_NOTICES.txt") `
            -Binary (Join-Path $stagedArtifactRoot "xmic_webrtc_dsp.dll") `
            -Target $lock.gnTarget
        if (-not $?) { throw "Failed to generate WebRTC third-party notices" }

        # depot_tools locates checkout-local binaries from the current source
        # directory. Capture the exact tools used by the build before leaving it.
        $gnVersionOutput = @(& gn.bat --version)
        $gnVersion = ($gnVersionOutput -join "`n").Trim()
        if ($LASTEXITCODE -ne 0 -or -not $gnVersion) {
            throw "Failed to capture the GN version"
        }
        $ninjaVersionOutput = @(& ninja.bat --version)
        $ninjaVersion = ($ninjaVersionOutput -join "`n").Trim()
        if ($LASTEXITCODE -ne 0 -or -not $ninjaVersion) {
            throw "Failed to capture the Ninja version"
        }
    } finally {
        if ($locationPushed) {
            Pop-Location
        }
        [IO.File]::WriteAllText($rootBuildPath, $rootBuildOriginal)
    }

    $stagedDll = Join-Path $stagedArtifactRoot "xmic_webrtc_dsp.dll"
    $stagedNotices = Join-Path $stagedArtifactRoot `
        "WEBRTC_THIRD_PARTY_NOTICES.txt"
    $dllHash = Get-LowercaseSha256 $stagedDll
    $noticesHash = Get-LowercaseSha256 $stagedNotices
    $overlayInputs = @(
        "native/webrtc-audio-processing/BUILD.gn"
        "native/webrtc-audio-processing/include/xmic_webrtc_dsp.h"
        "native/webrtc-audio-processing/src/xmic_webrtc_dsp.cc"
    ) | ForEach-Object {
        [ordered]@{
            path = $_
            sha256 = Get-LowercaseSha256 (Join-Path $projectRoot $_)
        }
    }
    $provenance = [ordered]@{
        schemaVersion = 1
        subject = [ordered]@{
            name = "xmic_webrtc_dsp.dll"
            sha256 = $dllHash
        }
        notices = [ordered]@{
            name = "WEBRTC_THIRD_PARTY_NOTICES.txt"
            sha256 = $noticesHash
        }
        materials = [ordered]@{
            webrtc = [ordered]@{
                repository = $lock.sourceUrl
                revision = $sourceRevision
                depsBlob = $depsBlob
            }
            depotTools = [ordered]@{
                repository = $lock.depotToolsUrl
                revision = $depotToolsRevision
            }
            declaredOverlayInputs = $overlayInputs
        }
        build = [ordered]@{
            capture = "generated"
            platform = "windows-x64"
            target = $lock.gnTarget
            gnArgs = $gnArgsList
            windowsSdkVersion = $lock.windowsSdkVersion
            builder = [ordered]@{
                visualStudioVersion = $visualStudioVersion
                gnVersion = $gnVersion
                ninjaVersion = $ninjaVersion
            }
        }
    }
    $stagedProvenance = Join-Path $stagedArtifactRoot "PROVENANCE.json"
    Write-Utf8Json -Value $provenance -Path $stagedProvenance

    $lock.dllSha256 = $dllHash
    $lock.noticesSha256 = $noticesHash
    $lock.provenanceSha256 = Get-LowercaseSha256 $stagedProvenance
    Write-Utf8Json -Value $lock -Path $stagedLockPath

    $checker = Join-Path $PSScriptRoot "check-webrtc-artifacts.mjs"
    Invoke-Checked -Command "node" -Arguments @(
        $checker,
        "--artifact-root", $stagedArtifactRoot,
        "--lock", $stagedLockPath)

    Publish-StagedArtifactSet `
        -StagedArtifactRoot $stagedArtifactRoot `
        -StagedLockPath $stagedLockPath `
        -ArtifactRoot $artifactRoot `
        -LockPath $lockPath
} finally {
    if (Test-Path -LiteralPath $stagingRoot) {
        Remove-Item -LiteralPath $stagingRoot -Recurse -Force
    }
}

Write-Output "WebRTC DSP built at $artifactRoot"
