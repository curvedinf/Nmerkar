int main() {
  int i = 1;
  int s = 0;
  while (i <= 10) {
    s = s + i;
    i = i + 1;
  }
  if (s == 55) {
    puts("hello, nmerkar");
  } else {
    puts("wrong");
  }
  printf("sum=%d\n", s);
  return 0;
}
