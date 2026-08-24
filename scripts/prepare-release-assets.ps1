[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$artifactRoot = [IO.Path]::GetFullPath(
    (Join-Path $projectRoot "artifacts"))
if (-not $OutputDirectory) {
    $OutputDirectory = Join-Path $artifactRoot "release"
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
$artifactPrefix = $artifactRoot.TrimEnd(
    [IO.Path]::DirectorySeparatorChar,
    [IO.Path]::AltDirectorySeparatorChar) +
    [IO.Path]::DirectorySeparatorChar
if (-not $OutputDirectory.StartsWith(
        $artifactPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Release output must remain under $artifactRoot"
}

$installerName = "XMic_${Version}_x64-setup.exe"
$installerPath = Join-Path $projectRoot (
    "target\release\bundle\nsis\$installerName")
if (-not (Test-Path -LiteralPath $installerPath -PathType Leaf)) {
    throw "Expected NSIS installer is missing: $installerPath"
}
$installer = Get-Item -LiteralPath $installerPath
if ($installer.Length -lt 1MB) {
    throw "NSIS installer is unexpectedly small: $($installer.Length) bytes"
}

$stream = [IO.File]::OpenRead($installerPath)
try {
    if ($stream.ReadByte() -ne 0x4D -or $stream.ReadByte() -ne 0x5A) {
        throw "NSIS installer does not have a Windows PE header"
    }
} finally {
    $stream.Dispose()
}

if (Test-Path -LiteralPath $OutputDirectory) {
    Remove-Item -LiteralPath $OutputDirectory -Recurse -Force
}
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$releaseInstaller = Join-Path $OutputDirectory $installerName
Copy-Item -LiteralPath $installerPath -Destination $releaseInstaller
$hash = (Get-FileHash -LiteralPath $releaseInstaller -Algorithm SHA256).Hash.
    ToLowerInvariant()
$encoding = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText(
    (Join-Path $OutputDirectory "SHA256SUMS.txt"),
    "$hash  $installerName`n",
    $encoding)

Write-Output "Prepared release assets in $OutputDirectory"
