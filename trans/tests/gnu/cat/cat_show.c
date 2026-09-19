#include "shim.h"

/* cat_show.c — pure per-byte rendering for cat's -v/-E/-T transformations
   and the input abstraction. No file-scope state: everything is a
   parameter or local. */

/* Read one byte: f == 0 means stdin. */
int cat_get(int f) {
  if (f == 0) {
    return getchar();
  }
  return fgetc(f);
}

/* Print c applying -E (show-ends), -T (show-tabs), -v (show-nonprinting). */
void cat_render(int c, int flags) {
  int v = flags & 32;
  int d;
  if (c == 10) {
    if ((flags & 2) != 0) {
      putchar(36);
    }
    putchar(10);
    return 0;
  }
  if (c == 9) {
    if ((flags & 16) != 0) {
      printf("^I");
    } else {
      putchar(9);
    }
    return 0;
  }
  if (v != 0) {
    if (c == 127) {
      printf("^?");
      return 0;
    }
    if (c < 32) {
      putchar(94);
      putchar(c + 64);
      return 0;
    }
    if (c > 126) {
      d = c - 128;
      if (d == 127) {
        printf("M-^?");
        return 0;
      }
      if (d < 32) {
        printf("M-^");
        putchar(d + 64);
        return 0;
      }
      printf("M-");
      putchar(d);
      return 0;
    }
  }
  putchar(c);
  return 0;
}
