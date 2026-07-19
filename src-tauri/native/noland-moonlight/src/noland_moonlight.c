#include "noland_moonlight.h"

#include <stdlib.h>

struct nl_runtime {
  uint32_t reserved;
};

nl_result_t nl_runtime_create(nl_runtime_t** output) {
  if (output == NULL) {
    return NL_RESULT_INVALID_ARGUMENT;
  }

  nl_runtime_t* runtime = (nl_runtime_t*)calloc(1, sizeof(nl_runtime_t));
  if (runtime == NULL) {
    return NL_RESULT_OUT_OF_MEMORY;
  }

  *output = runtime;
  return NL_RESULT_OK;
}

void nl_runtime_destroy(nl_runtime_t* runtime) {
  free(runtime);
}

const char* nl_runtime_version_string(void) {
  return "noland-moonlight/0.1.0";
}

int32_t nl_runtime_smoke_test(void) {
  return 7;
}
