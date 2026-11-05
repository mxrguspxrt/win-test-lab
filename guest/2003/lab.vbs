' lab.vbs - win-test-lab lab, guest side, for Windows Server 2003 (VBScript, no PowerShell).
'
' Read from the control disc (\LAB\lab.vbs) and run by agent.vbs with ExecuteGlobal at every
' logon; it uses the agent's / common.vbs's SendEvent, JSON and RunProgram helpers. It continues
' where it left off (state in C:\WTL\lab-state.txt) and handles one standalone (workgroup)
' machine: no domain, no server roles, no lab LAN.
'
'   phase new:     the SYSTEM worker, the lab's local users and groups, the Administrator's lab
'                  password, the computer renamed to its machine key, automatic logon as the
'                  logon user -> LAB_JOINED, reboot
'   phase joined:  logged on as that user -> LAB_CLIENT_READY, then LAB_IDLE (0-state build) or
'                  the run's mounts and steps (LAB_MOUNT, OUTPUT, LAB_STEP, LAB_RUN_DONE)
'
' InvokeLab(labDir) returns "ready", "done" (steps ran), "reboot" or "fail".
' A step may run as "system" (via the SYSTEM worker) or as "logon" (this machine's logged-on
' user). Running a step as another lab user is not supported on Server 2003 yet.

Dim gLabStateFile, gLabWinlogon, gSlow, gSvcAnswered
gLabStateFile = "C:\WTL\lab-state.txt"
gLabWinlogon = "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\"
gSlow = 1
gSvcAnswered = False

' ---------------------------------------------------------------- state + small helpers

Function GetLabPhase()
  Dim t
  t = Trim(ReadText(gLabStateFile))
  If t = "" Then t = "new"
  GetLabPhase = t
End Function

Sub SetLabPhase(phase)
  WriteText gLabStateFile, phase
End Sub

Sub LabPhase(text)
  SendEvent "LAB_PHASE", Empty, text
End Sub

Sub SetLabAutoLogon(domain, user, password)
  SetRegStr gLabWinlogon & "AutoAdminLogon", "1"
  SetRegStr gLabWinlogon & "DefaultUserName", user
  SetRegStr gLabWinlogon & "DefaultDomainName", domain
  SetRegStr gLabWinlogon & "DefaultPassword", password
  DelReg gLabWinlogon & "AutoLogonCount"
End Sub

