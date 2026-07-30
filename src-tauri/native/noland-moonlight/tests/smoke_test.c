#include "noland_moonlight.h"

#include <stdio.h>
#include <string.h>

int main(void) {
  nl_runtime_t* runtime = NULL;
  nl_start_request_t start_request;
  nl_event_t event;
  nl_stats_t stats;

  if (nl_runtime_create(&runtime) != NL_RESULT_OK) {
    fprintf(stderr, "failed to create runtime\n");
    return 1;
  }

  if (nl_runtime_smoke_test() != 7) {
    fprintf(stderr, "unexpected smoke test result\n");
    nl_runtime_destroy(runtime);
    return 1;
  }

  memset(&start_request, 0, sizeof(start_request));
  start_request.host_id = "host-1";
  start_request.app_id = 10;
  start_request.session_url = "rtsp://example/session";
  start_request.host_address = "10.77.0.1";
  start_request.server_app_version = "7.1.431.-1";
  start_request.width = 1920;
  start_request.height = 1080;
  start_request.fps = 60;
  start_request.bitrate_kbps = 25000;
  start_request.packet_size = 1024;
  start_request.streaming_remotely = 1;
  start_request.audio_configuration = 0x000302CA;
  start_request.supported_video_formats = 1;
  start_request.client_refresh_rate_x100 = 6000;
  start_request.color_space = 1;
  start_request.color_range = 0;
  start_request.encryption_flags = -1;

  if (nl_runtime_start(runtime, &start_request) != NL_RESULT_OK) {
    fprintf(stderr, "failed to start runtime\n");
    nl_runtime_destroy(runtime);
    return 1;
  }

  if (nl_runtime_poll_event(runtime, &event) != NL_RESULT_OK) {
    fprintf(stderr, "expected queued event after start\n");
    nl_runtime_destroy(runtime);
    return 1;
  }

  if (nl_runtime_read_stats(runtime, &stats) != NL_RESULT_OK) {
    fprintf(stderr, "failed to read stats\n");
    nl_runtime_destroy(runtime);
    return 1;
  }

  if (stats.start_count != 1) {
    fprintf(stderr, "unexpected start_count=%llu\n", (unsigned long long)stats.start_count);
    nl_runtime_destroy(runtime);
    return 1;
  }

  if (nl_runtime_request_stop(runtime) != NL_RESULT_OK) {
    fprintf(stderr, "failed to stop runtime\n");
    nl_runtime_destroy(runtime);
    return 1;
  }

  nl_runtime_destroy(runtime);
  return 0;
}
