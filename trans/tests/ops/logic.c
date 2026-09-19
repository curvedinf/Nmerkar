int main() {
  int a = 1;
  int b = 0;
  printf("%d %d\n", a && a, a && b);
  printf("%d %d\n", b || a, b || b);
  printf("%d %d\n", !a, !b);
  printf("%d\n", !a || b && a);
  return 0;
}