' lets the host power the machine off (ACPI) whoever is logged on, without waiting on programs
Sub SetShutdownFriendly()
  Dim hive, p
  SetRegDword "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System\ShutdownWithoutLogon", 1
  SetRegStr "HKCU\Control Panel\Desktop\AutoEndTasks", "1"
  ' new profiles (the lab's users) start from the Default User hive
  hive = ""
  For Each p In Array("C:\Documents and Settings\Default User\NTUSER.DAT", "C:\Users\Default\NTUSER.DAT")
    If gFso.FileExists(p) Then
      hive = p
      Exit For
    End If
  Next
  If hive <> "" Then
    RunLog "reg.exe load HKU\WTLDefault """ & hive & """"
    RunLog "reg.exe add ""HKU\WTLDefault\Control Panel\Desktop"" /v AutoEndTasks /t REG_SZ /d 1 /f"
    RunLog "reg.exe unload HKU\WTLDefault"
  End If
End Sub

' the agent runs as the logon user from now on: it writes its log and output under C:\WTL
Sub GrantUsersModify(dir)
  RunLog "echo y| cacls """ & dir & """ /T /E /G Users:C"
End Sub

' copy the worker + agent + library to their baked places, and make the worker start at every
' boot as SYSTEM (a scheduled task; Server 2003's schtasks has no /F and no /RL)
Sub RegisterSvc(labDir)
  Dim f, src, dst, tr, out, rc, ru, ok
  gStage = "registering the SYSTEM worker"
  For Each f In Array("labsvc.vbs", "agent.vbs", "common.vbs")
    src = labDir & "\" & f
    If gFso.FileExists(src) Then
      dst = gWtlDir & "\" & f
      gFso.CopyFile src, dst, True
      gFso.GetFile(dst).Attributes = 0
    End If
  Next
  tr = Chr(34) & "wscript.exe //B //Nologo " & gWtlDir & "\labsvc.vbs" & Chr(34)
  rc = RunCmd("schtasks.exe /Delete /TN WTL-Svc /F", out)
  ok = False
  For Each ru In Array("SYSTEM", "NT AUTHORITY\SYSTEM")
    If Not ok Then
      rc = RunCmd("schtasks.exe /Create /TN WTL-Svc /TR " & tr & " /SC ONSTART /RU " & Chr(34) & ru & Chr(34), out)
      If rc = 0 Then
        ok = True
      Else
        LabPhase "schtasks /Create (run as " & ru & ") exit=" & rc & " " & Trim(out)
      End If
    End If
  Next
  If Not ok Then Fail "could not register the SYSTEM worker task WTL-Svc"
  RunLog "schtasks.exe /Run /TN WTL-Svc"
End Sub

' ---------------------------------------------------------------- the machine

' the local Administrator gets the lab's admin_password (the image has the build one), and the
' next automatic logon uses it
Sub SetLocalAdmin(cfg)
  Dim pw, adm
  pw = JStr(cfg, "admin_password")
  If pw = "" Then Exit Sub
  gStage = "setting the Administrator password"
  Set adm = GetObject("WinNT://./Administrator,user")
  adm.SetPassword pw
  adm.SetInfo
  SetLabAutoLogon gNet.ComputerName, "Administrator", pw
End Sub

Function GetOrCreateGroup(comp, machine, g)
  Dim grp
  On Error Resume Next
  Set grp = GetObject("WinNT://" & machine & "/" & g & ",group")
  If Err.Number <> 0 Then
    Err.Clear
    Set grp = comp.Create("group", g)
    grp.SetInfo
  End If
  If Err.Number <> 0 Then
    On Error GoTo 0
    Fail "creating the local group " & g & " failed"
  End If
  On Error GoTo 0
  Set GetOrCreateGroup = grp
End Function

' the lab's users as local accounts in their local groups; a missing group is created and
' passwords never expire
Sub CreateLocalUsers(cfg)
  Dim users, i, u, name, pw, comp, usr, groups, j, g, grp, existing, machine, glist
  machine = gNet.ComputerName
  users = JArr(cfg, "users")
  Set comp = GetObject("WinNT://" & machine & ",computer")
  For i = 0 To UBound(users)
    Set u = users(i)
    name = JStr(u, "name")
    pw = JStr(u, "password")
    gStage = "creating the local user " & name
    existing = False
    On Error Resume Next
    Set usr = GetObject("WinNT://" & machine & "/" & name & ",user")
    If Err.Number = 0 Then existing = True
    Err.Clear
    On Error GoTo 0
    If Not existing Then Set usr = comp.Create("user", name)
    usr.SetPassword pw
    usr.SetInfo
    ' ADS_UF_DONT_EXPIRE_PASSWD
    On Error Resume Next
    usr.Put "UserFlags", (usr.Get("UserFlags") Or &H10000)
    usr.SetInfo
    If Err.Number <> 0 Then LabPhase "user " & name & ": could not set 'password never expires': " & Err.Description
    Err.Clear
    On Error GoTo 0
    groups = JArr(u, "groups")
    glist = ""
    For j = 0 To UBound(groups)
      g = CStr(groups(j))
      gStage = "adding " & name & " to the local group " & g
      Set grp = GetOrCreateGroup(comp, machine, g)
      On Error Resume Next
      grp.Add usr.ADsPath
      Err.Clear
      On Error GoTo 0
      glist = glist & ", " & g
    Next
    If glist <> "" Then glist = " in " & Mid(glist, 3)
    LabPhase "local user " & name & glist
  Next
End Sub

Sub RenameComputer(newName)
  Dim cs, rc
  gStage = "renaming the computer to " & newName
  For Each cs In GetObject("winmgmts:\\.\root\cimv2").ExecQuery("Select * From Win32_ComputerSystem")
    rc = cs.Rename(newName)
    If rc <> 0 Then Fail "renaming the computer to " & newName & " failed (Win32_ComputerSystem.Rename returned " & rc & ")"
  Next
End Sub

Function InvokeLabClient(cfg, labDir)
  Dim phase, user, pass, want, who, cs, wg
  If JStr(cfg, "mode") <> "workgroup" Then Fail "Windows Server 2003 machines run standalone (workgroup) labs only; this lab is an AD lab"
  If JBool(cfg, "lan") Then Fail "Windows Server 2003 machines run without a lab LAN (one machine per lab)"
  phase = GetLabPhase()
  ' the account this machine logs on as: its logon user, else Administrator, on this machine
  user = JStr(cfg, "logon_user")
  pass = JStr(cfg, "logon_password")
  If user = "" Then
    user = "Administrator"
    pass = JStr(cfg, "admin_password")
  End If
  want = JStr(cfg, "machine")
  If want = "" Then want = gNet.ComputerName

  If phase = "new" Then
    If UBound(JArr(cfg, "roles")) >= 0 Or UBound(JArr(cfg, "features")) >= 0 Or UBound(JArr(cfg, "setup")) >= 0 Then
      Fail "Windows Server 2003 machines have no roles, features or setup scripts yet"
    End If
    EnsureFolder gWtlDir & "\run"
    EnsureFolder gWtlDir & "\svc"
    gStage = "making the machine shut down cleanly"
    SetShutdownFriendly
    gStage = "granting the Users group access to C:\WTL"
    GrantUsersModify gWtlDir
    RegisterSvc labDir
    SetLocalAdmin cfg
    CreateLocalUsers cfg
    If LCase(gNet.ComputerName) <> LCase(want) Then
      LabPhase "renaming this computer " & gNet.ComputerName & " -> " & want
      RenameComputer want
    End If
    gStage = "setting the automatic logon"
    SetLabAutoLogon want, user, pass
    SetLabPhase "joined"
    SendEvent "LAB_JOINED", Array("workgroup", want, "autologon", want & "\" & user), ""
    gSh.Run "shutdown.exe -r -t 0 -f", 0, False
    InvokeLabClient = "reboot"
    Exit Function
  End If

  who = gNet.UserDomain & "\" & gNet.UserName
  If LCase(gNet.UserDomain) <> LCase(want) Or LCase(gNet.UserName) <> LCase(user) Then
    Fail "logged on as " & who & ", expected " & want & "\" & user & " (automatic logon)"
  End If
  wg = ""
  For Each cs In GetObject("winmgmts:\\.\root\cimv2").ExecQuery("Select Workgroup, Domain From Win32_ComputerSystem")
    wg = cs.Workgroup & ""
    ' on a workgroup machine Domain holds the workgroup name when Workgroup is Null
    If wg = "" Then wg = cs.Domain & ""
  Next
  SendEvent "LAB_CLIENT_READY", Array("user", who, "workgroup", wg, "computer", gNet.ComputerName, "ip", JStr(cfg, "ip")), ""
  InvokeLabClient = "ready"
End Function

' ---------------------------------------------------------------- the SYSTEM worker (labsvc.vbs)

' Ask the worker to do something; returns its response (a Dictionary of key=value). Until it has
' answered once in this boot, a request may wait up to 900 s for the worker to start.
Function SvcCall(req, timeoutS)
  Dim id, reqPath, resPath, t0, told, res
  timeoutS = timeoutS * gSlow
  If Not gSvcAnswered Then
    If timeoutS < 900 * gSlow Then timeoutS = 900 * gSlow
  End If
  EnsureFolder gWtlDir & "\svc"
  id = gFso.GetBaseName(gFso.GetTempName())
  reqPath = gWtlDir & "\svc\" & id & ".req"
  resPath = gWtlDir & "\svc\" & id & ".res"
  KvWrite reqPath, req
  t0 = Timer
  told = False
  Do While Not gFso.FileExists(resPath)
    If Elapsed(t0) >= timeoutS Then Fail "the privileged worker did not answer within " & timeoutS & "s (is WTL-Svc running?)"
    If Not gSvcAnswered And Not told And Elapsed(t0) >= 30 Then
      LabPhase "waiting for the SYSTEM worker (WTL-Svc) to start"
      told = True
    End If
    WScript.Sleep 300
  Loop
  WScript.Sleep 100
  Set res = KvParse(ReadText(resPath))
  DeleteIf resPath
  gSvcAnswered = True
  Set SvcCall = res
End Function

' give each mounted disc its fixed drive letter (needs privilege, so the worker does it)
Sub SetLabMounts(mounts)
  Dim i, m, letter, req, res
  For i = 0 To UBound(mounts)
    Set m = mounts(i)
    letter = UCase(JStr(m, "letter"))
    gStage = "mounting " & letter & ":"
    Set req = NewDict()
    req("action") = "mount"
    req("letter") = letter
    Set res = SvcCall(req, 120)
    If res("ok") <> "1" Then Fail "mount " & letter & ": failed: " & res("error")
    SendEvent "LAB_MOUNT", Array("letter", res("letter"), "host", JStr(m, "host")), ""
  Next
End Sub

' ---------------------------------------------------------------- run steps

' Run one step. as_ is "system" (the SYSTEM worker) or "logon" (this machine's logged-on user,
' here in this session). Reports LAB_STEP machine= as= n= script= exit= [secs=] (after the
' step's OUTPUT lines).
Sub RunLabStep(machine, n, s, timeoutS)
  Dim as_, path, argsLine, keep, runDir, outF, errF, cwd, req, res, state, exitCode, secs, errMsg
  as_ = JStr(s, "as_")
  If as_ = "" Then as_ = JStr(s, "as")
  path = JStr(s, "path")
  argsLine = JStr(s, "args")
  keep = JBool(s, "keep_running")
  runDir = gWtlDir & "\run"
  EnsureFolder runDir
  outF = runDir & "\step" & n & ".out"
  errF = runDir & "\step" & n & ".err"
  cwd = gFso.GetParentFolderName(path)
  gStage = "step " & n & " (" & path & ")"
  exitCode = -1
  secs = 0
  errMsg = ""
  If as_ = "user" Then
    state = "fail"
    errMsg = "running a step as another user is not supported on Windows Server 2003 yet (use system or the logon user)"
  ElseIf as_ = "logon" Then
    state = RunProgram("step" & n, path, argsLine, cwd, outF, errF, timeoutS, keep, exitCode, secs, errMsg)
  Else
    Set req = NewDict()
    req("action") = "run"
    req("path") = path
    req("args") = argsLine
    req("cwd") = cwd
    req("timeout") = CStr(timeoutS)
    req("out") = outF
    req("err") = errF
    If keep Then req("keep_running") = "1" Else req("keep_running") = "0"
    Set res = SvcCall(req, timeoutS + 60)
    If res("ok") <> "1" Then
      state = "fail"
      errMsg = res("error")
    ElseIf res("running") = "1" Then
      state = "running"
      secs = res("secs")
    ElseIf res("timed_out") = "1" Then
      state = "timeout"
      secs = res("secs")
    Else
      state = "exit"
      exitCode = CLng(res("exit"))
      secs = res("secs")
    End If
  End If
  If state = "fail" Then
    SendEvent "LAB_STEP", Array("machine", machine, "as", as_, "n", n, "script", path, "exit", "launchfail"), errMsg
    Exit Sub
  End If
  SendOutput "step" & n & "out", outF
  SendOutput "step" & n & "err", errF
  If state = "running" Then
    SendEvent "LAB_STEP", Array("machine", machine, "as", as_, "n", n, "script", path, "exit", "running", "secs", SecsText(secs)), ""
  ElseIf state = "timeout" Then
    SendEvent "LAB_STEP", Array("machine", machine, "as", as_, "n", n, "script", path, "exit", "timeout", "secs", SecsText(secs)), ""
  Else
    SendEvent "LAB_STEP", Array("machine", machine, "as", as_, "n", n, "script", path, "exit", exitCode, "secs", SecsText(secs)), ""
  End If
End Sub

Function InvokeLabSteps(cfg)
  Dim run, steps, machine, defWait, i, s, stepWait, n
  Set run = cfg.Item("run")
  machine = JStr(cfg, "machine")
  steps = JArr(run, "steps")
  defWait = JInt(run, "wait", 60)
  SetLabMounts JArr(run, "mounts")
  n = 0
  For i = 0 To UBound(steps)
    Set s = steps(i)
    n = n + 1
    stepWait = JInt(s, "wait", defWait)
    RunLabStep machine, n, s, stepWait
  Next
  SendEvent "LAB_RUN_DONE", Array("machine", machine, "steps", n), ""
  InvokeLabSteps = "done"
End Function

Function InvokeLabMulti(cfg, labDir)
  Dim r
  If JStr(cfg, "role") <> "member" Then Fail "Windows Server 2003 machines cannot have the role '" & JStr(cfg, "role") & "' yet"
  r = InvokeLabClient(cfg, labDir)
  If r <> "ready" Then
    InvokeLabMulti = r
    Exit Function
  End If
  ' the 0-state build: the finished machine is frozen; the host powers it off (ACPI)
  If JBool(cfg, "shutdown_when_ready") Then
    SendEvent "LAB_IDLE", Empty, ""
    InvokeLabMulti = "reboot"
    Exit Function
  End If
  If JHas(cfg, "run") Then
    If IsObject(cfg.Item("run")) Then
      InvokeLabMulti = InvokeLabSteps(cfg)
      Exit Function
    End If
  End If
  InvokeLabMulti = "ready"
End Function

' ================================================================ entry

Function InvokeLab(labDir)
  Dim cfg, r, msg
  r = "fail"
  On Error Resume Next
  gStage = "reading lab.json"
  Set cfg = JsonParse(ReadText(labDir & "\lab.json"))
  If Err.Number = 0 Then
    SendEvent "LAB_START", Array("role", JStr(cfg, "role"), "phase", GetLabPhase(), "user", gNet.UserDomain & "\" & gNet.UserName), ""
    If JStr(cfg, "fmt") <> "multi" Then Fail "lab.json fmt '" & JStr(cfg, "fmt") & "' is not supported (this lab.vbs reads fmt=multi; rebuild with the current lab)"
  End If
  If Err.Number = 0 Then
    If JInt(cfg, "slow", 1) > 1 Then gSlow = JInt(cfg, "slow", 1)
    r = InvokeLabMulti(cfg, labDir)
  End If
  If Err.Number <> 0 Then
    msg = Err.Description & " (0x" & Hex(Err.Number) & ") at: " & gStage
    Err.Clear
    On Error GoTo 0
    SendEvent "LAB_FAIL", Empty, msg
    InvokeLab = "fail"
  Else
    InvokeLab = r
  End If
End Function
