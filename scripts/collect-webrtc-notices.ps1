[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$SourceRoot,
    [Parameter(Mandatory)]
    [string]$OutputRoot,
    [Parameter(Mandatory)]
    [string]$Destination,
    [Parameter(Mandatory)]
    [string]$Binary,
    [string]$Target = "//xmic_webrtc_dsp:xmic_webrtc_dsp"
)

$ErrorActionPreference = "Stop"
if ($PSVersionTable.PSVersion.Major -lt 7) {
    throw "WebRTC notice generation requires PowerShell 7 or newer"
}

function Get-RelativePath {
    param(
        [Parameter(Mandatory)]
        [string]$BasePath,
        [Parameter(Mandatory)]
        [string]$Path
    )

    $baseFullPath = [IO.Path]::GetFullPath($BasePath)
    $separator = [IO.Path]::DirectorySeparatorChar
    if (-not $baseFullPath.EndsWith($separator.ToString())) {
        $baseFullPath += $separator
    }
    $pathFullPath = [IO.Path]::GetFullPath($Path)
    $baseUri = [Uri]::new($baseFullPath)
    $pathUri = [Uri]::new($pathFullPath)
    if ($baseUri.Scheme -ne $pathUri.Scheme) {
        return $pathFullPath
    }
    return [Uri]::UnescapeDataString(
        $baseUri.MakeRelativeUri($pathUri).ToString()).Replace(
            [char]'/', $separator)
}

function Resolve-PhysicalPath {
    param(
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$Description
    )

    $fullPath = [IO.Path]::GetFullPath($Path)
    $pathRoot = [IO.Path]::GetPathRoot($fullPath)
    if (-not $pathRoot) {
        throw "$Description has no filesystem root: $Path"
    }

    $current = $pathRoot
    $segments = $fullPath.Substring($pathRoot.Length) -split '[\\/]'
    foreach ($segment in $segments) {
        if (-not $segment) { continue }
        $current = Join-Path $current $segment
        if (-not (Test-Path -LiteralPath $current)) {
            throw "$Description does not exist: $current"
        }
        $item = Get-Item -LiteralPath $current -Force
        if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            $target = $item.ResolveLinkTarget($true)
            if ($null -eq $target) {
                throw "$Description has an unresolved reparse target: $current"
            }
            $current = $target.FullName
        } else {
            $current = $item.FullName
        }
    }
    return [IO.Path]::GetFullPath($current)
}

function Resolve-ContainedPath {
    param(
        [Parameter(Mandatory)]
        [string]$RootPath,
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$Description
    )

    $root = (Resolve-PhysicalPath `
            -Path $RootPath -Description "WebRTC source root").TrimEnd(
        [IO.Path]::DirectorySeparatorChar,
        [IO.Path]::AltDirectorySeparatorChar)
    $candidate = Resolve-PhysicalPath -Path $Path -Description $Description
    $prefix = $root + [IO.Path]::DirectorySeparatorChar
    if ($candidate -ne $root -and -not $candidate.StartsWith(
            $prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$Description escapes the WebRTC source root: $Path"
    }
    return $candidate
}

function Resolve-ContainedFile {
    param(
        [Parameter(Mandatory)]
        [string]$RootPath,
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$Description
    )

    $candidate = Resolve-ContainedPath `
        -RootPath $RootPath -Path $Path -Description $Description
    if (-not (Test-Path -LiteralPath $candidate -PathType Leaf)) {
        throw "$Description is not a file: $candidate"
    }
    return $candidate
}

function Resolve-ContainedDirectory {
    param(
        [Parameter(Mandatory)]
        [string]$RootPath,
        [Parameter(Mandatory)]
        [string]$Path,
        [Parameter(Mandatory)]
        [string]$Description
    )

    $candidate = Resolve-ContainedPath `
        -RootPath $RootPath -Path $Path -Description $Description
    if (-not (Test-Path -LiteralPath $candidate -PathType Container)) {
        throw "$Description is not a directory: $candidate"
    }
    return $candidate
}

function Get-LicenseFiles {
    param(
        [Parameter(Mandatory)]
        [string]$ComponentRoot
    )

    $ComponentRoot = Resolve-ContainedDirectory `
        -RootPath $SourceRoot `
        -Path $ComponentRoot `
        -Description "GN component"

    $files = [Collections.Generic.HashSet[string]]::new(
        [StringComparer]::OrdinalIgnoreCase)
    Get-ChildItem -LiteralPath $ComponentRoot -File | Where-Object {
        $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE|COPYRIGHT|PATENTS?)([-_.].*)?$' -or
        $_.Name -eq 'README.chromium'
    } | ForEach-Object {
        $resolved = Resolve-ContainedFile `
            -RootPath $SourceRoot `
            -Path $_.FullName `
            -Description "Component license file"
        [void]$files.Add($resolved)
    }

    $metadata = Join-Path $ComponentRoot "README.chromium"
    if (Test-Path -LiteralPath $metadata -PathType Leaf) {
        foreach ($line in Get-Content -LiteralPath $metadata) {
            if ($line -notmatch '^\s*License File:\s*(.+?)\s*$') { continue }
            foreach ($value in $Matches[1].Split(',')) {
                $licensePath = $value.Trim()
                if (-not $licensePath -or $licensePath -eq 'NOT_SHIPPED') { continue }
                if ($licensePath.StartsWith('//')) {
                    $resolved = Join-Path $SourceRoot $licensePath.Substring(2)
                } else {
                    $resolved = Join-Path $ComponentRoot $licensePath
                }
                $contained = Resolve-ContainedFile `
                    -RootPath $SourceRoot `
                    -Path $resolved `
                    -Description "Declared license file"
                [void]$files.Add($contained)
            }
        }
    }
    return @($files | Sort-Object)
}

