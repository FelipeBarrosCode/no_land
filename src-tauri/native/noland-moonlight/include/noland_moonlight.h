#ifndef NOLAND_MOONLIGHT_H
#define NOLAND_MOONLIGHT_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct nl_runtime nl_runtime_t;

typedef enum nl_result {
  NL_RESULT_OK = 0,
  NL_RESULT_INVALID_ARGUMENT = 1,
  NL_RESULT_OUT_OF_MEMORY = 2,
  NL_RESULT_NOT_READY = 3
} nl_result_t;

nl_result_t nl_runtime_create(nl_runtime_t** output);
void nl_runtime_destroy(nl_runtime_t* runtime);
const char* nl_runtime_version_string(void);
int32_t nl_runtime_smoke_test(void);

#ifdef __cplusplus
}
#endif

#endif
