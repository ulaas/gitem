# Builds gitem's size-optimised binary and its installer, leaving both in target\dist. Needs Inno Setup
# 6 (iscc) on PATH. It installs nothing: running the setup it makes is a separate, deliberate step.
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$line = Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $line) { throw 'No version in Cargo.toml' }
$version = $line.Matches[0].Groups[1].Value
if (-not $SkipBuild) { cargo build --manifest-path (Join-Path $root 'Cargo.toml') --profile dist -p gitem; if ($LASTEXITCODE) { exit $LASTEXITCODE } }
$exe = Join-Path $root 'target\dist\gitem.exe'
if (-not (Test-Path $exe)) { throw "No binary at $exe" }
iscc "/DAppVersion=$version" "/DSourceExe=$exe" "/O$(Join-Path $root 'target\dist')" (Join-Path $PSScriptRoot 'gitem.iss')
exit $LASTEXITCODE
