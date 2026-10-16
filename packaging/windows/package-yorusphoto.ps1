<#
.SYNOPSIS
  Assemble an unzipped Windows portable yorusphoto build without replacing existing output.
.EXAMPLE
  powershell -File packaging/windows/package-yorusphoto.ps1 -Name yorusphoto-portable
.EXAMPLE
  powershell -File packaging/windows/package-yorusphoto.ps1 -Name yorusphoto-portable-v2 -RuntimeDlls C:\toolchain\bin\libunwind.dll
.EXAMPLE
  powershell -File packaging/windows/package-yorusphoto.ps1 -Name yorusphoto-portable -ArchiveOnly
#>
param(
    [string] $Name = 'yorusphoto-portable',
    [string] $BuildDirectory,
    [string] $CodexVendorDirectory,
    [string] $CodexDistributionManifest,
    [string] $CodexLicensePath,
    [string] $CodexNoticePath,
    [string] $EguiFontsDirectory,
    [string[]] $RuntimeDlls = @(),
    [switch] $ArchiveOnly
)

$ErrorActionPreference = 'Stop'
$repositoryPath = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
function Get-CodexDistributionFiles([string] $Root, [string] $ManifestPath) {
    $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
    if ($ManifestPath) {
        $manifest = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
        if (-not $manifest.files -or $manifest.files.Count -gt 10000) { throw 'Invalid Codex distribution manifest.' }
        $runtimeMetadata = Get-Content -LiteralPath (Join-Path $rootPath 'codex-package.json') -Raw | ConvertFrom-Json
        if ($manifest.version -ne $runtimeMetadata.version -or $manifest.target -ne $runtimeMetadata.target) { throw 'Codex runtime and verified manifest versions differ.' }
        $rows = @($manifest.files)
    } else {
        # Legacy packages contained native binaries only. New resource-bearing distributions
        # require the SHA-512-verified fetch manifest rather than guessing public extensions.
        $rows = @()
        foreach ($directoryName in @('bin', 'codex-path', 'codex-resources')) {
            foreach ($file in Get-ChildItem -LiteralPath (Join-Path $rootPath $directoryName) -File -Recurse) {
                if ($file.Extension -notin @('.exe', '.dll')) { throw 'Codex has non-binary resources. Provide its verified -CodexDistributionManifest.' }
                $rows += [pscustomobject]@{ path = $file.FullName.Substring($rootPath.Length + 1).Replace('\', '/'); sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant(); bytes = $file.Length }
            }
        }
        $metadata = Join-Path $rootPath 'codex-package.json'
        if (Test-Path -LiteralPath $metadata -PathType Leaf) { $rows += [pscustomobject]@{ path = 'codex-package.json'; sha256 = (Get-FileHash -LiteralPath $metadata -Algorithm SHA256).Hash.ToLowerInvariant(); bytes = (Get-Item -LiteralPath $metadata).Length } }
    }
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($row in $rows) {
        $relative = [string] $row.path
        $parts = $relative.Split('/')
        if ($relative -notmatch '^(?:(?:bin|codex-path|codex-resources)/[^\\:]+|codex-package\.json)$' -or
            @($parts | Where-Object { $_ -in @('', '.', '..', 'data', 'yorusphotoData', 'auth.json', 'config.toml', 'control.token', 'sessions', 'logs', 'mcp-config.json', 'codex-chat.json', 'codex-chat.previous.json') }).Count -ne 0 -or
            -not $seen.Add($relative) -or [string] $row.sha256 -notmatch '^[0-9a-fA-F]{64}$') { throw 'Unsafe or duplicate Codex distribution manifest path.' }
        $file = [IO.Path]::GetFullPath((Join-Path $rootPath $relative))
        if (-not $file.StartsWith($rootPath + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase) -or
            -not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Codex distribution file is missing or outside its root: $relative" }
        $item = Get-Item -LiteralPath $file
        # Cloud placeholders may also carry ReparsePoint. Reject actual links/junctions,
        # while allowing ordinary OneDrive files to hydrate and pass their content hash.
        $actualLink = $item.LinkType -in @('SymbolicLink', 'Junction') -or ($item.LinkType -ne 'HardLink' -and @($item.Target | Where-Object { $_ }).Count -ne 0)
        if ($actualLink -or $item.Length -ne [long] $row.bytes -or
            (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -ine [string] $row.sha256) { throw "Codex distribution integrity mismatch: $relative" }
        [pscustomobject]@{ Source = $file; RelativePath = $relative; bytes = $item.Length; sha256 = ([string] $row.sha256).ToLowerInvariant() }
    }
}
$distributionLicenseNames = @(
    'yorusphoto-brand.txt', 'openai-marks.txt', 'yorusai-direct.txt', 'artcraft-brand.txt', 'photocraft-app-icon.txt',
    'lucide-icons.txt', 'noun-magnetic-lasso.txt', 'symmetry-icons.txt', 'scowl-dictionary.txt',
    'OFL-Inter.txt', 'OFL-JetBrainsMono.txt',
    'egui-Hack-Regular.txt', 'egui-NotoEmoji-OFL.txt', 'egui-Ubuntu-UFL.txt', 'egui-emoji-icon-font-MIT.txt'
)
if ($Name -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]*$' -or $Name -in @('.', '..')) {
    throw 'Name must be a single directory name using letters, digits, dots, underscores or hyphens.'
}
if (-not $BuildDirectory) { $BuildDirectory = Join-Path $repositoryPath 'target\agent-toolchain\release' }
if (-not $CodexVendorDirectory) {
    $verifiedCache = Join-Path $repositoryPath '.toolchain\codex-latest'
    if (Test-Path -LiteralPath $verifiedCache -PathType Container) {
        $latest = Get-ChildItem -LiteralPath $verifiedCache -Directory | Where-Object { $_.Name -match '^\d+\.\d+\.\d+$' -and (Test-Path -LiteralPath (Join-Path $_.FullName 'distribution-manifest.json') -PathType Leaf) } | Sort-Object { [version] $_.Name } -Descending | Select-Object -First 1
        if ($latest) {
            $CodexVendorDirectory = Join-Path $latest.FullName 'native\package\vendor\x86_64-pc-windows-msvc'
            $CodexDistributionManifest = Join-Path $latest.FullName 'distribution-manifest.json'
            if (-not $CodexLicensePath) { $CodexLicensePath = Join-Path $latest.FullName 'notices\LICENSE' }
            if (-not $CodexNoticePath) { $CodexNoticePath = Join-Path $latest.FullName 'notices\NOTICE' }
        }
    }
}
if (-not $CodexVendorDirectory) {
    $CodexVendorDirectory = Join-Path $env:APPDATA 'npm\node_modules\@openai\codex\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc'
}
if (-not $CodexDistributionManifest) {
    $candidate = [IO.Path]::GetFullPath((Join-Path $CodexVendorDirectory '..\..\..\..\distribution-manifest.json'))
    if (Test-Path -LiteralPath $candidate -PathType Leaf) {
        $CodexDistributionManifest = $candidate
        if (-not $CodexLicensePath) { $CodexLicensePath = Join-Path (Split-Path -Parent $candidate) 'notices\LICENSE' }
        if (-not $CodexNoticePath) { $CodexNoticePath = Join-Path (Split-Path -Parent $candidate) 'notices\NOTICE' }
    }
}
if (-not $CodexLicensePath) { $CodexLicensePath = Join-Path $repositoryPath '..\codex-auth-research\LICENSE' }
if (-not $CodexNoticePath) { $CodexNoticePath = Join-Path $repositoryPath '..\codex-auth-research\NOTICE' }
$distPath = Join-Path $repositoryPath 'dist'
$outputPath = [IO.Path]::GetFullPath((Join-Path $distPath $Name))
if ($ArchiveOnly) {
    if (-not (Test-Path -LiteralPath $outputPath -PathType Container)) { throw "Portable folder is missing: $outputPath" }
    $archivePath = Join-Path $distPath ($Name + '.zip')
    if (Test-Path -LiteralPath $archivePath) { throw "Archive already exists: $archivePath. Existing archives are never replaced." }
    $publicFiles = @('yorusphoto.exe', 'yorusphoto-cli.exe', 'Start-yorusphoto.ps1', 'Start-yorusphoto.cmd', 'portable.txt', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'NOTICE', 'ATTRIBUTION.md')
    if (Test-Path -LiteralPath (Join-Path $outputPath 'yorusphoto-performance.md') -PathType Leaf) { $publicFiles += 'yorusphoto-performance.md' }
    $entries = @()
    foreach ($fileName in $publicFiles) {
        $sourcePath = Join-Path $outputPath $fileName
        if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) { throw "Required archive input is missing: $sourcePath" }
        $entries += @{ Source = $sourcePath; Name = $fileName }
    }
    $entries += @(Get-ChildItem -LiteralPath $outputPath -Filter '*.dll' -File | ForEach-Object { @{ Source = $_.FullName; Name = $_.Name } })
    foreach ($fileName in $distributionLicenseNames) {
        $sourcePath = Join-Path $outputPath ('licenses\' + $fileName)
        if (-not (Test-Path -LiteralPath $sourcePath -PathType Leaf)) { throw "Required distribution license is missing: $sourcePath" }
        $entries += @{ Source = $sourcePath; Name = 'licenses/' + $fileName }
    }
    foreach ($fileName in @('LICENSE', 'NOTICE', 'distribution-manifest.json')) {
        $sourcePath = Join-Path $outputPath ('codex\' + $fileName)
        if (Test-Path -LiteralPath $sourcePath -PathType Leaf) { $entries += @{ Source = $sourcePath; Name = 'codex/' + $fileName } }
    }
    $archiveRuntimeRoot = Join-Path $outputPath 'codex'
    $archiveRuntimeManifest = Join-Path $archiveRuntimeRoot 'distribution-manifest.json'
    if (-not (Test-Path -LiteralPath $archiveRuntimeManifest -PathType Leaf)) { $archiveRuntimeManifest = $null }
    foreach ($runtimeFile in @(Get-CodexDistributionFiles $archiveRuntimeRoot $archiveRuntimeManifest)) {
        $entries += @{ Source = $runtimeFile.Source; Name = 'codex/' + $runtimeFile.RelativePath }
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::Open($archivePath, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($entry in $entries) {
            [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $entry.Source, $entry.Name, [IO.Compression.CompressionLevel]::Optimal) | Out-Null
        }
        # User data and generated MCP paths are initialized locally after extraction.
        $archive.CreateEntry('workspace/') | Out-Null
    } finally {
        $archive.Dispose()
    }
    Write-Output "Portable ZIP created without user settings, credentials, tokens, logs or generated workspaces: $archivePath"
    return
}
if (Test-Path -LiteralPath $outputPath) {
    throw "Output already exists: $outputPath. Choose a fresh -Name; existing packages and their data are never removed."
}

$editorPath = Join-Path $BuildDirectory 'photocraft.exe'
$cliPath = Join-Path $BuildDirectory 'photocraft-cli.exe'
$codexPath = Join-Path $CodexVendorDirectory 'bin\codex.exe'
foreach ($required in @($editorPath, $cliPath, $codexPath, $CodexLicensePath, $CodexNoticePath)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) { throw "Required package input is missing: $required" }
}
foreach ($runtimeDll in $RuntimeDlls) {
    if ([IO.Path]::GetExtension($runtimeDll) -ine '.dll' -or -not (Test-Path -LiteralPath $runtimeDll -PathType Leaf)) {
        throw "RuntimeDlls must contain existing DLL files: $runtimeDll"
    }
}
foreach ($directoryName in @('bin', 'codex-path', 'codex-resources')) {
    $sourcePath = Join-Path $CodexVendorDirectory $directoryName
    if (-not (Test-Path -LiteralPath $sourcePath -PathType Container)) { throw "Codex runtime directory is missing: $sourcePath" }
}
$codexDistributionFiles = @(Get-CodexDistributionFiles $CodexVendorDirectory $CodexDistributionManifest)
if (-not $EguiFontsDirectory) {
    $lockText = Get-Content -LiteralPath (Join-Path $repositoryPath 'Cargo.lock') -Raw
    $fontVersionMatch = [regex]::Match($lockText, '(?m)^name = "epaint_default_fonts"\r?\nversion = "([^"]+)"')
    if (-not $fontVersionMatch.Success) { throw 'Cannot find epaint_default_fonts version in Cargo.lock.' }
    $fontPackageName = 'epaint_default_fonts-' + $fontVersionMatch.Groups[1].Value
    $cargoRegistryPath = Join-Path $env:USERPROFILE '.cargo\registry\src'
    foreach ($registrySource in Get-ChildItem -LiteralPath $cargoRegistryPath -Directory -ErrorAction SilentlyContinue) {
        $candidate = Join-Path $registrySource.FullName ($fontPackageName + '\fonts')
        if (Test-Path -LiteralPath $candidate -PathType Container) { $EguiFontsDirectory = $candidate; break }
    }
    if (-not $EguiFontsDirectory) { throw 'Cannot locate bundled egui font licenses; provide -EguiFontsDirectory from epaint_default_fonts.' }
}
$assetLicenses = @(
    @{ Source = (Join-Path $repositoryPath 'assets\yorusphoto\LICENSE-brand.txt'); Name = 'yorusphoto-brand.txt' },
    @{ Source = (Join-Path $repositoryPath 'assets\yorusphoto\NOTICE-OpenAI.txt'); Name = 'openai-marks.txt' },
    @{ Source = (Join-Path $repositoryPath 'assets\yorusphoto\NOTICE-YorusAi-Direct.txt'); Name = 'yorusai-direct.txt' },
    @{ Source = (Join-Path $repositoryPath 'docs\brand\LICENSE-brand.txt'); Name = 'artcraft-brand.txt' },
    @{ Source = (Join-Path $repositoryPath 'assets\app-icon\LICENSE.txt'); Name = 'photocraft-app-icon.txt' },
    @{ Source = (Join-Path $repositoryPath 'assets\icons\LICENSE-lucide.txt'); Name = 'lucide-icons.txt' },
    @{ Source = (Join-Path $repositoryPath 'assets\icons\LICENSE-noun-magnetic-lasso.txt'); Name = 'noun-magnetic-lasso.txt' },
    @{ Source = (Join-Path $repositoryPath 'assets\icons\LICENSE-symmetry-axes.txt'); Name = 'symmetry-icons.txt' },
    @{ Source = (Join-Path $repositoryPath 'assets\dict\LICENSE-SCOWL.txt'); Name = 'scowl-dictionary.txt' },
    @{ Source = (Join-Path $repositoryPath 'assets\fonts\OFL-Inter.txt'); Name = 'OFL-Inter.txt' },
    @{ Source = (Join-Path $repositoryPath 'assets\fonts\OFL-JetBrainsMono.txt'); Name = 'OFL-JetBrainsMono.txt' },
    @{ Source = (Join-Path $EguiFontsDirectory 'Hack-Regular.txt'); Name = 'egui-Hack-Regular.txt' },
    @{ Source = (Join-Path $EguiFontsDirectory 'OFL.txt'); Name = 'egui-NotoEmoji-OFL.txt' },
    @{ Source = (Join-Path $EguiFontsDirectory 'UFL.txt'); Name = 'egui-Ubuntu-UFL.txt' },
    @{ Source = (Join-Path $EguiFontsDirectory 'emoji-icon-font-mit-license.txt'); Name = 'egui-emoji-icon-font-MIT.txt' }
)
foreach ($license in $assetLicenses) {
    if (-not (Test-Path -LiteralPath $license.Source -PathType Leaf)) { throw "Required asset license is missing: $($license.Source)" }
}

New-Item -ItemType Directory -Path $outputPath | Out-Null
Copy-Item -LiteralPath $editorPath -Destination (Join-Path $outputPath 'yorusphoto.exe')
Copy-Item -LiteralPath $cliPath -Destination (Join-Path $outputPath 'yorusphoto-cli.exe')
foreach ($runtimeDll in $RuntimeDlls) { Copy-Item -LiteralPath $runtimeDll -Destination $outputPath }
foreach ($launcher in @('Start-yorusphoto.ps1', 'Start-yorusphoto.cmd')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot $launcher) -Destination $outputPath
}
Copy-Item -LiteralPath (Join-Path $repositoryPath 'docs\yorusphoto.md') -Destination (Join-Path $outputPath 'README.md')
Copy-Item -LiteralPath (Join-Path $repositoryPath 'docs\yorusphoto-performance.md') -Destination (Join-Path $outputPath 'yorusphoto-performance.md')
@'
yorusphoto portable mode

Opening yorusphoto.exe directly keeps editor settings in yorusphotoData beside this file.
Start-yorusphoto.cmd instead uses the dedicated data folder and enables authenticated local MCP.
Codex manages its ChatGPT sign-in separately in the current user's Codex directory.
'@ | Set-Content -LiteralPath (Join-Path $outputPath 'portable.txt') -Encoding UTF8
foreach ($notice in @('LICENSE-MIT', 'LICENSE-APACHE', 'NOTICE', 'ATTRIBUTION.md')) {
    Copy-Item -LiteralPath (Join-Path $repositoryPath $notice) -Destination $outputPath
}
$licenseOutputPath = Join-Path $outputPath 'licenses'
New-Item -ItemType Directory -Path $licenseOutputPath | Out-Null
foreach ($license in $assetLicenses) { Copy-Item -LiteralPath $license.Source -Destination (Join-Path $licenseOutputPath $license.Name) }

# Preserve every verified distribution asset, including resource JSON and bundled licenses.
# The immutable manifest allowlist excludes any subsequently created auth/config/log files.
$codexOutputPath = Join-Path $outputPath 'codex'
New-Item -ItemType Directory -Path $codexOutputPath | Out-Null
foreach ($runtimeFile in $codexDistributionFiles) {
    $destinationPath = Join-Path $codexOutputPath $runtimeFile.RelativePath
    New-Item -ItemType Directory -Path (Split-Path -Parent $destinationPath) -Force | Out-Null
    Copy-Item -LiteralPath $runtimeFile.Source -Destination $destinationPath
}
$runtimeMetadata = Get-Content -LiteralPath (Join-Path $codexOutputPath 'codex-package.json') -Raw | ConvertFrom-Json
$publicManifest = @{ version = $runtimeMetadata.version; target = $runtimeMetadata.target; files = @($codexDistributionFiles | ForEach-Object { @{ path = $_.RelativePath; bytes = $_.bytes; sha256 = $_.sha256 } }) }
if ($CodexDistributionManifest) {
    $verifiedManifest = Get-Content -LiteralPath $CodexDistributionManifest -Raw | ConvertFrom-Json
    if ($verifiedManifest.version -ne $runtimeMetadata.version -or $verifiedManifest.target -ne $runtimeMetadata.target) { throw 'Codex runtime and verified manifest versions differ.' }
    $publicManifest.platformIntegrity = $verifiedManifest.platformIntegrity
    $publicManifest.wrapperIntegrity = $verifiedManifest.wrapperIntegrity
    $publicManifest.sourceRef = $verifiedManifest.sourceRef
}
[IO.File]::WriteAllText((Join-Path $codexOutputPath 'distribution-manifest.json'), ($publicManifest | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
Copy-Item -LiteralPath $CodexLicensePath -Destination (Join-Path $codexOutputPath 'LICENSE')
Copy-Item -LiteralPath $CodexNoticePath -Destination (Join-Path $codexOutputPath 'NOTICE')

Write-Output "Portable folder assembled: $outputPath"
Write-Output 'Validate this folder before creating a ZIP. Launch Start-yorusphoto.cmd to initialize settings and external MCP configuration.'
