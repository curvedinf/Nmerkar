/* nativelib.c — proves the libc calls with Enmerkar-native equivalents
   compile to ops, not FFI imports: strlen -> length, strcat -> concat,
   puts -> print (+newline). A grep gate in the runner additionally checks
   the emitted .en contains no `_call strlen/strcat/puts`. */
int main() {
  char* a = "enmer";
  char* b = "kar";
  printf("%d\n", strlen("hello"));
  printf("%d\n", strlen(""));
  printf("%d\n", strlen(a));
  char* j = strcat(a, b);
  printf("%s\n", j);
  puts(j);
  puts("native ops");
  printf("%d\n", strlen(j));
  return 0;
}
