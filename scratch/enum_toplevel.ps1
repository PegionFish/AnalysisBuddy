Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;
public class EW {
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);
  public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int m);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int m);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static List<string> All() {
    var res = new List<string>();
    EnumWindows((h, l) => {
      if (!IsWindowVisible(h)) return true;
      var cn = new StringBuilder(128); GetClassName(h, cn, 128);
      var tt = new StringBuilder(256); GetWindowText(h, tt, 256);
      RECT r; GetWindowRect(h, out r);
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (r.R - r.L > 50 && r.B - r.T > 50)
        res.Add(pid + " | " + cn + " | '" + tt + "' | " + r.L + "," + r.T + "," + r.R + "," + r.B);
      return true;
    }, IntPtr.Zero);
    return res;
  }
}
"@
[EW]::All() | ForEach-Object { Write-Output $_ }
