#include "shim.h"

/* nl_fmt.c — number parsing and line-number formatting for nl. */

int nl_num(char* s) {
  int i = 0;
  int n = 0;
  int c;
  if (s[0] == 45 || s[0] == 43) {
    i = 1;
  }
  c = s[i];
  while (c >= 48 && c <= 57) {
    n = n * 10 + (c - 48);
    i++;
    c = s[i];
  }
  return n;
}

/* fmt: 0 = ln (left, no pad), 1 = rn (right, space pad), 2 = rz (right,
   zero pad). Prints number + separator. */
void nl_print_num(int n, int w, int fmt, char* sep) {
  if (fmt == 0) {
    printf("%-*d%s", w, n, sep);
    return 0;
  }
  if (fmt == 2) {
    printf("%0*d%s", w, n, sep);
    return 0;
  }
  printf("%*d%s", w, n, sep);
  return 0;
}

int nl_seplen(char* s) {
  int n = 0;
  while (s[n] != 0) {
    n++;
  }
  return n;
}
