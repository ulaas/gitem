# The editor in a real window, with real Windows input: a click, Unicode text, Enter, paste
# from and copy to the system clipboard, and Ctrl+S, checking the file and clipboard after.
# Run it against a scratch repository whose note.txt is "first line\r\nsecond line\r\n",
# with scratch settings and git config, as IDE-PLAN.md's rules say. It changes the clipboard.
# Every input goes only after checking the foreground window is the one this script started.
param([string]$Exe, [string]$Repo, [string]$Cfg, [string]$GitCfg)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public static class W {
    [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
    [StructLayout(LayoutKind.Explicit)] public struct U { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
    [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public U u; }
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] i, int size);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    public static void Key(ushort vk, bool up) { var i = new INPUT[1]; i[0].type = 1; i[0].u.ki.wVk = vk; i[0].u.ki.dwFlags = up ? 2u : 0u; SendInput(1, i, Marshal.SizeOf(typeof(INPUT))); }
    public static void Char(char c) { var i = new INPUT[2]; for (int k = 0; k < 2; k++) { i[k].type = 1; i[k].u.ki.wScan = c; i[k].u.ki.dwFlags = 4u | (k == 1 ? 2u : 0u); } SendInput(2, i, Marshal.SizeOf(typeof(INPUT))); }
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    /// The biggest visible top-level window of a process: its real main window.
    public static IntPtr Biggest(uint pid) {
        IntPtr best = IntPtr.Zero; long area = 0;
        EnumWindows((h, l) => { uint id; GetWindowThreadProcessId(h, out id); RECT r; if (id == pid && IsWindowVisible(h) && GetWindowRect(h, out r)) { long a = (long)(r.R - r.L) * (r.B - r.T); if (a > area) { area = a; best = h; } } return true; }, IntPtr.Zero);
        return best;
    }
    public static void Click() { var i = new INPUT[2]; i[0].type = 0; i[0].u.mi.dwFlags = 2; i[1].type = 0; i[1].u.mi.dwFlags = 4; SendInput(2, i, Marshal.SizeOf(typeof(INPUT))); }
}
"@

$env:GITEM_CONFIG_DIR = $Cfg
$env:GIT_CONFIG_GLOBAL = $GitCfg
$env:GIT_CONFIG_NOSYSTEM = '1'
$p = Start-Process -FilePath $Exe -ArgumentList @("`"$Repo`"", '--edit', 'note.txt') -PassThru
try {
    $h = [IntPtr]::Zero
    for ($i = 0; $i -lt 60; $i++) { Start-Sleep -Milliseconds 500; $h = [W]::Biggest([uint32]$p.Id); $rr = New-Object W+RECT; if ($h -ne [IntPtr]::Zero -and [W]::GetWindowRect($h, [ref]$rr) -and ($rr.R - $rr.L) -gt 600) { break } }
    if ($h -eq [IntPtr]::Zero) { throw 'no window' }
    Start-Sleep -Seconds 4
    function Ours { $f = [W]::GetForegroundWindow(); $id = 0; [void][W]::GetWindowThreadProcessId($f, [ref]$id); return $id -eq $p.Id }
    function Focus { [void][W]::SetForegroundWindow($h); Start-Sleep -Milliseconds 300; if (-not (Ours)) { throw 'gitem is not the foreground window; stopped before sending anything' } }
    function Chord([ushort[]]$keys) { Focus; foreach ($k in $keys) { [W]::Key($k, $false); Start-Sleep -Milliseconds 40 }; [array]::Reverse($keys); foreach ($k in $keys) { [W]::Key($k, $true) }; Start-Sleep -Milliseconds 250 }
    function Send-Text([string]$s) { Focus; foreach ($c in $s.ToCharArray()) { [W]::Char($c); Start-Sleep -Milliseconds 15 }; Start-Sleep -Milliseconds 250 }
    $CTRL = 0x11
    # Into the text, then to its end.
    Focus
    $r = New-Object W+RECT; [void][W]::GetWindowRect($h, [ref]$r)
    [void][W]::SetCursorPos([int]($r.L + ($r.R - $r.L) * 0.6), [int]($r.T + ($r.B - $r.T) * 0.35))
    Start-Sleep -Milliseconds 200; Focus; [W]::Click(); Start-Sleep -Milliseconds 400
    Chord @($CTRL, 0x23)            # Ctrl+End

    # Unicode text, as an input method or the touch keyboard delivers it.
    Send-Text "çğış 中文 ✓"
    Chord @(0x0D)                    # Enter

    # Paste from the system clipboard, then copy everything back out of it.
    Set-Clipboard -Value 'from the clipboard'
    Chord @($CTRL, 0x56)             # Ctrl+V
    Chord @($CTRL, 0x41)             # Ctrl+A
    Chord @($CTRL, 0x43)             # Ctrl+C
    Start-Sleep -Milliseconds 300
    $copied = Get-Clipboard -Raw
    Chord @(0x23)                    # End, to drop the selection
    Chord @($CTRL, 0x53)             # Ctrl+S
    Start-Sleep -Seconds 1

    $bytes = [IO.File]::ReadAllBytes((Join-Path $Repo 'note.txt'))
    $want = "first line`r`nsecond line`r`nçğış 中文 ✓`r`nfrom the clipboard"
    $got = [Text.Encoding]::UTF8.GetString($bytes)
    if ($got -ne $want) { throw "the file isn't what was typed: $got" }
    if ($copied -ne $want) { throw "the clipboard isn't what was copied: $copied" }
    "ok: typed, pasted, copied and saved in a real window"

} finally {
    if (-not $p.HasExited) { Stop-Process -Id $p.Id }
}
