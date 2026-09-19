#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Reference builds (gcc) only: forward the program arguments to helper
   functions that use the argc/argv builtins. The transpiler implements
   argc/argv as runtime builtins available in any function, so it skips
   this shim entirely (leading '#' lines are dropped). Reference builds
   compile gnu_main.c (never transpiled), which calls the tool main. */
static int __nkr_argc_g;
static char** __nkr_argv_g;
#define argc __nkr_argc_g
#define argv __nkr_argv_g
int tool_main();
#define main tool_main
