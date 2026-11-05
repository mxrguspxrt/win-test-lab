' setup.vbs - win-test-lab BUILD phase for Windows Server 2003 (VBScript, no PowerShell).
'
' Started by C:\wtl-first.cmd (HKLM Run entry WTL-Build, merged from image.reg) at the first
' automatic logon of the imported, sysprepped image, from the WTL_BUILD disc: <CD>:\WTL\setup.vbs. It copies the
' guest scripts to C:\WTL, gives Administrator the build password, makes the logon automatic,
' turns off what gets in the way of an unattended machine (firewall, shutdown tracker, error
' dialogs), reports BUILD_DONE on COM1, installs the agent autostart and shuts the machine down.
' No Windows Update: the image came with its updates.
'
' Events go to COM1 and to C:\WTL\agent.log (SPEC.md, "Serial protocol").

Dim gSetupFso
Set gSetupFso = CreateObject("Scripting.FileSystemObject")
ExecuteGlobal gSetupFso.OpenTextFile(gSetupFso.BuildPath(gSetupFso.GetParentFolderName(WScript.ScriptFullName), "common.vbs"), 1).ReadAll

Call SetupMain()

' ---------------------------------------------------------------- the build

Sub CopyGuestFiles(srcDir)
  Dim f
  EnsureFolder gWtlDir
  gFso.CopyFile srcDir & "\*", gWtlDir & "\", True
  ' files copied from a CD keep the read-only attribute, which would block a later update
  For Each f In gFso.GetFolder(gWtlDir).Files
    If (f.Attributes And 1) <> 0 Then f.Attributes = f.Attributes And (Not 1)
  Next
End Sub

' the local Administrator gets the build password, and the machine logs on as it from now on
Sub SetAdminLogon(password)
  Dim adm, winlogon
  gStage = "setting the Administrator password"
  Set adm = GetObject("WinNT://./Administrator,user")
  adm.SetPassword password
  adm.SetInfo
  gStage = "setting the automatic logon"
  winlogon = "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\"
  SetRegStr winlogon & "AutoAdminLogon", "1"
  SetRegStr winlogon & "DefaultUserName", "Administrator"
  SetRegStr winlogon & "DefaultPassword", password
  SetRegStr winlogon & "DefaultDomainName", gNet.ComputerName
  DelReg winlogon & "AutoLogonCount"
End Sub

Sub ConfigureSystem()
  gStage = "configuring the system"
  ' Windows Firewall (Server 2003 SP1 and later)
  RunLog "netsh.exe firewall set opmode disable"
  ' no Shutdown Event Tracker dialog on shutdown / restart
  SetRegDword "HKLM\SOFTWARE\Policies\Microsoft\Windows NT\Reliability\ShutdownReasonOn", 0
  SetRegDword "HKLM\SOFTWARE\Policies\Microsoft\Windows NT\Reliability\ShutdownReasonUI", 0
  ' no error-reporting dialogs that would wait for a click
  SetRegDword "HKLM\SOFTWARE\Microsoft\PCHealth\ErrorReporting\DoReport", 0
  SetRegDword "HKLM\SOFTWARE\Microsoft\PCHealth\ErrorReporting\ShowUI", 0
  ' no screen saver, and let the host power the machine off whoever is logged on
  SetRegStr "HKCU\Control Panel\Desktop\ScreenSaveActive", "0"
  SetRegStr "HKCU\Control Panel\Desktop\AutoEndTasks", "1"
  SetRegDword "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System\ShutdownWithoutLogon", 1
End Sub

Sub SetupMain()
  Dim srcDir, cfg, password, caption, osw, msg
  ' Under "On Error Resume Next" a failing step does not stop the next one, so every step is
  ' guarded by "no error so far"; the first error is reported as BUILD_FAIL at the end.
  On Error Resume Next
  srcDir = gSetupFso.GetParentFolderName(WScript.ScriptFullName)
  gStage = "copying the guest scripts"
  WtlLog "setup.vbs (2003) start from " & srcDir
  If Err.Number = 0 Then CopyGuestFiles srcDir
  If Err.Number = 0 Then Set cfg = JsonParse(ReadText(gWtlDir & "\build.json"))
  If Err.Number = 0 Then
    password = JStr(cfg, "password")
    If password = "" Then password = "WinTest!2026"
    caption = "unknown"
    For Each osw In GetObject("winmgmts:\\.\root\cimv2").ExecQuery("Select Caption From Win32_OperatingSystem")
      caption = Trim(osw.Caption)
    Next
    WtlLog "os=" & JStr(cfg, "os") & " caption=" & caption & " user=" & gNet.UserDomain & "\" & gNet.UserName
  End If
  If Err.Number = 0 Then SetAdminLogon password
  If Err.Number = 0 Then WtlLog "Administrator password + automatic logon set"
  If Err.Number = 0 Then ConfigureSystem
  If Err.Number = 0 Then WtlLog "system configuration applied"
  If Err.Number = 0 Then
    ' BUILD_DONE first, the agent autostart after it (as in the PowerShell guests)
    SendEvent "BUILD_DONE", Empty, ""
    SetRegStr "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Run\WTL-Agent", "wscript.exe //B //Nologo " & gWtlDir & "\agent.vbs"
    LogFile "agent autostart installed (HKLM Run WTL-Agent)"
    ' this build script ran from the Run entry image.reg registered: once only
    DelReg "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Run\WTL-Build"
    WScript.Sleep 2000
    gSh.Run "shutdown.exe -s -t 0 -f", 0, False
    WScript.Sleep 600000
  End If
  If Err.Number <> 0 Then
    msg = Err.Description & " (0x" & Hex(Err.Number) & ") at: " & gStage
    Err.Clear
    SendEvent "BUILD_FAIL", Empty, msg
    ' leave the machine up so the host can take a screenshot
    Do
      WScript.Sleep 3600000
    Loop
  End If
End Sub
