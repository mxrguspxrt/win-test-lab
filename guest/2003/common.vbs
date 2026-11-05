' common.vbs - win-test-lab guest library for Windows Server 2003 (VBScript, no PowerShell).
'
' Windows Server 2003 ships no PowerShell, so its whole guest side is VBScript and batch files
' (Windows Script Host 5.6, present on every Server 2003). VBScript has no include statement:
' setup.vbs, agent.vbs and labsvc.vbs read this file and run it with ExecuteGlobal.
' ASCII only. Errors: a procedure that fails raises an error (Fail); the top-level procedure of
' each script runs under "On Error Resume Next", catches it and reports it.
'
' Serial protocol (SPEC.md): one text line per event on COM1:
'   WTL <EVENT> [key=value ...] [| free text]

Dim gFso, gSh, gNet, gWtlDir, gLogFile, gSerialMode, gStage, gReSafe
Set gFso = CreateObject("Scripting.FileSystemObject")
Set gSh = CreateObject("WScript.Shell")
Set gNet = CreateObject("WScript.Network")
gWtlDir = "C:\WTL"
gLogFile = "C:\WTL\agent.log"
gSerialMode = 0
gStage = ""
Set gReSafe = New RegExp
gReSafe.Pattern = "^[A-Za-z0-9._~:\\/+,@()-]+$"

' ---------------------------------------------------------------- small helpers

Sub Fail(msg)
  Err.Raise vbObjectError + 513, "win-test-lab", msg
End Sub

Function Pad2(n)
  If n < 10 Then
    Pad2 = "0" & n
  Else
    Pad2 = CStr(n)
  End If
End Function

Function Stamp()
  Dim d
  d = Now
  Stamp = Year(d) & "-" & Pad2(Month(d)) & "-" & Pad2(Day(d)) & " " & Pad2(Hour(d)) & ":" & Pad2(Minute(d)) & ":" & Pad2(Second(d))
End Function

' seconds since the Timer value T0 (Timer wraps at midnight)
Function Elapsed(t0)
  Dim t
  t = Timer - t0
  If t < 0 Then t = t + 86400
  Elapsed = t
End Function

Function NewDict()
  Dim d
  Set d = CreateObject("Scripting.Dictionary")
  d.CompareMode = 1
  Set NewDict = d
End Function

Sub EnsureFolder(path)
  If Not gFso.FolderExists(path) Then
    On Error Resume Next
    gFso.CreateFolder path
    Err.Clear
    On Error GoTo 0
  End If
End Sub

Sub DeleteIf(path)
  If gFso.FileExists(path) Then
    On Error Resume Next
    gFso.DeleteFile path, True
    Err.Clear
    On Error GoTo 0
  End If
End Sub

' the whole text of a file, or "" when it is missing or empty
Function ReadText(path)
  Dim f
  ReadText = ""
  If gFso.FileExists(path) Then
    On Error Resume Next
    Set f = gFso.OpenTextFile(path, 1, False)
    If Err.Number = 0 Then
      If Not f.AtEndOfStream Then ReadText = f.ReadAll
      f.Close
    End If
    Err.Clear
    On Error GoTo 0
  End If
End Function

Sub WriteText(path, text)
  Dim f
  Set f = gFso.CreateTextFile(path, True, False)
  f.Write text
  f.Close
End Sub

Sub AppendText(path, text)
  Dim f
  Set f = gFso.OpenTextFile(path, 8, True)
  f.Write text
  f.Close
End Sub

Sub SetRegStr(path, value)
  gSh.RegWrite path, value, "REG_SZ"
End Sub

Sub SetRegDword(path, value)
  gSh.RegWrite path, value, "REG_DWORD"
End Sub

Sub DelReg(path)
  On Error Resume Next
  gSh.RegDelete path
  Err.Clear
  On Error GoTo 0
End Sub

' ---------------------------------------------------------------- commands

' Run CMDLINE through cmd.exe, hidden, wait for it; returns its exit code and its output (ByRef).
' CMDLINE must not start with a double quote (cmd /c would strip it).
Function RunCmd(cmdline, ByRef outText)
  Dim tmp, rc
  EnsureFolder gWtlDir
  tmp = gWtlDir & "\cmd-" & CLng(Timer * 100) & ".out"
  rc = gSh.Run("cmd.exe /d /c " & cmdline & " > " & Chr(34) & tmp & Chr(34) & " 2>&1", 0, True)
  outText = ReadText(tmp)
  DeleteIf tmp
  RunCmd = rc
End Function

