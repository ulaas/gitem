# Runs the tests in CI on Windows and, if a test binary stops making progress, prints every
# thread's stack before stopping it. A hang there used to hold the runner until GitHub's
# six-hour limit, with nothing in the log to say where it was stuck.
#
# A binary is taken for stuck after $Limit minutes: far more than the slowest file takes.
param([int]$Limit = 15)
$ErrorActionPreference = 'Stop'

$cdb = @(
    "${env:ProgramFiles(x86)}\Windows Kits\10\Debuggers\x64\cdb.exe",
    "$env:ProgramFiles\Windows Kits\10\Debuggers\x64\cdb.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $cdb) { Write-Host 'No cdb here; a stuck test will be stopped without its stacks.' }

$deps = (Resolve-Path 'target\debug\deps' -ErrorAction SilentlyContinue)
$tests = Start-Process -FilePath 'cargo' -ArgumentList @('test', '--workspace', '--locked', '--no-fail-fast') -NoNewWindow -PassThru
$handle = $tests.Handle  # keeps the exit code readable after it ends

while (-not $tests.HasExited) {
    Start-Sleep -Seconds 30
    if (-not $deps) { $deps = (Resolve-Path 'target\debug\deps' -ErrorAction SilentlyContinue) }
    if (-not $deps) { continue }
    $stuck = Get-Process -ErrorAction SilentlyContinue | Where-Object {
        $_.Path -and $_.Path.StartsWith($deps.Path, [StringComparison]::OrdinalIgnoreCase) -and
        ((Get-Date) - $_.StartTime).TotalMinutes -gt $Limit
    }
    foreach ($p in $stuck) {
        Write-Host "::error::$($p.Name) has been running for over $Limit minutes. Its threads:"
        if ($cdb) {
            & $cdb -p $p.Id -noio -c '.symfix; .reload; ~*kn 60; qd' 2>&1 | Write-Host
        }
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    }
}
$tests.WaitForExit()
exit $tests.ExitCode
