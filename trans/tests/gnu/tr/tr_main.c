#include "shim.h"

/* tr_main.c — translate or delete characters.
   Reads from stdin and writes to stdout. Translates characters from
   string1 to corresponding characters in string2. Without string2,
   deletes characters in string1. Supports -d (delete) and -s (squeeze
   repeats). Character ranges like a-z are supported. */

int main() {
  int i = 1;
  int delete_mode = 0;
  int squeeze = 0;
  int nfiles = 0;
  char* string1 = NULL;
  char* string2 = NULL;

  /* Parse options. */
  int j = 1;
  while (j < argc) {
    char* name = argv[j];
    if (name[0] == 45 && name[1] != 0) {
      int k = 1;
      while (name[k] != 0) {
        if (name[k] == 100) delete_mode = 1;  /* -d */
        else if (name[k] == 115) squeeze = 1;  /* -s */
        k = k + 1;
      }
      j = j + 1;
      continue;
    }
    nfiles = nfiles + 1;
    j = j + 1;
  }

  if (nfiles >= 1) {
    string1 = argv[1];
  }
  if (nfiles >= 2) {
    string2 = argv[2];
  }

  if (!string1) {
    fprintf(stderr, "tr: missing operand\n");
    return 1;
  }

  /* Read from stdin, translate, write to stdout. */
  int c;
  int prev_c = -1;
  int in_squeeze = 0;

  while (1) {
    c = getchar();
    if (c == EOF) break;

    /* Check if c is in string1. */
    int in_string1 = 0;
    int k = 0;
    while (string1[k] != 0) {
      if (string1[k] == c) {
        in_string1 = 1;
        break;
      }
      /* Handle ranges: a-z */
      if (string1[k] == 45 && string1[k + 1] != 0) {
        /* range start */
        int range_start = string1[k - 1];
        int range_end = string1[k + 1];
        if (c >= range_start && c <= range_end) {
          in_string1 = 1;
          break;
        }
        k = k + 2;
        continue;
      }
      k = k + 1;
    }

    if (delete_mode && in_string1) {
      /* Skip this character. */
      continue;
    }

    if (squeeze && in_string1) {
      if (c == prev_c) {
        continue;
      }
      prev_c = c;
      putchar(c);
      continue;
    }

    if (string2 && in_string1) {
      /* Translate c to corresponding character in string2. */
      int k = 0;
      int found = 0;
      while (string1[k] != 0) {
        if (string1[k] == c) {
          found = 1;
          break;
        }
        if (string1[k] == 45 && string1[k + 1] != 0) {
          k = k + 2;
          continue;
        }
        k = k + 1;
      }
      if (found) {
        /* Find corresponding character in string2. */
        int j = 0;
        int count = 0;
        while (string1[j] != 0) {
          if (string1[j] == c) {
            break;
          }
          if (string1[j] == 45 && string1[j + 1] != 0) {
            j = j + 2;
            continue;
          }
          j = j + 1;
        }
        /* Count characters in string2 up to position j. */
        int pos = 0;
        while (string2[pos] != 0) {
          if (string2[pos] == 45 && string2[pos + 1] != 0) {
            /* range in string2 */
            pos = pos + 2;
            continue;
          }
          pos = pos + 1;
          if (pos > j) break;
        }
        putchar(string2[pos - 1]);
      } else {
        putchar(c);
      }
    } else {
      putchar(c);
    }
  }

  return 0;
}
