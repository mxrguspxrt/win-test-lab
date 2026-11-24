/*
 * whereami.c - inside a lab machine: which lab, environment and machine is this? Checks that
 * every place wtl records it agrees, and (optionally) that it is what you expect.
 *
 *   whereami.exe [--env NAME] [--id ID] [--machine KEY] [--hostname NAME] [--vars]
 *
 * Reads, and prints:
 *   - the computer name (the machine's hostname: its lab key, or the new name of a renamed
 *     environment's machine)
 *   - C:\WTL\instance.json                       (lab, environment, id, machine, hostname, note,
 *                                                 run_id)
 *   - HKLM\SOFTWARE\WinTestLab                   (the same values; on 64-bit Windows both the
 *                                                 64-bit and the 32-bit registry view)
 *   - with --vars: the step's WTL_ENVIRONMENT, WTL_ENVIRONMENT_ID, WTL_MACHINE, WTL_HOSTNAME,
 *     WTL_RUN_ID
 * and checks that they all say the same, that the computer name is the hostname they name, and
 * that environment / id / machine / hostname are --env / --id / --machine / --hostname when
 * given (an empty value skips that check). Ends with "RESULT OK" (exit 0) or "RESULT FAIL: N problem(s)" (exit 1).
 *
 * Build: examples/envs/build.sh (gcc-mingw-w64-x86-64; gcc-mingw-w64-i686 for Server 2003).
 * One .exe, no DLLs, no data files.
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

#ifndef KEY_WOW64_64KEY
#define KEY_WOW64_64KEY 0x0100
#endif
#ifndef KEY_WOW64_32KEY
#define KEY_WOW64_32KEY 0x0200
#endif

#define VAL 512

static int problems = 0;

static void problem(const char *what, const char *got, const char *want)
{
    printf("  PROBLEM: %s is \"%s\", expected \"%s\"\n", what, got, want);
    problems++;
}

/* the string value of "key" in the (flat, wtl-written) JSON text; "" when absent */
static void json_get(const char *json, const char *key, char *out)
{
    char pat[64];
    const char *p;
    int n = 0;
    out[0] = 0;
    snprintf(pat, sizeof pat, "\"%s\": \"", key);
    p = strstr(json, pat);
    if (!p)
        return;
    for (p += strlen(pat); *p && *p != '"' && n < VAL - 1; p++) {
        if (*p == '\\' && p[1]) {
            p++;
            if (*p == 'u' && p[1] && p[2] && p[3] && p[4]) {
                out[n++] = '?'; /* non-ASCII: not compared, only shown */
                p += 4;
                continue;
            }
        }
        out[n++] = *p;
    }
    out[n] = 0;
}

/* a REG_SZ value of HKLM\SOFTWARE\WinTestLab in one registry view; 0 if the key is missing */
static int reg_get(REGSAM view, const char *name, char *out)
{
    HKEY k;
    DWORD type = 0, size = VAL - 1;
    out[0] = 0;
    if (RegOpenKeyExA(HKEY_LOCAL_MACHINE, "SOFTWARE\\WinTestLab", 0, KEY_READ | view, &k) != ERROR_SUCCESS)
        return 0;
    if (RegQueryValueExA(k, name, NULL, &type, (BYTE *)out, &size) != ERROR_SUCCESS || type != REG_SZ)
        out[0] = 0;
    else
        out[size < VAL ? size : VAL - 1] = 0;
    RegCloseKey(k);
    return 1;
}

static int is_64bit_windows(void)
{
#ifdef _WIN64
    return 1;
#else
    typedef BOOL(WINAPI * fn)(HANDLE, PBOOL);
    BOOL wow = FALSE;
    fn f = (fn)GetProcAddress(GetModuleHandleA("kernel32"), "IsWow64Process");
    return f && f(GetCurrentProcess(), &wow) && wow;
#endif
}

static void same(const char *what, const char *got, const char *want)
{
    if (strcmp(got, want) != 0)
        problem(what, got, want);
}

