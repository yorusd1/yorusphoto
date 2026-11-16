<#
.SYNOPSIS
  Start the portable editor and its authenticated local MCP control endpoint.
#>
param(
    [ValidateRange(1, 65535)] [int] $ControlPort = 50495,
    [switch] $InitializeOnly
)

$ErrorActionPreference = 'Stop'
$packagePath = [IO.Path]::GetFullPath($PSScriptRoot)
$editorPath = Join-Path $packagePath 'yorusphoto.exe'
$cliPath = Join-Path $packagePath 'yorusphoto-cli.exe'
$dataPath = Join-Path $packagePath 'data'
# Direct executable launches use yorusphotoData. Reuse that existing conversation
# when the MCP launcher has no chat history of its own; never copy or merge histories.
$directDataPath = Join-Path $packagePath 'yorusphotoData'
if ((Test-Path -LiteralPath (Join-Path $directDataPath 'codex-chat.json') -PathType Leaf) -and
    -not (Test-Path -LiteralPath (Join-Path $dataPath 'codex-chat.json') -PathType Leaf)) {
    $dataPath = $directDataPath
}
$workspacePath = Join-Path $packagePath 'workspace'
$tokenPath = Join-Path $dataPath 'control.token'
$mcpPath = Join-Path $packagePath 'mcp-config.json'

foreach ($binary in @($editorPath, $cliPath)) {
    if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
        throw "The portable package is incomplete: missing $binary"
    }
}
New-Item -ItemType Directory -Path $dataPath -Force | Out-Null
New-Item -ItemType Directory -Path $workspacePath -Force | Out-Null

# The engine creates its token atomically. Protect its parent before creation on Windows,
# where the Rust host does not set Unix-style file permissions. Never read or print the token.
# Only the access rules change: the folder already belongs to you, and setting an owner needs a
# privilege many accounts (and scripted shells) don't hold. A failure warns instead of stopping
# the launch.
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
function Protect-Private([string] $Path, [bool] $IsDirectory) {
    try {
        if ($IsDirectory) {
            $acl = [Security.AccessControl.DirectorySecurity]::new()
            $inherit = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit
        } else {
            $acl = [Security.AccessControl.FileSecurity]::new()
            $inherit = [Security.AccessControl.InheritanceFlags]::None
        }
        $acl.SetAccessRuleProtection($true, $false)
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new(
            $identity.User,
            [Security.AccessControl.FileSystemRights]::FullControl,
            $inherit,
            [Security.AccessControl.PropagationFlags]::None,
            [Security.AccessControl.AccessControlType]::Allow
        ))
        (Get-Item -LiteralPath $Path -Force).SetAccessControl($acl)
    } catch {
        Write-Warning "Could not limit $Path to your account ($($_.Exception.Message)). Keep that folder private: it holds the control token."
    }
}
Protect-Private $dataPath $true
if (Test-Path -LiteralPath $tokenPath -PathType Leaf) {
    Protect-Private $tokenPath $false
}

# Config contains only the private token FILE PATH, never the credential itself. Regenerate
# absolute paths after moving or extracting the package to a different directory.
$configuration = [ordered]@{
    mcpServers = [ordered]@{
        yorusphoto = [ordered]@{
            command = $cliPath
            args = @('mcp', '--bridge', "127.0.0.1:$ControlPort", '--control-token-file', $tokenPath)
        }
    }
}
$configuration | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath $mcpPath -Encoding UTF8

$env:PHOTOCRAFT_CONFIG_DIR = $dataPath
$codexBinPath = Join-Path $packagePath 'codex\bin'
$codexToolsPath = Join-Path $packagePath 'codex\codex-path'
$env:PATH = $codexBinPath + ';' + $codexToolsPath + ';' + $env:PATH
if ($InitializeOnly) {
    Write-Output "Initialized portable settings, workspace, and MCP configuration at $packagePath"
    return
}

# Start-Process combines ArgumentList into a Windows command line; quote every path so
# extracted package directories containing spaces remain intact.
$launchArguments = @(
    '--control', [string]$ControlPort,
    '--control-token-file', ('"' + $tokenPath + '"'),
    '--automation-read-root', ('"' + $workspacePath + '"'),
    '--automation-write-root', ('"' + $workspacePath + '"')
)
Start-Process -FilePath $editorPath -WorkingDirectory $packagePath -ArgumentList $launchArguments -WindowStyle Normal | Out-Null
Write-Output "Started yorusphoto. External MCP configuration: $mcpPath"
