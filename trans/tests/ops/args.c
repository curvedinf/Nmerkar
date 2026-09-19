int main() {
  printf("argc=%d\n", argc);
  if (argc > 1) {
    printf("a1=%s\n", argv[1]);
  }
  if (argc > 2) {
    printf("a2=%s\n", argv[2]);
  }
  int i = 0;
  int tot = 0;
  while (i < argc) {
    tot += 1;
    i++;
  }
  printf("tot=%d\n", tot);
  return 0;
}
