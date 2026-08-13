param([int]$X, [int]$Y)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class SI {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion u; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion {
    [FieldOffset(0)] public MOUSEINPUT mi;
  }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT {
    public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo;
  }
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  const uint MOUSEEVENTF_MOVE = 0x0001, LEFTDOWN = 0x0002, LEFTUP = 0x0004, ABSOLUTE = 0x8000, VIRTUALDESK = 0x4000;
  public static void Click(int sx, int sy) {
    SetCursorPos(sx, sy);
    System.Threading.Thread.Sleep(120);
    INPUT[] arr = new INPUT[2];
    arr[0].type = 0; arr[0].u.mi.dwFlags = LEFTDOWN;
    arr[1].type = 0; arr[1].u.mi.dwFlags = LEFTUP;
    SendInput(2, arr, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@
[SI]::Click($X, $Y)
Write-Output ("clicked " + $X + "," + $Y)
