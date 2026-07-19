#include "noland_moonlight.h"

#include <stdio.h>

int main(void) {
  nl_runtime_t* runtime = NULL;
  if (nl_runtime_create(&runtime) != NL_RESULT_OK) {
    fprintf(stderr, "failed to create runtime\n");
    return 1;
  }

  if (nl_runtime_smoke_test() != 7) {
    fprintf(stderr, "unexpected smoke test result\n");
    nl_runtime_destroy(runtime);
    return 1;
  }

  nl_runtime_destroy(runtime);
  return 0;
}