int main(int argc, char **argv)
{
    const char *want_env = "", *want_id = "", *want_machine = "", *want_host = "";
    int vars = 0, i;
    for (i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--env") && i + 1 < argc)
            want_env = argv[++i];
        else if (!strcmp(argv[i], "--id") && i + 1 < argc)
            want_id = argv[++i];
        else if (!strcmp(argv[i], "--machine") && i + 1 < argc)
            want_machine = argv[++i];
        else if (!strcmp(argv[i], "--hostname") && i + 1 < argc)
            want_host = argv[++i];
        else if (!strcmp(argv[i], "--vars"))
            vars = 1;
        else {
            printf("usage: whereami.exe [--env NAME] [--id ID] [--machine KEY] [--hostname NAME] [--vars]\n");
            return 2;
        }
    }

    char host[MAX_COMPUTERNAME_LENGTH + 1] = "";
    DWORD hn = sizeof host;
    GetComputerNameA(host, &hn);
    printf("computer name   %s\n", host);

    /* C:\WTL\instance.json */
    char json[8192] = "";
    FILE *f = fopen("C:\\WTL\\instance.json", "rb");
    if (f) {
        size_t n = fread(json, 1, sizeof json - 1, f);
        json[n] = 0;
        fclose(f);
    } else {
        printf("  PROBLEM: C:\\WTL\\instance.json is missing\n");
        problems++;
    }
    char lab[VAL], env[VAL], id[VAL], machine[VAL], hostname[VAL], note[VAL], run_id[VAL];
    json_get(json, "lab", lab);
    json_get(json, "environment", env);
    json_get(json, "id", id);
    json_get(json, "machine", machine);
    json_get(json, "hostname", hostname);
    if (!hostname[0]) /* from wtl 0.4: the computer name was the machine key */
        strcpy(hostname, machine);
    json_get(json, "note", note);
    json_get(json, "run_id", run_id);
    printf("instance.json   lab=%s environment=%s id=%s machine=%s hostname=%s run_id=%s\n", lab, env, id, machine, hostname, run_id);
    if (note[0])
        printf("                note: %s\n", note);
    if (!env[0] || !id[0] || !machine[0]) {
        printf("  PROBLEM: instance.json lacks environment, id or machine\n");
        problems++;
    }

    /* the computer name is the machine's hostname (renamed for real in a renamed environment) */
    if (_stricmp(host, hostname) != 0)
        problem("the computer name", host, hostname);

    /* the registry: the 64-bit view, and the 32-bit one on 64-bit Windows */
    REGSAM views[2] = {KEY_WOW64_64KEY, KEY_WOW64_32KEY};
    const char *view_names[2] = {"64-bit", "32-bit"};
    int nviews = is_64bit_windows() ? 2 : 1;
    for (i = 0; i < nviews; i++) {
        char r_env[VAL], r_id[VAL], r_machine[VAL], r_host[VAL], r_run[VAL], what[64];
        const char *vn = nviews == 1 ? "" : view_names[i];
        if (!reg_get(views[i], "Environment", r_env)) {
            printf("  PROBLEM: HKLM\\SOFTWARE\\WinTestLab is missing (%s view)\n", nviews == 1 ? "the" : vn);
            problems++;
            continue;
        }
        reg_get(views[i], "Id", r_id);
        reg_get(views[i], "Machine", r_machine);
        reg_get(views[i], "Hostname", r_host);
        if (!r_host[0])
            strcpy(r_host, r_machine);
        reg_get(views[i], "RunId", r_run);
        printf("registry %-6s  environment=%s id=%s machine=%s hostname=%s run_id=%s\n", vn, r_env, r_id, r_machine, r_host, r_run);
        snprintf(what, sizeof what, "registry %sEnvironment", nviews == 1 ? "" : (i ? "(32-bit) " : "(64-bit) "));
        same(what, r_env, env);
        snprintf(what, sizeof what, "registry %sId", nviews == 1 ? "" : (i ? "(32-bit) " : "(64-bit) "));
        same(what, r_id, id);
        snprintf(what, sizeof what, "registry %sMachine", nviews == 1 ? "" : (i ? "(32-bit) " : "(64-bit) "));
        same(what, r_machine, machine);
        snprintf(what, sizeof what, "registry %sHostname", nviews == 1 ? "" : (i ? "(32-bit) " : "(64-bit) "));
        same(what, r_host, hostname);
        snprintf(what, sizeof what, "registry %sRunId", nviews == 1 ? "" : (i ? "(32-bit) " : "(64-bit) "));
        same(what, r_run, run_id);
    }

    /* the step's own variables */
    if (vars) {
        char v_env[VAL] = "", v_id[VAL] = "", v_machine[VAL] = "", v_host[VAL] = "", v_run[VAL] = "";
        GetEnvironmentVariableA("WTL_ENVIRONMENT", v_env, VAL);
        GetEnvironmentVariableA("WTL_ENVIRONMENT_ID", v_id, VAL);
        GetEnvironmentVariableA("WTL_MACHINE", v_machine, VAL);
        GetEnvironmentVariableA("WTL_HOSTNAME", v_host, VAL);
        GetEnvironmentVariableA("WTL_RUN_ID", v_run, VAL);
        printf("step variables  WTL_ENVIRONMENT=%s WTL_ENVIRONMENT_ID=%s WTL_MACHINE=%s WTL_HOSTNAME=%s WTL_RUN_ID=%s\n", v_env, v_id,
               v_machine, v_host, v_run);
        same("WTL_ENVIRONMENT", v_env, env);
        same("WTL_ENVIRONMENT_ID", v_id, id);
        same("WTL_MACHINE", v_machine, machine);
        same("WTL_HOSTNAME", v_host, hostname);
        same("WTL_RUN_ID", v_run, run_id);
    }

    /* what the caller expects */
    if (want_env[0])
        same("the environment", env, want_env);
    if (want_id[0])
        same("the id", id, want_id);
    if (want_machine[0] && _stricmp(machine, want_machine) != 0)
        problem("the machine", machine, want_machine);
    if (want_host[0] && _stricmp(hostname, want_host) != 0)
        problem("the hostname", hostname, want_host);

    if (problems) {
        printf("RESULT FAIL: %d problem(s)\n", problems);
        return 1;
    }
    printf("RESULT OK: this is %s (%s) of %s (id %s)\n", machine, host, env, id);
    return 0;
}
