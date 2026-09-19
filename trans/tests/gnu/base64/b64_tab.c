#include "shim.h"

/* b64_tab.c — the base64 alphabet plus encode/decode primitives. The
   subset has no arrays; the alphabet is a string literal indexed by
   byte reads. No file-scope variables: everything threads through
   parameters. */

char* b64_table() {
  return "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
}

/* Index of c in the alphabet, or -1. */
int b64_index(int c) {
  char* t = b64_table();
  int i = 0;
  while (i < 64) {
    if (t[i] == c) {
      return i;
    }
    i++;
  }
  return 0 - 1;
}

/* Emit one character honoring the wrap width; returns the new column. */
int b64_out(int c, int col, int wrap) {
  putchar(c);
  if (wrap == 0) {
    return col + 1;
  }
  col = col + 1;
  if (col >= wrap) {
    putchar(10);
    col = 0;
  }
  return col;
}

/* Emit the alphabet char for a 6-bit value, honoring wrap. */
int b64_emit6(int v, int col, int wrap) {
  char* t = b64_table();
  return b64_out(t[v], col, wrap);
}

int b64_num(char* s) {
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
