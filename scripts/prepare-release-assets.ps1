[CmdletBinding()]
param(
    [Parameter()]
    [ValidatePattern('^\d+\.\d+\.\d+$')]
    [string]$Version,
    [string]$OutputDirectory
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.IO.Compression

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

function Get-RepositoryVersion {
    $packageManifestPath = Join-Path $projectRoot "package.json"
    $packageManifest = Get-Content -LiteralPath $packageManifestPath -Raw |
        ConvertFrom-Json
    $repositoryVersion = [string]$packageManifest.version
    if ($repositoryVersion -notmatch '^\d+\.\d+\.\d+$') {
        throw "Repository version is not MAJOR.MINOR.PATCH: $repositoryVersion"
    }
    return $repositoryVersion
}

function Assert-PeFile([string]$Path, [string]$Description) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Description is missing: $Path"
    }
    $file = Get-Item -LiteralPath $Path
    if ($file.Length -lt 1MB) {
        throw "$Description is unexpectedly small: $($file.Length) bytes"
    }

    $stream = [IO.File]::OpenRead($Path)
    try {
        if ($stream.ReadByte() -ne 0x4D -or $stream.ReadByte() -ne 0x5A) {
            throw "$Description does not have a Windows PE header"
        }
    } finally {
        $stream.Dispose()
    }
}

function Open-ZipForCreate([string]$Path) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew)
    try {
        return [IO.Compression.ZipArchive]::new(
            $stream,
            [IO.Compression.ZipArchiveMode]::Create,
            $false)
    } catch {
        $stream.Dispose()
        throw
    }
}

function Add-ZipFile(
    [IO.Compression.ZipArchive]$Archive,
    [string]$SourcePath,
    [string]$EntryName
) {
    $entry = $Archive.CreateEntry(
        $EntryName,
        [IO.Compression.CompressionLevel]::Optimal)
    $entryStream = $entry.Open()
    $sourceStream = [IO.File]::OpenRead($SourcePath)
    try {
        $sourceStream.CopyTo($entryStream)
    } finally {
        $sourceStream.Dispose()
        $entryStream.Dispose()
    }
}

function Assert-ZipEntries([string]$Path, [string[]]$ExpectedEntries) {
    $stream = [IO.File]::OpenRead($Path)
    $archive = [IO.Compression.ZipArchive]::new(
        $stream,
        [IO.Compression.ZipArchiveMode]::Read,
        $false)
    try {
        $actualEntries = @($archive.Entries | ForEach-Object { $_.FullName })
        $duplicateEntries = @($actualEntries | Group-Object | Where-Object Count -gt 1)
        if ($duplicateEntries.Count -ne 0) {
            throw "ZIP contains duplicate entries: $($duplicateEntries.Name -join ', ')"
        }
        $unexpected = @($actualEntries | Where-Object { $_ -notin $ExpectedEntries })
        $missing = @($ExpectedEntries | Where-Object { $_ -notin $actualEntries })
        if ($unexpected.Count -ne 0 -or $missing.Count -ne 0 -or
            $actualEntries.Count -ne $ExpectedEntries.Count) {
            throw "ZIP entry allowlist mismatch for $Path; " +
                "unexpected=[$($unexpected -join ', ')], missing=[$($missing -join ', ')]"
        }
    } finally {
        $archive.Dispose()
        $stream.Dispose()
    }
}

