#include "shim.h"

/* tee_main.c — copy stdin to stdout and to each FILE. -a append, -i
   (ignored, no signals in the subset), -- ends options. */

int main() {
  int append = 0;
  int i = 1;
  int no_more_opts = 0;
  int from = 1;
  int to = 0;
  int n;
  int chunk = 0;
  char* buf;
  char* s;
  while (i < argc) {
    s = argv[i];
    if (no_more_opts == 0 && s[0] == 45 && s[1] != 0) {
      if (s[1] == 45 && s[2] == 0) {
        no_more_opts = 1;
        i++;
        continue;
      }
      if (s[1] == 97) {
        append = 1;
        i++;
        continue;
      }
      if (s[1] == 105) {
        i++;
        continue;
      }
      fprintf(stderr, "tee: invalid option -- %s\n", s + 1);
      return 1;
    }
    if (to < from) {
      from = i;
    }
    to = i;
    i++;
  }
  buf = malloc(65536);
  while (1) {
    n = fread(buf, 1, 65536, stdin);
    if (n <= 0) {
      break;
    }
    fwrite(buf, 1, n, stdout);
    if (to >= from) {
      if (tee_write_files(from, to, buf, n, append != 0 || chunk != 0) != 0) {
        return 1;
      }
      chunk = 1;
    }
  }
  return 0;
}
