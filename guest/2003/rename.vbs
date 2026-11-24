' rename.vbs - win-test-lab: the privileged half of renaming a Windows Server 2003 machine of an
' environment (guest side), run as SYSTEM by the worker when lab.vbs asks (see InvokeLabRename
' there). It ships on the control disc next to lab.vbs, so a 0 state built by an older wtl has it
' too. Self-contained (no common.vbs: that one is baked into the 0 state and may be older).
'
'   cscript //nologo rename.vbs NEWNAME   rename this computer, point the automatic logon
'       (COMPUTER\user) at the new name, reboot in 5 s. Exit 0 = done; else the error is on
'       stderr.
Option Explicit
Dim newName, cs, rc, sh
If WScript.Arguments.Count < 1 Then
  WScript.StdErr.WriteLine "usage: rename.vbs NEWNAME"
  WScript.Quit 2
End If
newName = WScript.Arguments(0)
rc = 1
For Each cs In GetObject("winmgmts:\\.\root\cimv2").ExecQuery("Select * From Win32_ComputerSystem")
  rc = cs.Rename(newName)
Next
If rc <> 0 Then
  WScript.StdErr.WriteLine "Win32_ComputerSystem.Rename(" & newName & ") returned " & rc
  WScript.Quit 1
End If
Set sh = CreateObject("WScript.Shell")
sh.RegWrite "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\DefaultDomainName", newName, "REG_SZ"
sh.Run "shutdown.exe -r -t 5 -f", 0, False
WScript.Quit 0
