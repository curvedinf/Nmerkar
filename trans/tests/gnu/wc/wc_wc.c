#include "shim.h"

/* wc_wc.c — counting primitives for wc.
   Each function opens the file by name, counts, and closes it — so
   calls don't interfere with each other's file position.
   "-" means stdin; the caller is responsible for buffering stdin to a
   temp file first and passing that temp file name instead. */

int wc_stdin_buffer(char* tmpname) {
  char* buf = malloc(65536);
  int f = fopen(tmpname, "w");
  int n;
  while ((n = fread(buf, 1, 65536, stdin)) > 0) {
    fwrite(buf, 1, n, f);
  }
  fclose(f);
  free(buf);
  return 0;
}

/* Count newlines in file. */
int wc_lines(char* name) {
  int c;
  int nl = 0;
  int f = fopen(name, "r");
  if (!f) return -1;
  while (1) {
    c = fgetc(f);
    if (c == EOF) break;
    if (c == 10) nl = nl + 1;
  }
  fclose(f);
  return nl;
}

/* Count words in file. A word is a run of non-whitespace bytes.
   Whitespace: space, tab, newline, vertical tab, form feed, CR. */
int wc_words(char* name) {
  int c;
  int nw = 0;
  int in_word = 0;
  int f = fopen(name, "r");
  if (!f) return -1;
  while (1) {
    c = fgetc(f);
    if (c == EOF) break;
    if (c == 32 || c == 9 || c == 10 || c == 13 || c == 11 || c == 12) {
      in_word = 0;
    } else if (in_word == 0) {
      in_word = 1;
      nw = nw + 1;
    }
  }
  fclose(f);
  return nw;
}

/* Count bytes in file. */
int wc_bytes(char* name) {
  int c;
  int nb = 0;
  int f = fopen(name, "r");
  if (!f) return -1;
  while (1) {
    c = fgetc(f);
    if (c == EOF) break;
    nb = nb + 1;
  }
  fclose(f);
  return nb;
}

/* Count maximum line length in file. */
int wc_maxline(char* name) {
  int c;
  int maxline = 0;
  int curline = 0;
  int f = fopen(name, "r");
  if (!f) return -1;
  while (1) {
    c = fgetc(f);
    if (c == EOF) break;
    if (c == 10) {
      if (curline > maxline) maxline = curline;
      curline = 0;
    } else {
      curline = curline + 1;
    }
  }
  if (curline > maxline) maxline = curline;
  fclose(f);
  return maxline;
}

/* Number of decimal digits in a non-negative integer. */
int num_digits(int v) {
  if (v == 0) return 1;
  if (v < 0) v = -v;
  int d = 1;
  while (v >= 10) { d = d + 1; v = v / 10; }
  return d;
}

/* Print a wc output line. */
int wc_print(int nl, int nw, int nb, int maxline, int flags, char* name, int print_name) {
  int has_l = (flags & 1) != 0;
  int has_w = (flags & 2) != 0;
  int has_c = (flags & 4) != 0;
  int has_m = (flags & 8) != 0;
  int has_L = (flags & 16) != 0;
  int width = 1;
  int d;

  if (flags == 0) {
    /* Compute width for all values when printing all three. */
    d = num_digits(nl); if (d > width) width = d;
    d = num_digits(nw); if (d > width) width = d;
    d = num_digits(nb); if (d > width) width = d;
    printf("%*d %*d %*d", width, nl, width, nw, width, nb);
    if (print_name) { printf(" "); printf("%s", name); }
    printf("\n");
    return 0;
  }

  /* Compute width for flag-specific values. */
  if (has_l) { d = num_digits(nl); if (d > width) width = d; }
  if (has_w) { d = num_digits(nw); if (d > width) width = d; }
  if (has_c) { d = num_digits(nb); if (d > width) width = d; }
  if (has_m) { d = num_digits(nb); if (d > width) width = d; }
  if (has_L) { d = num_digits(maxline); if (d > width) width = d; }

  if (has_l) printf("%*d", width, nl);
  if (has_w) printf("%*d", width, nw);
  if (has_c) printf("%*d", width, nb);
  if (has_m) printf("%*d", width, nb);
  if (has_L) printf("%*d", width, maxline);
  if (print_name) { printf(" "); printf("%s", name); }
  printf("\n");
  return 0;
}
