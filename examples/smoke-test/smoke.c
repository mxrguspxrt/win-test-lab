/*
 * smoke.c - a tiny, self-contained test program for `lab run`.
 * Build: examples/smoke-test/build.sh  (needs gcc-mingw-w64-x86-64)
 * No DLLs, no data files - just one .exe. It prints who it runs as and the Windows version,
 * then exits 0, so a run that reaches "RESULT OK" / exit 0 proves the lab launched the exe.
 */
#include <windows.h>
#include <stdio.h>

int main(void)
{
    char user[256] = "?", dom[256] = "?";
    DWORD n = sizeof user;
    GetUserNameA(user, &n);
    GetEnvironmentVariableA("USERDOMAIN", dom, sizeof dom);

    OSVERSIONINFOA v;
    ZeroMemory(&v, sizeof v);
    v.dwOSVersionInfoSize = sizeof v;
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
    GetVersionExA(&v);
#pragma GCC diagnostic pop

    printf("smoke.exe running as %s\\%s\n", dom, user);
    printf("Windows version %lu.%lu build %lu\n",
           (unsigned long)v.dwMajorVersion, (unsigned long)v.dwMinorVersion,
           (unsigned long)v.dwBuildNumber);
    printf("RESULT OK\n");
    return 0;
}
