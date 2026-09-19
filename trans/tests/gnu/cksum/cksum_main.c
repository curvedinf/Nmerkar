#include "shim.h"

/* cksum_main.c — print CRC-32 and byte count of each FILE (or stdin).
   Output: "%u %d" (plus the name when FILE arguments are given). */

int cksum_stream(int f, int print_name, char* name) {
  int crc = 0;
  int len = 0;
  int n;
  char* buf = malloc(65536);
  while (1) {
    n = fread(buf, 1, 65536, f);
    if (n <= 0) {
      break;
    }
    crc = cksum_buf(crc, buf, n);
    len = len + n;
  }
  crc = cksum_length(crc, len);
  crc = crc ^ 4294967295;
  if (print_name != 0) {
    printf("%u %d %s\n", crc, len, name);
  } else {
    printf("%u %d\n", crc, len);
  }
  return 0;
}

int main() {
  int i = 1;
  int f;
  while (i < argc) {
    f = fopen(argv[i], "r");
    if (!f) {
      fprintf(stderr, "cksum: %s: No such file or directory\n", argv[i]);
      return 1;
    }
    cksum_stream(f, 1, argv[i]);
    fclose(f);
    i++;
  }
  if (argc == 1) {
    cksum_stream(stdin, 0, "");
  }
  return 0;
}
