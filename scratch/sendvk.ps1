param([int]$VK)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class VKS {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion u; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion { [FieldOffset(0)] public KEYBDINPUT ki; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  public static void Press(int vk) {
    INPUT[] a = new INPUT[2];
    a[0].type = 1; a[0].u.ki.wVk = (ushort)vk; a[0].u.ki.dwFlags = 0;
    a[1].type = 1; a[1].u.ki.wVk = (ushort)vk; a[1].u.ki.dwFlags = 0x0002;
    SendInput(2, a, Marshal.SizeOf(typeof(INPUT)));
  }
}
"@
[VKS]::Press($VK)
Write-Output ("vk=" + $VK)
