# Runs the tests in CI on Windows and, if a test binary stops making progress, saves a minidump
# of it, with the binary and its symbols, before stopping it. A hang there used to hold the
# runner until GitHub's six-hour limit, with nothing in the log to say where it was stuck. The
# runner has no debugger, so the dump is uploaded and read elsewhere, with `cdb -z`.
#
# A binary is taken for stuck after $Limit minutes: far more than the slowest file takes.
param([int]$Limit = 10, [string]$Out = 'hang-dumps')
$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class Dump {
    [DllImport("dbghelp.dll", SetLastError = true)]
    static extern bool MiniDumpWriteDump(IntPtr process, uint pid, SafeFileHandle file, uint type, IntPtr e, IntPtr u, IntPtr c);
    // Normal, with thread info, handle data and the memory that stacks point at: enough for
    // every thread's stack and what it's waiting on, without the whole heap.
    public static bool Write(IntPtr process, uint pid, string path) {
        using (var f = System.IO.File.Create(path)) {
            return MiniDumpWriteDump(process, pid, f.SafeFileHandle, 0x1000 | 0x0004 | 0x0040, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
        }
    }
}
"@

New-Item -ItemType Directory -Force $Out | Out-Null
$deps = $null
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
        $name = [IO.Path]::GetFileNameWithoutExtension($p.Path)
        $dump = Join-Path $Out "$name.dmp"
        $ok = $false
        try { $ok = [Dump]::Write($p.Handle, [uint32]$p.Id, $dump) } catch { Write-Host "dump failed: $_" }
        Copy-Item $p.Path $Out -ErrorAction SilentlyContinue
        # rustc may spell the PDB with underscores for the hyphens.
        foreach ($pdb in @($name, $name.Replace('-', '_'))) { Copy-Item (Join-Path (Split-Path $p.Path) "$pdb.pdb") $Out -ErrorAction SilentlyContinue }
        Write-Host "::error::$name has been running for over $Limit minutes; dumped: $ok. It's stopped, and the dump is in the job's artifacts."
        Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
    }
}
$tests.WaitForExit()
exit $tests.ExitCode
