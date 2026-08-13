param([long]$Hwnd)
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
using System.Collections.Generic;
public class W2 {
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern IntPtr GetDC(IntPtr h);
  [DllImport("gdi32.dll")] public static extern int GetDeviceCaps(IntPtr hdc, int idx);
  [DllImport("user32.dll")] public static extern int ReleaseDC(IntPtr h, IntPtr hdc);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  public static string Info(IntPtr h) {
    RECT wr; GetWindowRect(h, out wr);
    RECT cr; GetClientRect(h, out cr);
    POINT tl = new POINT(); tl.X = cr.L; tl.Y = cr.T; ClientToScreen(h, ref tl);
    POINT br = new POINT(); br.X = cr.R; br.Y = cr.B; ClientToScreen(h, ref br);
    IntPtr dc = GetDC(IntPtr.Zero);
    int dpiX = GetDeviceCaps(dc, 88); int dpiY = GetDeviceCaps(dc, 90);
    ReleaseDC(IntPtr.Zero, dc);
    return "winRect=" + wr.L + "," + wr.T + "," + wr.R + "," + wr.B
      + " clientScreen=" + tl.X + "," + tl.Y + "->" + br.X + "," + br.Y
      + " dpi=" + dpiX + "x" + dpiY;
  }
}
"@
Write-Output ("PARENT " + $Hwnd + ": " + [W2]::Info([IntPtr]$Hwnd))
foreach ($c in @(197374,197376,4522252,5048544,1574180)) {
  Write-Output ("CHILD  " + $c + ": " + [W2]::Info([IntPtr]$c))
}
