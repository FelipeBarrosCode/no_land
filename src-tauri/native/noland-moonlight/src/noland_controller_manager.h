#ifndef NOLAND_CONTROLLER_MANAGER_H
#define NOLAND_CONTROLLER_MANAGER_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct nl_runtime nl_runtime_t;

typedef struct nl_controller_manager nl_controller_manager_t;

nl_controller_manager_t* nl_controller_manager_create(void);
void nl_controller_manager_destroy(nl_controller_manager_t* manager);

typedef struct nl_dualsense_output_report {
  uint8_t valid_flag0;
  uint8_t valid_flag1;
  uint8_t motor_right;
  uint8_t motor_left;
  uint8_t reserved[4];
  uint8_t mute_button_led;
  uint8_t power_save_control;
  uint8_t right_trigger_effect_type;
  uint8_t right_trigger_effect[10];
  uint8_t left_trigger_effect_type;
  uint8_t left_trigger_effect[10];
  uint8_t reserved2[6];
  uint8_t valid_flag2;
  uint8_t reserved3[2];
  uint8_t lightbar_setup;
  uint8_t led_brightness;
  uint8_t player_leds;
  uint8_t lightbar_red;
  uint8_t lightbar_green;
  uint8_t lightbar_blue;
} nl_dualsense_output_report_t;

bool nl_controller_manager_start(nl_controller_manager_t* manager, nl_runtime_t* runtime);
void nl_controller_manager_stop(nl_controller_manager_t* manager);

void nl_controller_manager_rumble(nl_controller_manager_t* manager, uint16_t controller_number, uint16_t low_freq_motor, uint16_t high_freq_motor);
void nl_controller_manager_rumble_triggers(nl_controller_manager_t* manager, uint16_t controller_number, uint16_t left_trigger, uint16_t right_trigger);
void nl_controller_manager_set_motion_event_state(nl_controller_manager_t* manager, uint16_t controller_number, uint8_t motion_type, uint16_t report_rate_hz);
void nl_controller_manager_set_led(nl_controller_manager_t* manager, uint16_t controller_number, uint8_t r, uint8_t g, uint8_t b);
void nl_controller_manager_set_adaptive_triggers(nl_controller_manager_t* manager, uint16_t controller_number, const nl_dualsense_output_report_t* report);

#ifdef __cplusplus
}
#endif

#endif
