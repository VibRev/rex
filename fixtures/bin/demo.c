#include <string.h>
#include <stdio.h>
static int check(const char *s) { return strcmp(s, "s3cret") == 0; }
int main(int c, char **v) { puts(c > 1 && check(v[1]) ? "ok" : "no"); return 0; }
