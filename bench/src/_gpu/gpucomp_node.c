// gpucomp_node.c — Node N-API binding over the shared gpucomp Vulkan launcher
#include <node_api.h>
#include <string.h>
#include <stdlib.h>
#include "gpucomp.h"

static napi_value Init(napi_env env, napi_callback_info info) {
  size_t argc = 1; napi_value argv[1];
  napi_get_cb_info(env, info, &argc, argv, 0, 0);
  char dev[64]; size_t len = 0;
  napi_get_value_string_utf8(env, argv[0], dev, sizeof dev, &len);
  napi_value out;
  napi_create_int32(env, gpu_init(dev), &out);
  return out;
}

static double* buf_data(napi_env env, napi_value v, int* is_null) {
  bool t; napi_is_typedarray(env, v, &t);
  if (!t) { *is_null = 1; return 0; }
  napi_typedarray_type ty; size_t len; void* data;
  napi_get_typedarray_info(env, v, &ty, &len, &data, 0, 0);
  *is_null = 0;
  return (double*)data;
}

// run(kernel, n, A|null, asz, B|null, bsz, R, rsz, n0, n1, n2, n3) -> int
static napi_value Run(napi_env env, napi_callback_info info) {
  enum { NARG = 12 };
  size_t argc = NARG; napi_value argv[NARG];
  napi_get_cb_info(env, info, &argc, argv, 0, 0);
  if (argc < NARG) { napi_throw_error(env, 0, "run needs 12 args"); return 0; }
  char kernel[32]; size_t kl = 0;
  napi_get_value_string_utf8(env, argv[0], kernel, sizeof kernel, &kl);
  double n;  napi_get_value_double(env, argv[1], &n);
  int a_null, b_null;
  double* A = buf_data(env, argv[2], &a_null);
  double asz; napi_get_value_double(env, argv[3], &asz);
  double* B = buf_data(env, argv[4], &b_null);
  double bsz; napi_get_value_double(env, argv[5], &bsz);
  int r_null;
  double* R = buf_data(env, argv[6], &r_null);
  double rsz; napi_get_value_double(env, argv[7], &rsz);
  double pc[6];
  for (int i = 0; i < 4; i++) napi_get_value_double(env, argv[8 + i], &pc[i]);
  napi_get_value_double(env, argv[11], &pc[4]);
  int rc = gpu_run_sized(kernel, (uint64_t)n, a_null ? 0 : A, (size_t)asz, b_null ? 0 : B, (size_t)bsz,
                         r_null ? 0 : R, (size_t)rsz,
                         (int64_t)pc[0], (int64_t)pc[1], (int64_t)pc[2], (int64_t)pc[3], pc[4], 0);
  napi_value out; napi_create_int32(env, rc, &out);
  return out;
}

static napi_value ModInit(napi_env env, napi_value exports) {
  napi_value fn;
  napi_create_function(env, "init", NAPI_AUTO_LENGTH, Init, 0, &fn);
  napi_set_named_property(env, exports, "init", fn);
  napi_create_function(env, "run", NAPI_AUTO_LENGTH, Run, 0, &fn);
  napi_set_named_property(env, exports, "run", fn);
  return exports;
}
NAPI_MODULE(NODE_GYP_MODULE_NAME, ModInit)
