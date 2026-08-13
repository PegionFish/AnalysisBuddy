Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;
public class EW2 {
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);
  public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int m);
  [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int m);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static List<string> ByPidOrClass(uint targetPid, string cls) {
    var res = new List<string>();
    EnumWindows((h, l) => {
      var cn = new StringBuilder(128); GetClassName(h, cn, 128);
      var tt = new StringBuilder(256); GetWindowText(h, tt, 256);
      RECT r; GetWindowRect(h, out r);
      uint pid; GetWindowThreadProcessId(h, out pid);
      if (pid == targetPid || cn.ToString() == cls)
        res.Add(pid + " | " + cn + " | '" + tt + "' | " + r.L + "," + r.T + "," + r.R + "," + r.B);
      return true;
    }, IntPtr.Zero);
    return res;
  }
}
"@
Write-Output "=== PickerHost pid 31892 windows ==="
[EW2]::ByPidOrClass(31892, "__none__") | ForEach-Object { Write-Output $_ }
Write-Output "=== #32770 dialog windows ==="
[EW2]::ByPidOrClass(0, "#32770") | ForEach-Object { Write-Output $_ }
