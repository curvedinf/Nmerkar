#include "shim.h"

/* tee_out.c — fan-out write. The subset has no arrays, so file handles
   cannot be kept open across chunks; instead every chunk is written by
   re-opening each file: the first chunk uses "w" (create/truncate), later
   chunks "a" (append). Behavior matches GNU tee's output. argv[i] is
   available in any function via the runtime argv extern. */

int tee_write_files(int from, int to, char* buf, int n, int append_mode) {
  int i = from;
  int f;
  char* mode = "w";
  while (i <= to) {
    if (append_mode != 0) {
      mode = "a";
    }
    f = fopen(argv[i], mode);
    if (!f) {
      fprintf(stderr, "tee: %s: cannot open\n", argv[i]);
      return 1;
    }
    fwrite(buf, 1, n, f);
    fclose(f);
    i++;
  }
  return 0;
}
