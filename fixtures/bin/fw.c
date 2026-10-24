/* The binary idax and bnx test their skill and ref examples against
 * (`fw.bin`), built for AArch64 (the architecture every example assumes).
 * Names a script may look up: functions `main`,
 * `check`, `banner`; data `tries`, `greeting`; imports `puts`, `strcmp`,
 * `malloc`, `free`. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int tries;
const char *greeting = "welcome";

__attribute__((noinline)) int check(const char *s) {
    tries++;
    return strcmp(s, "s3cret") == 0;
}

__attribute__((noinline)) void banner(void) {
    char *line = malloc(32);
    if (line != NULL) {
        strcpy(line, greeting);
        puts(line);
        free(line);
    }
}

int main(int argc, char **argv) {
    banner();
    puts(argc > 1 && check(argv[1]) ? "access granted" : "access denied");
    return 0;
}
