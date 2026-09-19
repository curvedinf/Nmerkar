int main() {
  int i = 0;
  int s = 0;
  while (i < 5) {
    s += i;
    i++;
  }
  printf("%d\n", s);
  int t = 0;
  for (i = 0; i < 4; i++) {
    t = t + 2;
  }
  printf("%d\n", t);
  int u = 10;
  do {
    u = u + 1;
  } while (u < 10);
  printf("%d\n", u);
  int v = 0;
  for (i = 0; i < 10; i++) {
    if (i == 3) break;
    if (i == 1) continue;
    v += i;
  }
  printf("%d\n", v);
  return 0;
}