' RunCmd for commands whose failure is worth a log line but not an abort
Sub RunLog(cmdline)
  Dim out, rc
  rc = RunCmd(cmdline, out)
  If rc <> 0 Then SendEvent "LOG", Empty, cmdline & " exit=" & rc & " " & Trim(out)
End Sub

' how many wscript / cscript processes run a script called SCRIPTNAME (this one included)
Function CountScript(scriptName)
  Dim wmi, procs, p, n, cl
  n = 0
  Set wmi = GetObject("winmgmts:\\.\root\cimv2")
  Set procs = wmi.ExecQuery("Select CommandLine From Win32_Process Where Name = 'wscript.exe' Or Name = 'cscript.exe'")
  For Each p In procs
    cl = ""
    If Not IsNull(p.CommandLine) Then cl = LCase(p.CommandLine)
    If InStr(cl, LCase(scriptName)) > 0 Then n = n + 1
  Next
  CountScript = n
End Function

Sub KillTree(p)
  On Error Resume Next
  gSh.Run "taskkill.exe /T /F /PID " & p.ProcessID, 0, True
  Err.Clear
  On Error GoTo 0
End Sub

' ---------------------------------------------------------------- serial line + log

Sub LogFile(line)
  On Error Resume Next
  EnsureFolder gWtlDir
  AppendText gLogFile, Stamp() & " " & line & vbCrLf
  Err.Clear
  On Error GoTo 0
End Sub

' one attempt to write LINE to a serial device name; True when it worked
Function SerialTry(devName, line)
  Dim f
  SerialTry = False
  On Error Resume Next
  Set f = gFso.OpenTextFile(devName, 2, False)
  If Err.Number = 0 Then
    f.Write line & vbCrLf
    If Err.Number = 0 Then SerialTry = True
    f.Close
  End If
  Err.Clear
  On Error GoTo 0
End Function

' fallback: let cmd.exe copy a one-line file to the device (no quoting problems with "|" etc.)
Sub SerialViaCmd(line)
  Dim tmp
  tmp = gWtlDir & "\serial-line.tmp"
  WriteText tmp, line & vbCrLf
  gSh.Run "cmd.exe /d /c type " & Chr(34) & tmp & Chr(34) & " > \\.\COM1", 0, True
End Sub

' Open COM1, write one line, close (like the PowerShell guests). The way that works is found on
' the first line (\\.\COM1 via the file system object, then COM1, then cmd.exe) and then kept.
Sub WriteSerial(line)
  Dim n
  If gSerialMode = 0 Then
    If SerialTry("\\.\COM1", line) Then
      gSerialMode = 1
      Exit Sub
    End If
    If SerialTry("COM1", line) Then
      gSerialMode = 2
      Exit Sub
    End If
    gSerialMode = 3
  End If
  Select Case gSerialMode
    Case 1
      For n = 1 To 3
        If SerialTry("\\.\COM1", line) Then Exit Sub
        WScript.Sleep 200
      Next
    Case 2
      For n = 1 To 3
        If SerialTry("COM1", line) Then Exit Sub
        WScript.Sleep 200
      Next
    Case Else
      SerialViaCmd line
  End Select
End Sub

' keep [A-Za-z0-9._~:\/+,@()-] as it is, percent-escape anything else (UTF-8 bytes)
Function WtlValue(v)
  Dim s, i, c, code, out
  ' a WMI property can be Null (CStr(Null) is an error)
  If IsNull(v) Then s = "" Else s = CStr(v)
  If s = "" Or gReSafe.Test(s) Then
    WtlValue = s
    Exit Function
  End If
  out = ""
  For i = 1 To Len(s)
    c = Mid(s, i, 1)
    code = AscW(c)
    If code < 0 Then code = code + 65536
    If (code >= 48 And code <= 57) Or (code >= 65 And code <= 90) Or (code >= 97 And code <= 122) Or code = 45 Or code = 46 Or code = 95 Or code = 126 Then
      out = out & c
    ElseIf code < 128 Then
      out = out & "%" & Right("0" & Hex(code), 2)
    ElseIf code < 2048 Then
      out = out & "%" & Hex(192 + (code \ 64)) & "%" & Hex(128 + (code Mod 64))
    Else
      out = out & "%" & Hex(224 + (code \ 4096)) & "%" & Hex(128 + ((code \ 64) Mod 64)) & "%" & Hex(128 + (code Mod 64))
    End If
  Next
  WtlValue = out
End Function

' log + COM1:  SendEvent "LAB_STEP", Array("n", 1, "exit", 0), "free text"
Sub SendEvent(name, pairs, text)
  Dim line, i
  line = "WTL " & name
  If IsArray(pairs) Then
    For i = 0 To UBound(pairs) - 1 Step 2
      line = line & " " & CStr(pairs(i)) & "=" & WtlValue(pairs(i + 1))
    Next
  End If
  If text <> "" Then line = line & " | " & text
  line = Replace(Replace(line, vbCr, " "), vbLf, " ")
  LogFile line
  WriteSerial line
