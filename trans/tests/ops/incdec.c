int main() {
  int i = 5;
  i++;
  printf("%d\n", i);
  i--;
  i--;
  printf("%d\n", i);
  ++i;
  ++i;
  ++i;
  printf("%d\n", i);
  --i;
  printf("%d\n", i);
  int j = i++;
  printf("%d %d\n", j, i);
  return 0;
}
