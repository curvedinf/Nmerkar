#include "shim.h"

/* wc_main.c — main entry and output for wc.
   Flags: 1=-l 2=-w 4=-c 8=-m 16=-L
   Prints per-file counts; prints "total" line when >1 file.
   stdin is read when no FILE arguments are given, or when "-" is given.
   stdin is buffered to a temp file first (wc_stdin_buffer in wc_wc.c) so
   the per-file counting functions can re-read it. */

int main() {
  int flags = 0;
  int i = 1;
  int nfiles = 0;
  int total_nl = 0;
  int total_nw = 0;
  int total_nb = 0;
  int total_max = 0;
  char* name;
  int nl, nw, nb, maxline;
  int seen_file = 0;
  char* tmpname = "/tmp/wc_tmp";

  /* Count files (non-option arguments) to decide whether to print names.
     GNU wc always prints filenames when more than one file is given. */
  int j = 1;
  while (j < argc) {
    name = argv[j];
    if (name[0] == 45 && name[1] != 0) {
      int k = 1;
      while (name[k] != 0) {
        k = k + 1;
      }
      j = j + 1;
      continue;
    }
    nfiles = nfiles + 1;
    j = j + 1;
  }

  /* If reading from stdin, buffer it to a temp file first. */
  if (nfiles == 0) {
    wc_stdin_buffer(tmpname);
    name = tmpname;
  }

  j = 1;
  while (j < argc) {
    name = argv[j];
    if (name[0] == 45 && name[1] != 0) {
      int k = 1;
      while (name[k] != 0) {
        if (name[k] == 108) flags = flags | 1;
        else if (name[k] == 119) flags = flags | 2;
        else if (name[k] == 99) flags = flags | 4;
        else if (name[k] == 109) flags = flags | 8;
        else if (name[k] == 76) flags = flags | 16;
        else {
          fprintf(stderr, "wc: invalid option -- '%c'\n", name[k]);
          return 1;
        }
        k = k + 1;
      }
      j = j + 1;
      continue;
    }

    seen_file = 1;
    if (name[0] == 45 && name[1] == 0) {
      /* "-" as a file argument among others: read stdin fresh. */
      wc_stdin_buffer(tmpname);
      name = tmpname;
    }

    /* Count the file. Each counting function opens, counts, closes. */
    nl = wc_lines(name);
    if (nl < 0) {
      fprintf(stderr, "wc: %s: No such file or directory\n", name);
      j = j + 1;
      continue;
    }
    nw = wc_words(name);
    nb = wc_bytes(name);
    maxline = wc_maxline(name);

    /* Print per-file counts. Print filename when >=1 file argument given. */
    wc_print(nl, nw, nb, maxline, flags, name, nfiles > 0);

    total_nl = total_nl + nl;
    total_nw = total_nw + nw;
    total_nb = total_nb + nb;
    if (maxline > total_max) total_max = maxline;

    j = j + 1;
  }

  if (!seen_file) {
    /* No file arguments: stdin was already buffered to tmpname above. */
    nl = wc_lines(tmpname);
    nw = wc_words(tmpname);
    nb = wc_bytes(tmpname);
    maxline = wc_maxline(tmpname);
    wc_print(nl, nw, nb, maxline, flags, "", 0);
  }

  if (nfiles > 1) {
    wc_print(total_nl, total_nw, total_nb, total_max, flags, "total", 1);
  }

  return 0;
}
