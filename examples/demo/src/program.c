/*
 * program.c - win-test-lab demo "program.exe": same protocol as echo-server.ps1.
 *
 * Build (on Linux with mingw-w64):
 *   x86_64-w64-mingw32-gcc -O2 -o ../program.exe program.c -lws2_32
 * then run examples/make-demo-zip.sh (it includes demo/program.exe if present).
 *
 * Protocol: on connect "HELLO wtl-demo os=<caption>"; PING->PONG; ECHO x->x;
 * ADD a b -> a+b; BYE -> BYE and close. Port 5000 (or argv[1]). Sequential clients.
 */
#define WIN32_LEAN_AND_MEAN
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void os_caption(char *out, size_t n)
{
    HKEY k;
    DWORD sz = (DWORD)n, type = 0;
    snprintf(out, n, "unknown");
    if (RegOpenKeyExA(HKEY_LOCAL_MACHINE, "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion",
                      0, KEY_READ | KEY_WOW64_64KEY, &k) == ERROR_SUCCESS) {
        if (RegQueryValueExA(k, "ProductName", NULL, &type, (LPBYTE)out, &sz) != ERROR_SUCCESS)
            snprintf(out, n, "unknown");
        out[n - 1] = 0;
        RegCloseKey(k);
    }
}

static int send_line(SOCKET s, const char *txt)
{
    char buf[4200];
    int len = snprintf(buf, sizeof buf, "%s\r\n", txt);
    if (len < 0) return -1;
    if (len > (int)sizeof buf - 1) len = (int)sizeof buf - 1;
    return send(s, buf, len, 0) == len ? 0 : -1;
}

/* returns 1 if the connection should be closed */
static int handle(SOCKET s, char *line)
{
    size_t L = strlen(line);
    while (L && (line[L - 1] == '\r' || line[L - 1] == '\n' || line[L - 1] == ' ')) line[--L] = 0;
    while (*line == ' ') line++;
    if (strcmp(line, "PING") == 0) { send_line(s, "PONG"); return 0; }
    if (strcmp(line, "BYE") == 0) { send_line(s, "BYE"); return 1; }
    if (strcmp(line, "ECHO") == 0) { send_line(s, ""); return 0; }
    if (strncmp(line, "ECHO ", 5) == 0) { send_line(s, line + 5); return 0; }
    if (strncmp(line, "ADD ", 4) == 0) {
        long long a, b; char extra;
        if (sscanf(line + 4, "%lld %lld %c", &a, &b, &extra) == 2) {
            char r[64]; snprintf(r, sizeof r, "%lld", a + b); send_line(s, r);
        } else send_line(s, "ERR bad numbers");
        return 0;
    }
    send_line(s, "ERR unknown command");
    return 0;
}

int main(int argc, char **argv)
{
    WSADATA w;
    SOCKET ls, cs;
    struct sockaddr_in addr;
    int port = argc > 1 ? atoi(argv[1]) : 5000, one = 1;
    char caption[256], hello[400];

    os_caption(caption, sizeof caption);
    snprintf(hello, sizeof hello, "HELLO wtl-demo os=%s", caption);
    if (WSAStartup(MAKEWORD(2, 2), &w) != 0) return 1;
    ls = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (ls == INVALID_SOCKET) return 1;
    setsockopt(ls, SOL_SOCKET, SO_REUSEADDR, (const char *)&one, sizeof one);
    memset(&addr, 0, sizeof addr);
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    addr.sin_port = htons((unsigned short)port);
    if (bind(ls, (struct sockaddr *)&addr, sizeof addr) != 0 || listen(ls, 5) != 0) {
        fprintf(stderr, "bind/listen failed on port %d: %d\n", port, WSAGetLastError());
        return 2;
    }
    printf("wtl-demo listening on 0.0.0.0:%d (os=%s)\n", port, caption);
    fflush(stdout);
    for (;;) {
        char buf[4096]; int used = 0, done = 0;
        cs = accept(ls, NULL, NULL);
        if (cs == INVALID_SOCKET) continue;
        send_line(cs, hello);
        while (!done) {
            int n = recv(cs, buf + used, (int)sizeof buf - 1 - used, 0);
            char *nl;
            if (n <= 0) break;
            used += n; buf[used] = 0;
            while (!done && (nl = strchr(buf, '\n')) != NULL) {
                *nl = 0;
                done = handle(cs, buf);
                used -= (int)(nl + 1 - buf);
                memmove(buf, nl + 1, (size_t)used + 1);
            }
            if (used >= (int)sizeof buf - 1) used = 0; /* overlong line: drop */
        }
        closesocket(cs);
    }
    return 0;
}
