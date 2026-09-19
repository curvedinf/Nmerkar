/* gcc reference build only — never transpiled (the runner passes the
   tool's .c files explicitly). Supplies the real main and forwards the
   arguments for the argc/argv macros in shim.h. */
#include "shim.h"
#undef main
int main(int __ac, char** __av) {
  __nkr_argc_g = __ac;
  __nkr_argv_g = __av;
  return tool_main();
}
