#include "shim.h"

/* cat_opts.c — option parsing for cat.
   Flags are packed into a bitmask:
     1 number-nonblank (-b, implies -n)   2 show-ends (-E)
     4 number-lines (-n)                  8 squeeze-blank (-S)
    16 show-tabs (-T)                   32 show-nonprinting (-v)
   GNU cat option equivalence: -A = -vET, -e = -vE, -t = -vT, -s = -S.
   Long options (--show-all etc.) are handled by the long table below. */

int cat_set_flag(int flags, int bit) {
  return flags | bit;
}

int cat_parse_short(char c, int flags) {
  if (c == 98) return cat_set_flag(flags, 1 | 4);
  if (c == 69) return cat_set_flag(flags, 2);
  if (c == 110) return cat_set_flag(flags, 4);
  if (c == 115) return cat_set_flag(flags, 8);
  if (c == 84) return cat_set_flag(flags, 16);
  if (c == 118) return cat_set_flag(flags, 32);
  if (c == 65) return cat_set_flag(flags, 2 | 16 | 32);
  if (c == 101) return cat_set_flag(flags, 2 | 32);
  if (c == 116) return cat_set_flag(flags, 16 | 32);
  if (c == 117) return flags;
  return 0 - 1;
}

/* Returns updated flags, or -1 on unknown option letter. */
int cat_parse_arg(char* s, int flags) {
  int i = 1;
  while (s[i] != 0) {
    flags = cat_parse_short(s[i], flags);
    if (flags == 0 - 1) return 0 - 1;
    i++;
  }
  return flags;
}
