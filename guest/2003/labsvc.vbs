' labsvc.vbs - win-test-lab privileged worker for Windows Server 2003 (VBScript, no PowerShell).
'
' Baked into the 0 state: copied from the control disc to C:\WTL during the 0-state build and
' started at every boot by the scheduled task "WTL-Svc", which runs as SYSTEM. It does what the
' logged-on lab user cannot: give the mounted discs their drive letters and run "system" steps.
' It talks only to the file system (the agent owns COM1), through a request / response queue of
' plain key=value files in C:\WTL\svc:
'
'   <id>.req   action=mount  letter=X
'              action=run    path=.. args=.. cwd=.. timeout=60 out=.. err=.. keep_running=0|1
'   <id>.res   ok=1 [letter=X:] [exit=N secs=S] [timed_out=1 secs=S] [running=1 secs=S]
'              ok=0 error=..

Dim gSvcFso
Set gSvcFso = CreateObject("Scripting.FileSystemObject")
ExecuteGlobal gSvcFso.OpenTextFile(gSvcFso.BuildPath(gSvcFso.GetParentFolderName(WScript.ScriptFullName), "common.vbs"), 1).ReadAll

Call SvcMain()

Sub SvcLog(text)
  On Error Resume Next
  AppendText gWtlDir & "\labsvc.log", Stamp() & " " & text & vbCrLf
  Err.Clear
  On Error GoTo 0
End Sub

' action=mount: the disc labelled WTL<LETTER> gets the drive letter LETTER:
Sub SvcMount(req, res)
  Dim letter, label, target, wmi, v, c, cur, i
  letter = UCase(req("letter"))
  label = "WTL" & letter
  target = letter & ":"
  Set wmi = GetObject("winmgmts:\\.\root\cimv2")
  Set v = Nothing
  For Each c In wmi.ExecQuery("Select * From Win32_Volume Where Label = '" & label & "'")
    Set v = c
    Exit For
  Next
  If v Is Nothing Then Fail "no disc labelled " & label
  cur = ""
  If Not IsNull(v.DriveLetter) Then cur = UCase(CStr(v.DriveLetter))
  If cur <> target Then
    ' free the letter if another volume holds it
    For Each c In wmi.ExecQuery("Select * From Win32_Volume Where DriveLetter = '" & target & "'")
      c.DriveLetter = Null
      c.Put_
    Next
    v.DriveLetter = target
    v.Put_
    For i = 1 To 20
      If gFso.DriveExists(letter) Then Exit For
      WScript.Sleep 500
    Next
  End If
  res("ok") = "1"
  res("letter") = target
End Sub

' action=run: run a program as SYSTEM, capture its output, wait for it up to its timeout
Sub SvcRun(id, req, res)
  Dim state, exitCode, secs, errMsg, keep, timeoutS
  If req("user") <> "" Then
    res("ok") = "0"
    res("error") = "running a step as another user is not supported on Windows Server 2003 yet (use system or the logon user)"
    Exit Sub
  End If
  keep = (req("keep_running") = "1")
  timeoutS = 60
  If IsNumeric(req("timeout")) Then timeoutS = CLng(req("timeout"))
  If timeoutS < 5 Then timeoutS = 5
  state = RunProgram("svc" & id, req("path"), req("args"), req("cwd"), req("out"), req("err"), timeoutS, keep, exitCode, secs, errMsg)
  Select Case state
    Case "exit"
      res("ok") = "1"
      res("exit") = CStr(exitCode)
      res("secs") = SecsText(secs)
    Case "timeout"
      res("ok") = "1"
      res("timed_out") = "1"
      res("secs") = SecsText(secs)
    Case "running"
      res("ok") = "1"
      res("running") = "1"
      res("secs") = SecsText(secs)
    Case Else
      res("ok") = "0"
      res("error") = errMsg
  End Select
End Sub

Sub HandleRequest(reqPath)
  Dim req, res, resPath, action, id
  Set res = NewDict()
  id = gFso.GetBaseName(reqPath)
  On Error Resume Next
  Set req = KvParse(ReadText(reqPath))
  action = LCase(req("action"))
  Select Case action
    Case "mount"
      SvcMount req, res
    Case "run"
      SvcRun id, req, res
    Case Else
      res("ok") = "0"
      res("error") = "unknown action '" & action & "'"
  End Select
  If Err.Number <> 0 Then
    res.RemoveAll
    res("ok") = "0"
    res("error") = Err.Description & " (0x" & Hex(Err.Number) & ")"
    Err.Clear
  End If
  On Error GoTo 0
  resPath = Left(reqPath, Len(reqPath) - 4) & ".res"
  KvWrite resPath, res
  DeleteIf reqPath
  SvcLog "handled " & id & " (" & action & ") ok=" & res("ok")
End Sub

Sub SvcMain()
  Dim svcDir, f, names, n, i
  If CountScript("labsvc.vbs") > 1 Then WScript.Quit 0
  svcDir = gWtlDir & "\svc"
  EnsureFolder gWtlDir
  EnsureFolder svcDir
  SvcLog "labsvc started as " & gNet.UserDomain & "\" & gNet.UserName
  Do
    On Error Resume Next
    Set names = NewDict()
    n = 0
    For Each f In gFso.GetFolder(svcDir).Files
      If LCase(gFso.GetExtensionName(f.Name)) = "req" Then
        names.Add CStr(n), f.Path
        n = n + 1
      End If
    Next
    For i = 0 To n - 1
      HandleRequest names(CStr(i))
    Next
    If Err.Number <> 0 Then
      SvcLog "loop error: " & Err.Description
      Err.Clear
    End If
    On Error GoTo 0
    WScript.Sleep 200
  Loop
End Sub
