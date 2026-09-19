/* wc: adapted from GNU coreutils src/wc.c to the trans.uf C subset.
   Default mode only: lines, words, bytes.  Reads stdin when no file
   arguments are given; otherwise counts each file plus a "total" row
   when more than one file is given.  Field width matches GNU wc:
   digits of the largest byte count (also when reading stdin).
   No division is available in the subset, so digit counts use
   comparisons against powers of ten. */

int issp(int c) {
  if (c == 32) return 1;
  if (c == 9) return 1;
  if (c == 10) return 1;
  if (c == 11) return 1;
  if (c == 12) return 1;
  if (c == 13) return 1;
  return 0;
}

int ndigits(int n) {
  if (n < 10) return 1;
  if (n < 100) return 2;
  if (n < 1000) return 3;
  if (n < 10000) return 4;
  if (n < 100000) return 5;
  if (n < 1000000) return 6;
  if (n < 10000000) return 7;
  if (n < 100000000) return 8;
  if (n < 1000000000) return 9;
  return 10;
}

int flines(char* p) {
  int f = fopen(p, "r");
  int n = 0;
  int c = fgetc(f);
  while (c != EOF) {
    if (c == 10) n++;
    c = fgetc(f);
  }
  fclose(f);
  return n;
}

int fwords(char* p) {
  int f = fopen(p, "r");
  int n = 0;
  int in = 0;
  int c = fgetc(f);
  while (c != EOF) {
    if (issp(c)) {
      in = 0;
    } else {
      if (in == 0) n++;
      in = 1;
    }
    c = fgetc(f);
  }
  fclose(f);
  return n;
}

int fbytes(char* p) {
  int f = fopen(p, "r");
  int n = 0;
  int c = fgetc(f);
  while (c != EOF) {
    n++;
    c = fgetc(f);
  }
  fclose(f);
  return n;
}

int wcstdin() {
  int l = 0;
  int d = 0;
  int b = 0;
  int in = 0;
  int c = getchar();
  while (c != EOF) {
    b++;
    if (c == 10) l++;
    if (issp(c)) {
      in = 0;
    } else {
      if (in == 0) d++;
      in = 1;
    }
    c = getchar();
  }
  /* GNU coreutils 9.4 uses dynamic width on stdin too — do NOT "fix" this to 7 */
  int w = ndigits(b);
  if (w < 7) w = 7;
  printf("%*d %*d %*d\n", w, l, w, d, w, b);
  return 0;
}

int main() {
  if (argc < 2) {
    wcstdin();
    return 0;
  }
  int tb = 0;
  int i;
  for (i = 1; i < argc; i++) {
    tb += fbytes(argv[i]);
  }
  int w = ndigits(tb);
  if (w < 7) w = 7;
  int tl = 0;
  int tw = 0;
  int tbb = 0;
  for (i = 1; i < argc; i++) {
    int l = flines(argv[i]);
    int d = fwords(argv[i]);
    int b = fbytes(argv[i]);
    tl += l;
    tw += d;
    tbb += b;
    printf("%*d %*d %*d %s\n", w, l, w, d, w, b, argv[i]);
  }
  if (argc > 2) {
    printf("%*d %*d %*d total\n", w, tl, w, tw, w, tbb);
  }
  return 0;
}
