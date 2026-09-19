#include "shim.h"

/* cksum_crc.c — POSIX cksum CRC (IEEE 802.3 frame check): MSB-first,
   polynomial 0x04C11DB7, init 0, explicit 32-bit masking (the subset's
   cells are 64-bit). The length is fed big-endian after the data. */

int cksum_byte(int crc, int c) {
  int j = 0;
  crc = crc ^ ((c & 255) << 24);
  crc = crc & 4294967295;
  while (j < 8) {
    if ((crc & 2147483648) != 0) {
      crc = ((crc << 1) ^ 79764919) & 4294967295;
    } else {
      crc = (crc << 1) & 4294967295;
    }
    j++;
  }
  return crc;
}

int cksum_buf(int crc, char* buf, int n) {
  int i = 0;
  while (i < n) {
    crc = cksum_byte(crc, buf[i]);
    i++;
  }
  return crc;
}

/* GNU/POSIX cksum feeds the length little-endian, one byte at a time
   while the remaining length is nonzero (an empty file feeds nothing).
   Shifts are unrolled to constants. */
int cksum_length(int crc, int len) {
  if (len != 0) {
    crc = cksum_byte(crc, len & 255);
  }
  if (len > 255) {
    crc = cksum_byte(crc, (len >> 8) & 255);
  }
  if (len > 65535) {
    crc = cksum_byte(crc, (len >> 16) & 255);
  }
  if (len > 16777215) {
    crc = cksum_byte(crc, (len >> 24) & 255);
  }
  if (len > 4294967295) {
    crc = cksum_byte(crc, (len >> 32) & 255);
  }
  return crc;
}
