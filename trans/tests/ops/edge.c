int add2(int a, int b) {
  return a + b;
}
int dead(int x) {
  return x;
  printf("unreachable\n");
}
int early(int n) {
  if (n > 10) {
    return n - 10;
  }
  while (n < 100) {
    n += 13;
    if (n > 50) {
      return n;
    }
  }
  return 0 - 1;
}
int main() {
  int i = 3;
  int j = i++;
  printf("A %d %d\n", i, j);
  int a = 0;
  int b = 0;
  a = b = 5;
  printf("B %d %d\n", a, b);
  a += b -= 2;
  printf("C %d %d\n", a, b);
  printf("D %d\n", add2(i++, ++j));
  printf("E %d %d\n", i, j);
  printf("F %d %d %d\n", -(-(-2)), ~0, !5 + !0);
  for (;;) {
    a++;
    break;
  }
  printf("G %d\n", a);
  int k;
  for (k = 10; k > 0; k -= 3) {
    if (k == 4) continue;
    if (k < 2) break;
    printf("H %d\n", k);
  }
  printf("I %d %d %d\n", early(25), early(5), early(200));
  printf("J %d\n", dead(7));
  return 0;
}
