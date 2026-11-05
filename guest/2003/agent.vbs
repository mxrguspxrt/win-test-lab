' agent.vbs - win-test-lab logon agent for Windows Server 2003 (VBScript, no PowerShell).
'
' Started at every logon by HKLM Run "WTL-Agent":  wscript.exe //B //Nologo C:\WTL\agent.vbs
' It finds the control disc the host's `wtl` wrote (a CD with \LAB\lab.vbs + \LAB\lab.json),
' loads lab.vbs from it and runs the lab, which reports over COM1 (SPEC.md, "Serial protocol").

Dim gAgentFso
Set gAgentFso = CreateObject("Scripting.FileSystemObject")
ExecuteGlobal gAgentFso.OpenTextFile(gAgentFso.BuildPath(gAgentFso.GetParentFolderName(WScript.ScriptFullName), "common.vbs"), 1).ReadAll

Call AgentMain()

' the control disc: a CD drive holding LAB\lab.vbs and LAB\lab.json ("" when there is none)
Function FindLabDir()
  Dim i, d, root
  FindLabDir = ""
  For i = 1 To 3
    For Each d In gFso.Drives
      If d.DriveType = 4 Then
        If d.IsReady Then
          root = d.RootFolder.Path
          If Right(root, 1) <> "\" Then root = root & "\"
          If gFso.FileExists(root & "LAB\lab.vbs") And gFso.FileExists(root & "LAB\lab.json") Then
            FindLabDir = root & "LAB"
            Exit Function
          End If
        End If
      End If
    Next
    WScript.Sleep 2000
  Next
End Function

Sub WaitForever()
  Do
    WScript.Sleep 60000
  Loop
End Sub

Sub AgentMain()
  Dim caption, build, osw, labDir, r, msg
  If CountScript("agent.vbs") > 1 Then WScript.Quit 0
  On Error Resume Next
  caption = "unknown"
  build = ""
  For Each osw In GetObject("winmgmts:\\.\root\cimv2").ExecQuery("Select Caption, BuildNumber From Win32_OperatingSystem")
    caption = Trim(osw.Caption)
    build = osw.BuildNumber
  Next
  SendEvent "AGENT_START", Array("os", caption, "build", build), ""
  ' no "program can't start" / error-report dialogs: they would block an unattended run
  SetRegDword "HKLM\SOFTWARE\Microsoft\PCHealth\ErrorReporting\DoReport", 0
  SetRegDword "HKLM\SOFTWARE\Microsoft\PCHealth\ErrorReporting\ShowUI", 0
  labDir = FindLabDir()
  If labDir = "" Then
    WtlLog "no lab control disc"
    WScript.Quit 0
  End If
  ExecuteGlobal ReadText(labDir & "\lab.vbs")
  If Err.Number = 0 Then r = InvokeLab(labDir)
  If Err.Number <> 0 Then
    msg = Err.Description & " (0x" & Hex(Err.Number) & ")"
    Err.Clear
    SendEvent "ERROR", Empty, msg
  End If
  ' ready, rebooting or already reported: stay alive so the host can look at the screen
  WaitForever
End Sub
