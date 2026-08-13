param([long]$Hwnd)
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;
public class W {
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr h, EnumWindowsProc cb, IntPtr l);
  public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern int GetClassName(IntPtr h, StringBuilder s, int m);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static List<string> Kids(IntPtr parent) {
    var res = new List<string>();
    EnumChildWindows(parent, (h, l) => {
      var sb = new StringBuilder(256);
      GetClassName(h, sb, 256);
      RECT r; GetWindowRect(h, out r);
      bool vis = IsWindowVisible(h);
      res.Add(h.ToInt64() + " | " + sb + " | vis=" + vis + " | " + r.L + "," + r.T + "," + r.R + "," + r.B);
      return true;
    }, IntPtr.Zero);
    return res;
  }
}
"@
$h = [IntPtr]$Hwnd
[W]::Kids($h) | ForEach-Object { Write-Output $_ }
