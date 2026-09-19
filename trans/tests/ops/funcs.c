int add2(int a, int b) {
  return a + b;
}

int fib(int n) {
  if (n < 2) return n;
  return fib(n - 1) + fib(n - 2);
}

int noret(int x) {
  x = x * 2;
  if (x > 100) return x;
  x = x + 1;
  return x;
}

int main() {
  printf("%d\n", add2(3, 4));
  printf("%d\n", fib(10));
  printf("%d\n", noret(3));
  printf("%d\n", noret(60));
  return 0;
}
