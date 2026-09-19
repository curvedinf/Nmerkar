int main() {
  int i;
  int j;
  int s = 0;
  for (i = 0; i < 3; i++) {
    for (j = 0; j < 4; j++) {
      if (j == 2) break;
      s = s + 1;
    }
    while (s < 2) {
      s = s + 10;
    }
  }
  printf("%d\n", s);
  int k = 0;
  while (k < 3) {
    if (k == 1) {
      int m = k;
      while (m < 2) {
        m++;
      }
      printf("%d\n", m);
    }
    k++;
  }
  return 0;
}