End Sub

Sub WtlLog(text)
  SendEvent "LOG", Empty, text
End Sub

' ---------------------------------------------------------------- JSON (VBScript has none)
' objects -> Scripting.Dictionary (text-compare keys), arrays -> Variant arrays, numbers ->
' Long / Double, true / false -> Boolean, null -> Empty.

Dim gJsonText, gJsonPos

Sub JsonSkip()
  Dim c
  Do While gJsonPos <= Len(gJsonText)
    c = Mid(gJsonText, gJsonPos, 1)
    If c = " " Or c = vbTab Or c = vbCr Or c = vbLf Then
      gJsonPos = gJsonPos + 1
    Else
      Exit Do
    End If
  Loop
End Sub

Function JsonString()
  Dim sb, c, e
  sb = ""
  gJsonPos = gJsonPos + 1
  Do
    If gJsonPos > Len(gJsonText) Then Fail "JSON: unterminated string"
    c = Mid(gJsonText, gJsonPos, 1)
    gJsonPos = gJsonPos + 1
    If c = """" Then Exit Do
    If c = "\" Then
      e = Mid(gJsonText, gJsonPos, 1)
      gJsonPos = gJsonPos + 1
      Select Case e
        Case "n"
          sb = sb & vbLf
        Case "r"
          sb = sb & vbCr
        Case "t"
          sb = sb & vbTab
        Case "b"
          sb = sb & Chr(8)
        Case "f"
          sb = sb & Chr(12)
        Case "u"
          sb = sb & ChrW(CLng("&H" & Mid(gJsonText, gJsonPos, 4)))
          gJsonPos = gJsonPos + 4
        Case Else
          sb = sb & e
      End Select
    Else
      sb = sb & c
    End If
  Loop
  JsonString = sb
End Function

Function JsonNumber()
  Dim s, c
  s = ""
  Do While gJsonPos <= Len(gJsonText)
    c = Mid(gJsonText, gJsonPos, 1)
    If InStr("0123456789+-.eE", c) > 0 Then
      s = s & c
      gJsonPos = gJsonPos + 1
    Else
      Exit Do
    End If
  Loop
  If s = "" Then Fail "JSON: unexpected character '" & Mid(gJsonText, gJsonPos, 1) & "' at offset " & gJsonPos
  If InStr(s, ".") > 0 Or InStr(LCase(s), "e") > 0 Then
    JsonNumber = CDbl(s)
  Else
    JsonNumber = CLng(s)
  End If
End Function

Sub JsonValue(ByRef out)
  Dim c, d, k, tmp, items, n, i, arr
  JsonSkip
  If gJsonPos > Len(gJsonText) Then Fail "JSON: unexpected end"
  c = Mid(gJsonText, gJsonPos, 1)
  If c = "{" Then
    Set d = NewDict()
    gJsonPos = gJsonPos + 1
    JsonSkip
    If Mid(gJsonText, gJsonPos, 1) = "}" Then
      gJsonPos = gJsonPos + 1
    Else
      Do
        JsonSkip
        If Mid(gJsonText, gJsonPos, 1) <> """" Then Fail "JSON: expected a key at offset " & gJsonPos
        k = JsonString()
        JsonSkip
        If Mid(gJsonText, gJsonPos, 1) <> ":" Then Fail "JSON: expected : at offset " & gJsonPos
        gJsonPos = gJsonPos + 1
        tmp = Empty
        JsonValue tmp
        If d.Exists(k) Then d.Remove k
        d.Add k, tmp
        JsonSkip
        c = Mid(gJsonText, gJsonPos, 1)
        gJsonPos = gJsonPos + 1
        If c = "}" Then Exit Do
        If c <> "," Then Fail "JSON: expected , or } at offset " & (gJsonPos - 1)
      Loop
    End If
    Set out = d
  ElseIf c = "[" Then
    Set items = NewDict()
    n = 0
    gJsonPos = gJsonPos + 1
    JsonSkip
    If Mid(gJsonText, gJsonPos, 1) = "]" Then
      gJsonPos = gJsonPos + 1
    Else
      Do
        tmp = Empty
        JsonValue tmp
        items.Add CStr(n), tmp
        n = n + 1
        JsonSkip
        c = Mid(gJsonText, gJsonPos, 1)
        gJsonPos = gJsonPos + 1
        If c = "]" Then Exit Do
        If c <> "," Then Fail "JSON: expected , or ] at offset " & (gJsonPos - 1)
      Loop
    End If
    If n = 0 Then
      arr = Array()
    Else
      ReDim arr(n - 1)
      For i = 0 To n - 1
        If IsObject(items(CStr(i))) Then
          Set arr(i) = items(CStr(i))
        Else
          arr(i) = items(CStr(i))
        End If
      Next
    End If
    out = arr
  ElseIf c = """" Then
    out = JsonString()
  ElseIf Mid(gJsonText, gJsonPos, 4) = "true" Then
    gJsonPos = gJsonPos + 4
    out = True
  ElseIf Mid(gJsonText, gJsonPos, 5) = "false" Then
    gJsonPos = gJsonPos + 5
    out = False
  ElseIf Mid(gJsonText, gJsonPos, 4) = "null" Then
    gJsonPos = gJsonPos + 4
    out = Empty
  Else
    out = JsonNumber()
  End If
End Sub

' parse TEXT; returns a Dictionary (or an array / scalar for a non-object document)
Function JsonParse(text)
  Dim v
  gJsonText = text
  gJsonPos = 1
  If Len(gJsonText) > 0 Then
    If AscW(Left(gJsonText, 1)) = 65279 Then gJsonPos = 2
  End If
  v = Empty
  JsonValue v
  JsonSkip
  If gJsonPos <= Len(gJsonText) Then Fail "JSON: unexpected text at offset " & gJsonPos
  If IsObject(v) Then
    Set JsonParse = v
  Else
    JsonParse = v
  End If
End Function

Function JHas(obj, key)
  JHas = False
  If IsObject(obj) Then
    If TypeName(obj) = "Dictionary" Then JHas = obj.Exists(key)
  End If
End Function

' a value as text ("" when absent, an object or an array)
Function JStr(obj, key)
  JStr = ""
  If JHas(obj, key) Then
    If Not IsObject(obj.Item(key)) And Not IsArray(obj.Item(key)) Then
      If Not IsNull(obj.Item(key)) Then JStr = CStr(obj.Item(key))
    End If
  End If
End Function

Function JInt(obj, key, dflt)
  Dim s
  JInt = dflt
  s = JStr(obj, key)
  If s <> "" Then
    If IsNumeric(s) Then JInt = CLng(s)
  End If
End Function

Function JBool(obj, key)
  Dim s
  s = LCase(JStr(obj, key))
  JBool = (s = "true" Or s = "1" Or s = "yes")
End Function

' an array value (an empty array when absent)
Function JArr(obj, key)
  If JHas(obj, key) Then
    If IsArray(obj.Item(key)) Then
      JArr = obj.Item(key)
      Exit Function
    End If
  End If
  JArr = Array()
End Function

' ---------------------------------------------------------------- key=value files
' The request / response files between the agent and the SYSTEM worker are plain key=value lines.

Function KvParse(text)
  Dim d, lines, i, ln, p
  Set d = NewDict()
  lines = Split(Replace(text, vbCr, ""), vbLf)
  For i = 0 To UBound(lines)
    ln = lines(i)
    p = InStr(ln, "=")
    If p > 1 Then d(Left(ln, p - 1)) = Mid(ln, p + 1)
  Next
  Set KvParse = d
End Function

' written under another name and renamed, so a reader never sees half a file
Sub KvWrite(path, d)
  Dim k, s
  s = ""
  For Each k In d.Keys
    s = s & k & "=" & Replace(Replace(CStr(d(k)), vbCr, " "), vbLf, " ") & vbCrLf
  Next
  WriteText path & ".tmp", s
  DeleteIf path
  gFso.MoveFile path & ".tmp", path
End Sub

' ---------------------------------------------------------------- program output over COM1

' Send the file PATH as base64 chunks:  WTL OUTPUT stream=step1out seq=N | <base64>
Sub SendOutput(streamName, path)
  Dim total, sent, seq, want, st, doc, el, bytes, b64, limit, truncated
  limit = 262144
  total = 0
  sent = 0
  seq = 0
  If gFso.FileExists(path) Then total = gFso.GetFile(path).Size
  If total > 0 Then
    Set st = CreateObject("ADODB.Stream")
    st.Type = 1
    st.Open
    st.LoadFromFile path
    Set doc = CreateObject("MSXML2.DOMDocument")
    Set el = doc.createElement("b")
    el.dataType = "bin.base64"
    Do While sent < limit And Not st.EOS
      want = 768
      If limit - sent < want Then want = limit - sent
      bytes = st.Read(want)
      el.nodeTypedValue = bytes
      b64 = Replace(Replace(el.Text, vbCr, ""), vbLf, "")
      SendEvent "OUTPUT", Array("stream", streamName, "seq", seq), b64
      sent = sent + UBound(bytes) + 1
      seq = seq + 1
    Loop
    st.Close
  End If
  truncated = 0
  If total > sent Then truncated = 1
  SendEvent "OUTPUT_END", Array("stream", streamName, "bytes", total, "sent", sent, "truncated", truncated), ""
End Sub

' ---------------------------------------------------------------- running a step

' the command text that runs PATH with ARGS inside a batch file
Function LaunchText(path, args)
  Dim q, a
  q = Chr(34)
  a = ""
  If args <> "" Then a = " " & args
  Select Case LCase(gFso.GetExtensionName(path))
    Case "bat", "cmd"
      LaunchText = "call " & q & path & q & a
    Case "vbs", "vbe", "js"
      LaunchText = "cscript.exe //nologo " & q & path & q & a
    Case "msi"
      LaunchText = "msiexec.exe /i " & q & path & q & " /qn /norestart" & a
    Case Else
      LaunchText = q & path & q & a
  End Select
End Function

' Run PATH in a wrapper batch file that sends its stdout / stderr to OUTF / ERRF and its exit
' code to a file, and wait for it up to TIMEOUTS seconds. Returns "exit", "timeout" (killed),
' "running" (KEEP: left running past the deadline) or "fail" (ERRMSG says why).
Function RunProgram(tag, path, args, cwd, outF, errF, timeoutS, keep, ByRef exitCode, ByRef secs, ByRef errMsg)
  Dim q, wrap, rcF, txt, p, t0, state, rcText
  q = Chr(34)
  EnsureFolder gWtlDir & "\run"
  wrap = gWtlDir & "\run\" & tag & ".cmd"
  rcF = gWtlDir & "\run\" & tag & ".rc"
  DeleteIf outF
  DeleteIf errF
  DeleteIf rcF
  txt = "@echo off" & vbCrLf
  ' a SYSTEM task on Server 2003 has no USERNAME / USERDOMAIN; newer Windows gives it COMPUTER$ / WORKGROUP
  txt = txt & "if " & q & "%USERNAME%" & q & "==" & q & q & " set USERNAME=%COMPUTERNAME%$" & vbCrLf
  txt = txt & "if " & q & "%USERDOMAIN%" & q & "==" & q & q & " set USERDOMAIN=WORKGROUP" & vbCrLf
  If cwd <> "" Then
    If gFso.FolderExists(cwd) Then txt = txt & "cd /d " & q & cwd & q & vbCrLf
  End If
  txt = txt & LaunchText(path, args) & " > " & q & outF & q & " 2> " & q & errF & q & " < nul" & vbCrLf
  ' (a space before ">": "echo 0> file" would redirect handle 0 and write nothing)
  txt = txt & "echo %errorlevel% > " & q & rcF & ".tmp" & q & vbCrLf
  txt = txt & "move /y " & q & rcF & ".tmp" & q & " " & q & rcF & q & " > nul" & vbCrLf
  WriteText wrap, txt
  exitCode = -1
  secs = 0
  errMsg = ""
  t0 = Timer
  On Error Resume Next
  Set p = gSh.Exec("cmd.exe /d /c " & wrap)
  If Err.Number <> 0 Then
    errMsg = Err.Description
    Err.Clear
    On Error GoTo 0
    RunProgram = "fail"
    Exit Function
  End If
  On Error GoTo 0
  state = ""
  Do
    If gFso.FileExists(rcF) Then
      state = "exit"
      Exit Do
    End If
    If p.Status <> 0 Then
      WScript.Sleep 300
      If gFso.FileExists(rcF) Then
        state = "exit"
      Else
        state = "fail"
        errMsg = "the step's command shell ended without reporting an exit code"
      End If
      Exit Do
    End If
    If Elapsed(t0) >= timeoutS Then
      state = "timeout"
      Exit Do
    End If
    WScript.Sleep 250
  Loop
  secs = Round(Elapsed(t0), 1)
  If state = "exit" Then
    rcText = Trim(ReadText(rcF))
    If IsNumeric(rcText) Then exitCode = CLng(rcText)
    RunProgram = "exit"
  ElseIf state = "timeout" Then
    If keep Then
      RunProgram = "running"
    Else
      KillTree p
      WScript.Sleep 1000
      RunProgram = "timeout"
    End If
  Else
    RunProgram = "fail"
  End If
End Function

' seconds as text with a dot, whatever the locale
Function SecsText(s)
  SecsText = Replace(CStr(s), ",", ".")
End Function
