#include <stdio.h>
#include <string.h>

int main(void) {
    char buffer[64];
    /* strlcpy was introduced in glibc 2.38; this must fail on parent glibc 2.35. */
    if (strlcpy(buffer, "glibc-2.38-symbol-ok", sizeof buffer) >= sizeof buffer)
        return 1;
    puts(buffer);
    return 0;
}
