#include "shim.h"

/* b64_main.c — base64 encode (default) / decode (-d) of stdin or one
   FILE, wrapping encoded output at -w columns (default 76, 0 = never).
   Decode ignores newlines; '=' pads. Invalid input characters are an
   error (GNU without -i). */

int b64_read(int f) {
  if (f == 0) {
    return getchar();
  }
  return fgetc(f);
}

int b64_encode(int f, int wrap) {
  int col = 0;
  int n;
  int v;
  int c;
  int b0 = 0;
  int b1 = 0;
  int b2 = 0;
  while (1) {
    n = 0;
    while (n < 3) {
      c = b64_read(f);
      if (c == EOF) {
        break;
      }
      if (n == 0) {
        b0 = c;
      }
      if (n == 1) {
        b1 = c;
      }
      if (n == 2) {
        b2 = c;
      }
      n++;
    }
    if (n == 0) {
      break;
    }
    v = b0 << 16;
    if (n >= 2) {
      v = v | (b1 << 8);
    }
    if (n >= 3) {
      v = v | b2;
    }
    col = b64_emit6((v >> 18) & 63, col, wrap);
    col = b64_emit6((v >> 12) & 63, col, wrap);
    if (n == 1) {
      col = b64_out(61, col, wrap);
      col = b64_out(61, col, wrap);
    } else {
      col = b64_emit6((v >> 6) & 63, col, wrap);
      if (n == 2) {
        col = b64_out(61, col, wrap);
      } else {
        col = b64_emit6(v & 63, col, wrap);
      }
    }
  }
  if (col != 0 && wrap != 0) {
    putchar(10);
  }
  return 0;
}

int b64_decode(int f) {
  int c;
  int idx;
  int quad = 0;
  int v = 0;
  int pad = 0;
  while (1) {
    c = b64_read(f);
    if (c == EOF) {
      break;
    }
    if (c == 10 || c == 13 || c == 32 || c == 9) {
      continue;
    }
    if (c == 61) {
      pad++;
      idx = 0;
    } else {
      if (pad != 0) {
        fprintf(stderr, "base64: extra data after padding\n");
        return 1;
      }
      idx = b64_index(c);
      if (idx == 0 - 1) {
        fprintf(stderr, "base64: invalid input\n");
        return 1;
      }
    }
    v = (v << 6) | idx;
    quad++;
    if (quad == 4) {
      if (pad == 0) {
        putchar((v >> 16) & 255);
        putchar((v >> 8) & 255);
        putchar(v & 255);
      }
      if (pad == 1) {
        putchar((v >> 16) & 255);
        putchar((v >> 8) & 255);
      }
      if (pad == 2) {
        putchar((v >> 16) & 255);
      }
      quad = 0;
      v = 0;
      if (pad != 0) {
        return 0;
      }
    }
  }
  return 0;
}

int main() {
  int decode = 0;
  int wrap = 76;
  int i = 1;
  char* s;
  int f = 0;
  while (i < argc) {
    s = argv[i];
    if (s[0] == 45 && s[1] == 100 && s[2] == 0) {
      decode = 1;
      i++;
      continue;
    }
    if (s[0] == 45 && s[1] == 119) {
      if (s[2] != 0) {
        wrap = b64_num(s + 2);
        i++;
        continue;
      }
      i++;
      wrap = b64_num(argv[i]);
      i++;
      continue;
    }
    f = fopen(s, "r");
    if (!f) {
      fprintf(stderr, "base64: %s: No such file or directory\n", s);
      return 1;
    }
    i++;
    break;
  }
  if (decode != 0) {
    return b64_decode(f);
  }
  return b64_encode(f, wrap);
}
