#include "shim.h"

/* nl_main.c — number lines of input. -b a|t|n body numbering (default t),
   -v start value (1), -i increment (1), -w width (6), -s separator (tab),
   -n ln|rn|rz format (default rn). */

int main() {
  int start = 1;
  int incr = 1;
  int w = 6;
  int fmt = 1;
  int body = 1;
  int lineno = 1;
  int at_line_start = 1;
  char* sep = "\t";
  int i = 1;
  char* s;
  int c;
  int f = 0;
  while (i < argc) {
    s = argv[i];
    if (s[0] == 45 && s[1] == 118) {
      if (s[2] != 0) {
        start = nl_num(s + 2);
        i++;
        continue;
      }
      i++;
      start = nl_num(argv[i]);
      i++;
      continue;
    }
    if (s[0] == 45 && s[1] == 105) {
      if (s[2] != 0) {
        incr = nl_num(s + 2);
        i++;
        continue;
      }
      i++;
      incr = nl_num(argv[i]);
      i++;
      continue;
    }
    if (s[0] == 45 && s[1] == 119) {
      if (s[2] != 0) {
        w = nl_num(s + 2);
        i++;
        continue;
      }
      i++;
      w = nl_num(argv[i]);
      i++;
      continue;
    }
    if (s[0] == 45 && s[1] == 115) {
      if (s[2] != 0) {
        sep = s + 2;
        i++;
        continue;
      }
      i++;
      sep = argv[i];
      i++;
      continue;
    }
    if (s[0] == 45 && s[1] == 98 && s[2] == 0) {
      i++;
      s = argv[i];
      if (s[0] == 97) {
        body = 0;
        i++;
        continue;
      }
      if (s[0] == 116) {
        body = 1;
        i++;
        continue;
      }
      if (s[0] == 110) {
        body = 2;
        i++;
        continue;
      }
      i++;
      continue;
    }
    if (s[0] == 45 && s[1] == 110 && s[2] == 0) {
      i++;
      s = argv[i];
      if (s[0] == 108 && s[1] == 110) {
        fmt = 0;
        i++;
        continue;
      }
      if (s[0] == 114 && s[1] == 110) {
        fmt = 1;
        i++;
        continue;
      }
      if (s[0] == 114 && s[1] == 122) {
        fmt = 2;
        i++;
        continue;
      }
      i++;
      continue;
    }
    if (s[0] == 45 && s[1] == 110) {
      if (s[2] == 108 && s[3] == 110) {
        fmt = 0;
        i++;
        continue;
      }
      if (s[2] == 114 && s[3] == 110) {
        fmt = 1;
        i++;
        continue;
      }
      if (s[2] == 114 && s[3] == 122) {
        fmt = 2;
        i++;
        continue;
      }
    }
    if (s[0] == 45 && s[1] == 98) {
      if (s[2] == 97) {
        body = 0;
        i++;
        continue;
      }
      if (s[2] == 116) {
        body = 1;
        i++;
        continue;
      }
      if (s[2] == 110) {
        body = 2;
        i++;
        continue;
      }
    }
    if (s[0] == 45 && s[1] == 0) {
      f = 0;
      i++;
      continue;
    }
    f = fopen(s, "r");
    if (!f) {
      fprintf(stderr, "nl: cannot open %s\n", s);
      return 1;
    }
    i++;
    break;
  }
  lineno = start;
  while (1) {
    if (f == 0) {
      c = getchar();
    } else {
      c = fgetc(f);
    }
    if (c == EOF) {
      break;
    }
    if (at_line_start != 0) {
      if (body == 2) {
        printf("%*s%*s", w, "", nl_seplen(sep), "");
      } else {
        if (body == 0 || c != 10) {
          nl_print_num(lineno, w, fmt, sep);
          lineno = lineno + incr;
        } else {
          printf("%*s%*s", w, "", nl_seplen(sep), "");
        }
      }
    }
    putchar(c);
    if (c == 10) {
      at_line_start = 1;
    } else {
      at_line_start = 0;
    }
  }
  if (at_line_start == 0) {
    putchar(10);
  }
  return 0;
}
