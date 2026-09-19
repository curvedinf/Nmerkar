#include "shim.h"

/* head_num.c — byte/line count parsing for head (-n/-c operands).
   Grammar: [+-]?digits [b|K|M|G]? with b=*512, K=*1024, M=*1024^2,
   G=*1024^3. Multiplication only — the subset has no division. */

int head_num(char* s) {
  int sign = 1;
  int i = 0;
  int n = 0;
  int mult = 1;
  int c;
  if (s[0] == 45) {
    sign = 0 - 1;
    i = 1;
  } else {
    if (s[0] == 43) {
      i = 1;
    }
  }
  c = s[i];
  while (c >= 48 && c <= 57) {
    n = n * 10 + (c - 48);
    i++;
    c = s[i];
  }
  if (c == 98) {
    mult = 512;
  }
  if (c == 75 || c == 107) {
    mult = 1024;
  }
  if (c == 77) {
    mult = 1048576;
  }
  if (c == 71) {
    mult = 1073741824;
  }
  return sign * n * mult;
}
