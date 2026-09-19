int classify(int n) {
  if (n < 0) return 0 - 1;
  if (n == 0) return 0;
  while (n > 10) {
    if (n > 100) return 100;
    n = n - 10;
  }
  return n;
}

int loopfind(int target) {
  int i;
  for (i = 0; i < 100; i++) {
    if (i * i > target) return i;
  }
  return 0 - 1;
}

int main() {
  printf("%d\n", classify(0 - 5));
  printf("%d\n", classify(0));
  printf("%d\n", classify(7));
  printf("%d\n", classify(25));
  printf("%d\n", classify(500));
  printf("%d\n", loopfind(10));
  printf("%d\n", loopfind(99));
  printf("%d\n", loopfind(10000));
  return 0;
}
