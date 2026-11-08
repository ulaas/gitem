# A keyboard-layout switch while gitem runs, in a real window: the editor must keep taking keys,
# in the new layout. It used to hang at the switch: wgpu's OpenGL backend keeps a hidden window
# on a thread that never reads its messages, and a layout switch waits for every window of the
# process to answer. gitem now leaves that backend out on Windows (chrome::wgpu_options).
#
# Run it against a scratch repository whose note.txt is "first line\n", with scratch settings and
# git config, as IDE-PLAN.md's rules say. It switches to Turkish Q, a layout every Windows has,
# then back to US English: Windows applies a switch to the whole session, so don't type meanwhile.
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
    [DllImport("user32.dll")] public static extern IntPtr LoadKeyboardLayout(string id, uint flags);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
    public static void Key(ushort vk, bool up) { var i = new INPUT[1]; i[0].type = 1; i[0].u.ki.wVk = vk; i[0].u.ki.dwFlags = up ? 2u : 0u; SendInput(1, i, Marshal.SizeOf(typeof(INPUT))); }
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
$h = [IntPtr]::Zero
try {
    for ($i = 0; $i -lt 60; $i++) { Start-Sleep -Milliseconds 500; $h = [W]::Biggest([uint32]$p.Id); $r = New-Object W+RECT; if ($h -ne [IntPtr]::Zero -and [W]::GetWindowRect($h, [ref]$r) -and ($r.R - $r.L) -gt 600) { break } }
    if ($h -eq [IntPtr]::Zero) { throw 'no window' }
    Start-Sleep -Seconds 4
    function Focus { [void][W]::SetForegroundWindow($h); Start-Sleep -Milliseconds 300; $f = [W]::GetForegroundWindow(); $id = 0; [void][W]::GetWindowThreadProcessId($f, [ref]$id); if ($id -ne $p.Id) { throw 'gitem is not the foreground window; stopped before sending anything' } }
    function Tap([ushort]$vk) { Focus; [W]::Key($vk, $false); Start-Sleep -Milliseconds 60; [W]::Key($vk, $true); Start-Sleep -Milliseconds 300 }
    function Chord([ushort[]]$keys) { Focus; foreach ($k in $keys) { [W]::Key($k, $false); Start-Sleep -Milliseconds 40 }; [array]::Reverse($keys); foreach ($k in $keys) { [W]::Key($k, $true) }; Start-Sleep -Milliseconds 250 }

    # Into the text, to its end.
    Focus; [void][W]::SetCursorPos([int]($r.L + ($r.R - $r.L) * 0.6), [int]($r.T + ($r.B - $r.T) * 0.35)); Start-Sleep -Milliseconds 200; [W]::Click(); Start-Sleep -Milliseconds 400
    Chord @(0x11, 0x23)                  # Ctrl+End

    # Turkish Q, as Win+Space would choose it.
    $tr = [W]::LoadKeyboardLayout('0000041F', 0)
    [void][W]::PostMessage($h, 0x50, [IntPtr]1, $tr); Start-Sleep -Seconds 1
    # I, ; and ' on a US keyboard are ı, ş and i in Turkish Q.
    Tap 0x49; Tap 0xBA; Tap 0xDE
    Chord @(0x11, 0x53)                  # Ctrl+S
    Start-Sleep -Seconds 1

    $got = [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes((Join-Path $Repo 'note.txt')))
    if ($got -ne "first line`nışi") { throw "after the switch, the file isn't what was typed: $got" }
    "ok: typed in Turkish Q after a layout switch, and saved"
} finally {
    if ($h -ne [IntPtr]::Zero -and -not $p.HasExited) { $us = [W]::LoadKeyboardLayout('00000409', 1); [void][W]::PostMessage($h, 0x50, [IntPtr]1, $us); Start-Sleep -Milliseconds 500 }
    if (-not $p.HasExited) { Stop-Process -Id $p.Id }
}
