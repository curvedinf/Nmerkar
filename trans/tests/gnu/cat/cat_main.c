#include "shim.h"

/* cat_main.c — stream driver: option parsing (cat_opts.c), per-byte
   rendering (cat_show.c). Line numbering and blank-squeezing state live as
   locals of cat_file so they reset per input, matching GNU cat. */

int cat_file(int f, int flags) {
  int c;
  int at_line_start = 1;
  int prev_nl = 1;
  int blank_emitted = 0;
  int lineno = 0;
  int number = flags & 4;
  int nonblank = flags & 1;
  int squeeze = flags & 8;
  while (1) {
    c = cat_get(f);
    if (c == EOF) {
      break;
    }
    if (c == 10 && prev_nl != 0) {
      /* empty line */
      if (squeeze != 0 && blank_emitted != 0) {
        continue;
      }
      blank_emitted = 1;
      if (number != 0 && nonblank == 0) {
        lineno = lineno + 1;
        printf("%6d\t", lineno);
      }
      cat_render(c, flags);
      prev_nl = 1;
      at_line_start = 1;
      continue;
    }
    if (at_line_start != 0 && number != 0) {
      lineno = lineno + 1;
      printf("%6d\t", lineno);
    }
    blank_emitted = 0;
    cat_render(c, flags);
    if (c == 10) {
      at_line_start = 1;
      prev_nl = 1;
    } else {
      at_line_start = 0;
      prev_nl = 0;
    }
  }
  return 0;
}

int main() {
  int flags = 0;
  int i = 1;
  int saw_file = 0;
  int no_more_opts = 0;
  char* name;
  int f;
  while (i < argc) {
    name = argv[i];
    if (no_more_opts == 0 && name[0] == 45 && name[1] != 0) {
      if (name[1] == 45 && name[2] == 0) {
        no_more_opts = 1;
        i++;
        continue;
      }
      flags = cat_parse_arg(name, flags);
      if (flags == 0 - 1) {
        fprintf(stderr, "cat: invalid option -- %s\n", name + 1);
        return 1;
      }
      i++;
      continue;
    }
    saw_file = 1;
    if (name[0] == 45 && name[1] == 0) {
      cat_file(0, flags);
    } else {
      f = fopen(name, "r");
      if (!f) {
        fprintf(stderr, "cat: %s: No such file or directory\n", name);
        return 1;
      }
      cat_file(f, flags);
      fclose(f);
    }
    i++;
  }
  if (saw_file == 0) {
    cat_file(0, flags);
  }
  return 0;
}
