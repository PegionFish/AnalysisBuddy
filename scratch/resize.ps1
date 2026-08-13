param([int]$W, [int]$H)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class RS {
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  public static void Resize(IntPtr h, int w, int ht) {
    // SWP_NOMOVE=0x2, SWP_NOZORDER=0x4, SWP_SHOWWINDOW=0x40
    SetWindowPos(h, IntPtr.Zero, 0, 0, w, ht, 0x2 | 0x4 | 0x40);
  }
}
"@
[RS]::Resize([IntPtr]131892, $W, $H)
Write-Output ("resized to " + $W + "x" + $H)
