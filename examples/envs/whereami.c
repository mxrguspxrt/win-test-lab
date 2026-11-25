/*
 * whereami.c - inside a lab machine: which lab, environment and machine is this? Checks that
 * the computer name and the step's variables agree, and (optionally) that they are what you
 * expect.
 *
 *   whereami.exe [--env NAME] [--id ID] [--machine KEY] [--hostname NAME] [--vars]
 *
 * Reads, and prints:
 *   - the computer name (the machine's hostname: its lab key, or the new name of a renamed
 *     environment's machine)
 *   - with --vars: the step's WTL_ENVIRONMENT, WTL_ENVIRONMENT_ID, WTL_MACHINE, WTL_HOSTNAME,
 *     WTL_RUN_ID
 * and checks that the computer name is --hostname, and with --vars that every variable is set,
 * that the computer name is WTL_HOSTNAME, that WTL_RUN_ID ends in the id and that environment /
 * id / machine are --env / --id / --machine (an empty value skips that check). Ends with
 * "RESULT OK" (exit 0) or "RESULT FAIL: N problem(s)" (exit 1).
 *
 * Build: examples/envs/build.sh (gcc-mingw-w64-x86-64; gcc-mingw-w64-i686 for Server 2003).
 * One .exe, no DLLs, no data files.
 */
#include <windows.h>
#include <stdio.h>
#include <string.h>

#define VAL 512

static int problems = 0;

static void problem(const char *what, const char *got, const char *want)
{
    printf("  PROBLEM: %s is \"%s\", expected \"%s\"\n", what, got, want);
    problems++;
}

static void same(const char *what, const char *got, const char *want)
{
    if (strcmp(got, want) != 0)
        problem(what, got, want);
}

/* a step variable; a missing one is a problem */
static void var(const char *name, char *out)
{
    out[0] = 0;
    GetEnvironmentVariableA(name, out, VAL);
    if (!out[0]) {
        printf("  PROBLEM: %s is not set\n", name);
        problems++;
    }
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
    if (want_host[0] && _stricmp(host, want_host) != 0)
        problem("the computer name", host, want_host);

    /* the step's own variables */
    char env[VAL] = "", id[VAL] = "", machine[VAL] = "", hostname[VAL] = "", run_id[VAL] = "";
    if (vars) {
        var("WTL_ENVIRONMENT", env);
        var("WTL_ENVIRONMENT_ID", id);
        var("WTL_MACHINE", machine);
        var("WTL_HOSTNAME", hostname);
        var("WTL_RUN_ID", run_id);
        printf("step variables  WTL_ENVIRONMENT=%s WTL_ENVIRONMENT_ID=%s WTL_MACHINE=%s WTL_HOSTNAME=%s WTL_RUN_ID=%s\n", env, id, machine,
               hostname, run_id);
        /* the computer name is the machine's hostname (renamed for real in a renamed environment) */
        if (hostname[0] && _stricmp(host, hostname) != 0)
            problem("the computer name", host, hostname);
        /* the run id is <time>-<id> */
        size_t rl = strlen(run_id), il = strlen(id);
        if (run_id[0] && id[0] && (rl <= il || run_id[rl - il - 1] != '-' || strcmp(run_id + rl - il, id) != 0))
            problem("WTL_RUN_ID's end", run_id, id);

        /* what the caller expects */
        if (want_env[0])
            same("the environment", env, want_env);
        if (want_id[0])
            same("the id", id, want_id);
        if (want_machine[0] && _stricmp(machine, want_machine) != 0)
            problem("the machine", machine, want_machine);
    }

    if (problems) {
        printf("RESULT FAIL: %d problem(s)\n", problems);
        return 1;
    }
    if (vars)
        printf("RESULT OK: this is %s (%s) of %s (id %s)\n", machine, host, env, id);
    else
        printf("RESULT OK: this is %s (no step variables to check)\n", host);
    return 0;
}