function Get-ArchiveRelativePath([string]$Root, [string]$Path) {
    # Windows PowerShell 5.1, which runs npm scripts, has no [IO.Path]::GetRelativePath.
    $rootFull = [IO.Path]::GetFullPath($Root).TrimEnd(
        [IO.Path]::DirectorySeparatorChar,
        [IO.Path]::AltDirectorySeparatorChar)
    $pathFull = [IO.Path]::GetFullPath($Path)
    if ($pathFull.Length -le $rootFull.Length -or -not $pathFull.StartsWith(
            $rootFull, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Path $pathFull is not inside $rootFull"
    }
    return $pathFull.Substring($rootFull.Length + 1)
}

function Get-SourceArchiveEntries([string]$SourceRoot) {
    $entries = [Collections.Generic.List[string]]::new()
    $entries.Add("MPL_SOURCE_CODE.txt")
    $entries.Add("LICENSE-MPL-2.0.txt")
    $entries.Add("sources/")
    $directories = @(Get-ChildItem -LiteralPath $SourceRoot -Directory -Recurse |
        Sort-Object FullName)
    foreach ($directory in $directories) {
        $relativePath = Get-ArchiveRelativePath $SourceRoot $directory.FullName
        $entries.Add("sources/$($relativePath.Replace('\', '/'))/")
    }
    $files = @(Get-ChildItem -LiteralPath $SourceRoot -File -Recurse |
        Sort-Object FullName)
    foreach ($file in $files) {
        $relativePath = Get-ArchiveRelativePath $SourceRoot $file.FullName
        $entries.Add("sources/$($relativePath.Replace('\', '/'))")
    }
    return $entries.ToArray()
}

$repositoryVersion = Get-RepositoryVersion
if (-not $Version) {
    $Version = $repositoryVersion
}
if ($Version -ne $repositoryVersion) {
    throw "Version argument $Version does not match repository version $repositoryVersion"
}

$installerName = "XMic_${Version}_x64-setup.exe"
$installerPath = Join-Path $projectRoot (
    "target\release\bundle\nsis\$installerName")
$executableName = "XMic.exe"
$executablePath = Join-Path $projectRoot "target\release\$executableName"
Assert-PeFile $installerPath "NSIS installer"
Assert-PeFile $executablePath "standalone executable"

$licenseArtifactRoot = Join-Path $projectRoot "artifacts\licenses"
$sourceManifestPath = Join-Path $licenseArtifactRoot "MPL_SOURCE_CODE.txt"
$sourceRoot = Join-Path $licenseArtifactRoot "sources"
$mplLicensePath = Join-Path $projectRoot "third-party\licenses\MPL-2.0.txt"
if (-not (Test-Path -LiteralPath $sourceManifestPath -PathType Leaf)) {
    throw "MPL source manifest is missing: $sourceManifestPath"
}
if (-not (Test-Path -LiteralPath $sourceRoot -PathType Container)) {
    throw "MPL source tree is missing: $sourceRoot"
}
if (-not (Test-Path -LiteralPath $mplLicensePath -PathType Leaf)) {
    throw "MPL-2.0 license text is missing: $mplLicensePath"
}

if (Test-Path -LiteralPath $OutputDirectory) {
    Remove-Item -LiteralPath $OutputDirectory -Recurse -Force
}
New-Item -ItemType Directory -Force $OutputDirectory | Out-Null

$payloadNames = @(
    $installerName,
    $executableName,
    "XMic_x64-portable.zip",
    "XMic_${Version}_MPL-2.0-source.zip"
)
$releaseInstaller = Join-Path $OutputDirectory $installerName
$releaseExecutable = Join-Path $OutputDirectory $executableName
Copy-Item -LiteralPath $installerPath -Destination $releaseInstaller
Copy-Item -LiteralPath $executablePath -Destination $releaseExecutable
Assert-PeFile $releaseInstaller "copied NSIS installer"
Assert-PeFile $releaseExecutable "copied standalone executable"

$portableZipPath = Join-Path $OutputDirectory $payloadNames[2]
$portableEntries = @(
    "XMic_x64-portable/",
    "XMic_x64-portable/XMic.exe",
    "XMic_x64-portable/portable.flag",
    "XMic_x64-portable/data/"
)
$portableArchive = Open-ZipForCreate $portableZipPath
try {
    $null = $portableArchive.CreateEntry($portableEntries[0])
    Add-ZipFile $portableArchive $executablePath $portableEntries[1]
    $null = $portableArchive.CreateEntry($portableEntries[2])
    $null = $portableArchive.CreateEntry($portableEntries[3])
} finally {
    $portableArchive.Dispose()
}
Assert-ZipEntries $portableZipPath $portableEntries

$sourceZipPath = Join-Path $OutputDirectory $payloadNames[3]
$sourceEntries = Get-SourceArchiveEntries $sourceRoot
$sourceArchive = Open-ZipForCreate $sourceZipPath
try {
    Add-ZipFile $sourceArchive $sourceManifestPath "MPL_SOURCE_CODE.txt"
    Add-ZipFile $sourceArchive $mplLicensePath "LICENSE-MPL-2.0.txt"
    $null = $sourceArchive.CreateEntry("sources/")
    foreach ($entryName in $sourceEntries | Select-Object -Skip 3) {
        if ($entryName.EndsWith('/')) {
            $null = $sourceArchive.CreateEntry($entryName)
            continue
        }
        $relativePath = $entryName.Substring("sources/".Length).Replace(
            '/', [IO.Path]::DirectorySeparatorChar)
        Add-ZipFile $sourceArchive (Join-Path $sourceRoot $relativePath) $entryName
    }
} finally {
    $sourceArchive.Dispose()
}
Assert-ZipEntries $sourceZipPath $sourceEntries

foreach ($payloadName in $payloadNames) {
    $payloadPath = Join-Path $OutputDirectory $payloadName
    if (-not (Test-Path -LiteralPath $payloadPath -PathType Leaf)) {
        throw "Release payload is missing: $payloadPath"
    }
}
$checksumLines = foreach ($payloadName in $payloadNames) {
    $payloadPath = Join-Path $OutputDirectory $payloadName
    $hash = (Get-FileHash -LiteralPath $payloadPath -Algorithm SHA256).Hash.
        ToLowerInvariant()
    "$hash  $payloadName"
}
$encoding = [Text.UTF8Encoding]::new($false)
$checksumPath = Join-Path $OutputDirectory "SHA256SUMS.txt"
[IO.File]::WriteAllText(
    $checksumPath,
    "$($checksumLines -join "`n")`n",
    $encoding)

$checksumNames = @(
    Get-Content -LiteralPath $checksumPath |
        ForEach-Object {
            if ($_ -notmatch '^([a-f0-9]{64})  (.+)$') {
                throw "Invalid SHA256SUMS line: $_"
            }
            $Matches[2]
        }
)
$unexpectedChecksums = @($checksumNames | Where-Object { $_ -notin $payloadNames })
$missingChecksums = @($payloadNames | Where-Object { $_ -notin $checksumNames })
if ($unexpectedChecksums.Count -ne 0 -or $missingChecksums.Count -ne 0 -or
    $checksumNames.Count -ne $payloadNames.Count -or
    @($checksumNames | Select-Object -Unique).Count -ne $payloadNames.Count) {
    throw "Checksum entry set does not exactly match the release payload set"
}

Write-Output "Prepared release assets in $OutputDirectory"
