#!/usr/bin/env bash
set -euo pipefail

CONFIG_PATH="${HOME}/.config/noland-mic/config.env"

sender_pid_file="/tmp/noland-mic-sender.pid"
receiver_pid_file="/tmp/noland-mic-receiver.pid"

print_state() {
  local label="$1"
  local pid_file="$2"
  if [[ -f "${pid_file}" ]]; then
    local pid
    pid="$(cat "${pid_file}")"
    if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
      printf '%s: running (pid %s)\n' "${label}" "${pid}"
      return
    fi
  fi
  printf '%s: stopped\n' "${label}"
}

print_state "sender" "${sender_pid_file}"
print_state "receiver" "${receiver_pid_file}"

if command -v pactl >/dev/null 2>&1; then
  if [[ -f "${CONFIG_PATH}" ]]; then
    # shellcheck disable=SC1090
    source "${CONFIG_PATH}"
    if [[ -n "${VIRTUAL_MIC_SINK:-}" ]]; then
      if pactl list short sinks | awk '{print $2}' | grep -qx "${VIRTUAL_MIC_SINK}"; then
        echo "sink ${VIRTUAL_MIC_SINK}: present"
      else
        echo "sink ${VIRTUAL_MIC_SINK}: missing"
      fi
    fi
    if [[ -n "${VIRTUAL_MIC_SOURCE:-}" ]]; then
      if pactl list short sources | awk '{print $2}' | grep -qx "${VIRTUAL_MIC_SOURCE}"; then
        echo "source ${VIRTUAL_MIC_SOURCE}: present"
      else
        echo "source ${VIRTUAL_MIC_SOURCE}: missing"
      fi
    fi
  fi
fi

echo "sender logs: /tmp/noland-mic-sender.log"
echo "receiver logs: /tmp/noland-mic-receiver.log"
