# Keep Cargo's flags out of PowerShell's named-parameter binding.
$CargoArgs = @($args)

$ErrorActionPreference = 'Stop'
$repoPath = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))
$toolchainBin = Join-Path $repoPath '.toolchain\llvm-mingw-20261006-msvcrt-x86_64\bin'
$cargoPath = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
if (-not (Test-Path -LiteralPath (Join-Path $toolchainBin 'x86_64-w64-mingw32-clang.exe'))) {
    throw 'The local LLVM MinGW toolchain is missing; see the yorusphoto build notes.'
}
# These variables affect this PowerShell process and its children only.
$env:PATH = $toolchainBin + ';' + (Split-Path -Parent $cargoPath) + ';' + $env:PATH
$env:CARGO_TARGET_DIR = Join-Path $repoPath 'target\agent-toolchain'
$env:CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = Join-Path $toolchainBin 'x86_64-w64-mingw32-clang.exe'
# Explicit --target checks build their host helpers without RUSTFLAGS. Give Clang
# the Rust-distributed GCC runtime libraries for those native helper links too.
$gnuRuntimeLibraries = Join-Path $env:USERPROFILE '.rustup\toolchains\stable-x86_64-pc-windows-gnu\lib\rustlib\x86_64-pc-windows-gnu\lib\self-contained'
$env:LIBRARY_PATH = $gnuRuntimeLibraries + $(if ($env:LIBRARY_PATH) { ';' + $env:LIBRARY_PATH } else { '' })
$env:RUSTFLAGS = '-C link-self-contained=yes'
$env:CARGO_BUILD_JOBS = '8'
Push-Location -LiteralPath $repoPath
try {
    & $cargoPath +stable-x86_64-pc-windows-gnu @CargoArgs
    $cargoExit = $LASTEXITCODE
} finally {
    Pop-Location
}
exit $cargoExit
