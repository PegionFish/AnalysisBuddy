param([int]$SX, [int]$SY)
# Convert screenshot coords -> physical screen
$ox = 633; $oy = 281; $k = 0.8258
$x = [int]($ox + $SX * $k)
$y = [int]($oy + $SY * $k)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class FF {
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("kernel32.dll")] public static extern uint GetCurrentThreadId();
  [DllImport("user32.dll")] public static extern bool AttachThreadInput(uint a, uint b, bool attach);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool BringWindowToTop(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion u; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  public static void ForceAndClick(IntPtr target, int sx, int sy) {
    IntPtr fg = GetForegroundWindow();
    uint dummyPid;
    uint fgThread = GetWindowThreadProcessId(fg, out dummyPid);
    uint myThread = GetCurrentThreadId();
    bool attached = AttachThreadInput(myThread, fgThread, true);
    ShowWindow(target, 9); // SW_RESTORE
    BringWindowToTop(target);
    SetForegroundWindow(target);
    if (attached) AttachThreadInput(myThread, fgThread, false);
    System.Threading.Thread.Sleep(150);
    SetCursorPos(sx, sy);
    System.Threading.Thread.Sleep(120);
    INPUT[] a = new INPUT[2];
    a[0].type = 0; a[0].u.mi.dwFlags = 0x0002; // LEFTDOWN
    a[1].type = 0; a[1].u.mi.dwFlags = 0x0004; // LEFTUP
    SendInput(2, a, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@
[FF]::ForceAndClick([IntPtr]131892, $x, $y)
Write-Output ("forceclick screenshot($SX,$SY) -> physical($x,$y)")
