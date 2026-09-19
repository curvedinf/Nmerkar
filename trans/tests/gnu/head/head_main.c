#include "shim.h"

/* head_main.c — first N lines (-n, default 10) or N bytes (-c) of each
   input. -q suppresses the ==> name <== headers, -v forces them (default:
   headers only when more than one input). Negative counts (all-but-last)
   are not supported — head would need to buffer. */

int head_read(int f) {
  if (f == 0) {
    return getchar();
  }
  return fgetc(f);
}

int head_stream(int f, int n, int bytes) {
  int c;
  int lines_done = 0;
  int bytes_done = 0;
  while (1) {
    if (bytes != 0) {
      if (bytes_done >= n) {
        return 0;
      }
    } else {
      if (lines_done >= n) {
        return 0;
      }
    }
    c = head_read(f);
    if (c == EOF) {
      return 0;
    }
    putchar(c);
    bytes_done++;
    if (c == 10) {
      lines_done++;
    }
  }
  return 0;
}

int main() {
  int n = 10;
  int bytes = 0;
  int quiet = 0;
  int always = 0;
  int no_more_opts = 0;
  int i = 1;
  int inputs = 0;
  int printed = 0;
  char* s;
  int f;
  while (i < argc) {
    s = argv[i];
    if (no_more_opts == 0 && s[0] == 45 && s[1] != 0) {
      if (s[1] == 110 || s[1] == 99) {
        if (s[1] == 99) {
          bytes = 1;
        }
        if (s[2] != 0) {
          n = head_num(s + 2);
        } else {
          i++;
          n = head_num(argv[i]);
        }
        i++;
        continue;
      }
      if (s[1] == 113) {
        quiet = 1;
        i++;
        continue;
      }
      if (s[1] == 118) {
        always = 1;
        i++;
        continue;
      }
      if (s[1] == 45 && s[2] == 0) {
        no_more_opts = 1;
        i++;
        continue;
      }
      if (s[1] >= 48 && s[1] <= 57) {
        n = head_num(s + 1);
        i++;
        continue;
      }
      fprintf(stderr, "head: invalid option -- %s\n", s + 1);
      return 1;
    }
    inputs++;
    i++;
  }
  i = 1;
  no_more_opts = 0;
  while (i < argc) {
    s = argv[i];
    if (no_more_opts == 0 && s[0] == 45 && s[1] != 0) {
      if (s[1] == 45 && s[2] == 0) {
        no_more_opts = 1;
        i++;
        continue;
      }
      if (s[1] == 110 || s[1] == 99) {
        if (s[2] == 0) {
          i++;
        }
      }
      i++;
      continue;
    }
    if (quiet == 0 && (always != 0 || inputs > 1)) {
      if (printed != 0) {
        putchar(10);
      }
      printf("==> %s <==\n", s);
    }
    printed = 1;
    if (s[0] == 45 && s[1] == 0) {
      head_stream(0, n, bytes);
    } else {
      f = fopen(s, "r");
      if (!f) {
        fprintf(stderr, "head: cannot open %s\n", s);
        return 1;
      }
      head_stream(f, n, bytes);
      fclose(f);
    }
    i++;
  }
  if (inputs == 0) {
    if (quiet == 0 && always != 0) {
      printf("==> standard input <==\n");
    }
    head_stream(0, n, bytes);
  }
  return 0;
}
