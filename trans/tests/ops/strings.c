int slen(char* s) {
  int n = 0;
  while (s[n] != 0) {
    n++;
  }
  return n;
}

int count(char* s, int c) {
  int n = 0;
  int i = 0;
  while (s[i] != 0) {
    if (s[i] == c) n++;
    i++;
  }
  return n;
}

int main() {
  printf("%d\n", slen("hello"));
  printf("%d\n", slen(""));
  printf("%d\n", count("banana", 97));
  printf("%d\n", count("banana", 110));
  char* msg = "hi there";
  printf("%d %d\n", msg[0], msg[1]);
  printf("%d\n", msg[0] == 104);
  puts(msg);
  return 0;
}
