param([string]$Text, [switch]$Enter)
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class SK {
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion u; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion { [FieldOffset(0)] public KEYBDINPUT ki; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT {
    public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo;
  }
  [DllImport("user32.dll")] public static extern uint SendInput(uint n, INPUT[] p, int cb);
  [DllImport("user32.dll")] public static extern short VkKeyScan(char c);
  const uint KEYUP = 0x0002, SCANCODE = 0x0008, UNICODE = 0x0004;
  static void Key(ushort vk, bool up) {
    INPUT[] a = new INPUT[1]; a[0].type = 1;
    a[0].u.ki.wVk = vk; a[0].u.ki.dwFlags = up ? KEYUP : 0;
    SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
  }
  static void Uni(char c, bool up) {
    INPUT[] a = new INPUT[1]; a[0].type = 1;
    a[0].u.ki.wScan = (ushort)c; a[0].u.ki.dwFlags = UNICODE | (up ? KEYUP : 0);
    SendInput(1, a, Marshal.SizeOf(typeof(INPUT)));
  }
  public static void Type(string s) {
    foreach (char c in s) { Uni(c, false); Uni(c, true); System.Threading.Thread.Sleep(8); }
  }
  public static void PressEnter() { Key(0x0D, false); Key(0x0D, true); }
}
"@
if ($Text) { [SK]::Type($Text) }
if ($Enter) { Start-Sleep -Milliseconds 200; [SK]::PressEnter() }
Write-Output ("typed len=" + $Text.Length + " enter=" + [bool]$Enter)
