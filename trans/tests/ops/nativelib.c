/* nativelib.c — proves the libc calls with Nmerkar-native equivalents
   compile to ops, not FFI imports: strlen -> length, strcat -> concat,
   puts -> print (+newline). A grep gate in the runner additionally checks
   the emitted .nd contains no `_call strlen/strcat/puts`. */
int main() {
  char* a = "nmer";
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