$SourceRoot = Resolve-PhysicalPath `
    -Path $SourceRoot -Description "WebRTC source root"
$OutputRoot = (Resolve-Path -LiteralPath $OutputRoot).Path
$Binary = Resolve-PhysicalPath -Path $Binary -Description "WebRTC binary"
$revision = (& git -C $SourceRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw "Failed to resolve the WebRTC revision" }
$depsBlob = (& git -C $SourceRoot hash-object DEPS).Trim()
if ($LASTEXITCODE -ne 0) { throw "Failed to hash the WebRTC DEPS file" }
$binaryHash = (Get-FileHash -LiteralPath $Binary -Algorithm SHA256).Hash.
    ToLowerInvariant()

$labels = @($Target)
Push-Location $SourceRoot
try {
    $labels += & gn.bat desc $OutputRoot $Target deps --all
    if ($LASTEXITCODE -ne 0) { throw "Failed to read the WebRTC GN dependency graph" }
} finally {
    Pop-Location
}

$componentLabels = @{}
foreach ($rawLabel in $labels) {
    $label = ($rawLabel.Trim() -replace '\s+\(.*\)$', '')
    if (-not $label.StartsWith('//')) { continue }
    $targetPath = ($label.Split(':')[0]).Substring(2)
    $segments = @($targetPath.Split('/') | Where-Object { $_ })
    $thirdPartyIndex = [Array]::IndexOf($segments, 'third_party')
    if ($thirdPartyIndex -lt 0 -or $thirdPartyIndex + 1 -ge $segments.Count) {
        continue
    }
    $componentSegments = [string[]]$segments[0..($thirdPartyIndex + 1)]
    $componentRelative = [IO.Path]::Combine($componentSegments)
    $componentRoot = Join-Path $SourceRoot $componentRelative
    if (-not (Test-Path -LiteralPath $componentRoot -PathType Container)) {
        throw "GN dependency component is missing: $componentRelative"
    }
    if (-not $componentLabels.ContainsKey($componentRoot)) {
        $componentLabels[$componentRoot] = [Collections.Generic.HashSet[string]]::new()
    }
    [void]$componentLabels[$componentRoot].Add($label)
}

if ($componentLabels.Count -eq 0) {
    throw "The WebRTC GN graph did not contain any third-party components"
}

$entries = [Collections.Generic.List[object]]::new()
$rootLicenseFiles = @(
    Join-Path $SourceRoot "LICENSE"
    Join-Path $SourceRoot "PATENTS"
)
foreach ($path in $rootLicenseFiles) {
    [void](Resolve-ContainedFile `
        -RootPath $SourceRoot `
        -Path $path `
        -Description "Required WebRTC license file")
}
$entries.Add([pscustomobject]@{
        Name = "WebRTC"
        Labels = @($Target)
        Files = $rootLicenseFiles
    })

foreach ($componentRoot in @($componentLabels.Keys | Sort-Object)) {
    $files = @(Get-LicenseFiles -ComponentRoot $componentRoot)
    if ($files.Count -eq 0) {
        $relativeRoot = Get-RelativePath `
            -BasePath $SourceRoot -Path $componentRoot
        $buildtoolsPrefix = "buildtools" + [IO.Path]::DirectorySeparatorChar +
            "third_party" + [IO.Path]::DirectorySeparatorChar
        if ($relativeRoot.StartsWith($buildtoolsPrefix)) {
            $fallback = Join-Path $SourceRoot (
                "third_party" + [IO.Path]::DirectorySeparatorChar +
                $relativeRoot.Substring($buildtoolsPrefix.Length))
            if (Test-Path -LiteralPath $fallback -PathType Container) {
                $files = @(Get-LicenseFiles -ComponentRoot $fallback)
            }
        }
    }
    if ($files.Count -eq 0) {
        $relativeRoot = Get-RelativePath `
            -BasePath $SourceRoot -Path $componentRoot
        throw "No license metadata found for GN component: $relativeRoot"
    }
    $entries.Add([pscustomobject]@{
        Name = Get-RelativePath -BasePath $SourceRoot -Path $componentRoot
        Labels = @($componentLabels[$componentRoot] | Sort-Object)
        Files = $files
    })
}

$lines = [Collections.Generic.List[string]]::new()
$lines.Add("XMic WebRTC Third-Party Notices")
$lines.Add("================================")
$lines.Add("")
$lines.Add("This file is generated from the pinned WebRTC checkout and GN graph.")
$lines.Add("WebRTC revision: $revision")
$lines.Add("WebRTC DEPS blob: $depsBlob")
$lines.Add("GN target: $Target")
$lines.Add("Binary: $([IO.Path]::GetFileName($Binary))")
$lines.Add("Binary SHA-256: $binaryHash")
$lines.Add("Third-party components: $($componentLabels.Count)")
$lines.Add("")
foreach ($entry in $entries) {
    $lines.Add(('=' * 80))
    $lines.Add("Component: $($entry.Name)")
    $lines.Add("GN labels:")
    foreach ($label in $entry.Labels) { $lines.Add("  $label") }
    $lines.Add("")
    foreach ($path in $entry.Files) {
        $relativePath = Get-RelativePath -BasePath $SourceRoot -Path $path
        $lines.Add("----- $relativePath -----")
        $lines.Add([IO.File]::ReadAllText($path).TrimEnd())
        $lines.Add("")
    }
}

$destinationDirectory = Split-Path -Parent $Destination
New-Item -ItemType Directory -Force $destinationDirectory | Out-Null
$encoding = [Text.UTF8Encoding]::new($false)
[IO.File]::WriteAllText(
    $Destination,
    ($lines -join "`n").TrimEnd() + "`n",
    $encoding)
Write-Output "WebRTC notices written to $Destination"
