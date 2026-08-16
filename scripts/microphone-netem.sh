#!/usr/bin/env bash
# Apply guarded, microphone-port-only Linux netem scenarios.
set -Eeuo pipefail

COMMAND="${1:-help}"
[[ $# -gt 0 ]] && shift
INTERFACE="lo"
SCENARIO="wifi"
PORTS_CSV="48200,48201,48202"
STATE_ROOT="/run/noland-mic-netem"
RUN_COMMAND=()

usage() {
    cat <<'EOF'
Usage:
  scripts/microphone-netem.sh plan  SCENARIO [--interface IFACE] [--ports CSV]
  sudo scripts/microphone-netem.sh apply SCENARIO [--interface IFACE] [--ports CSV]
  sudo scripts/microphone-netem.sh run   SCENARIO [--interface IFACE] [--ports CSV] -- COMMAND...
  sudo scripts/microphone-netem.sh clear [--interface IFACE]
  scripts/microphone-netem.sh status [--interface IFACE]

Scenarios (egress UDP only):
  mild       10ms +/- 3ms delay, 0.2% random loss
  wifi       35ms +/- 12ms delay, 1% loss, 0.2% reordering
  congested  80ms +/- 25ms delay, 3% loss, 256kbit rate
  outage     100% packet loss

Safety properties:
  * plan/status never need root and do not change networking.
  * apply/run/clear require an explicit sudo invocation; this script never calls sudo.
  * only the listed IPv4 UDP destination ports enter the impaired prio band.
  * an existing non-noqueue root qdisc is never replaced.
  * run executes COMMAND as the original sudo user, then removes its qdisc on
    EXIT, INT, or TERM. Only the tc setup/cleanup remains privileged.

`apply` persists until `clear`; prefer `run` for tests with automatic cleanup.
On a non-loopback interface this is egress-only. Test both endpoints if both
traffic directions need impairment.
EOF
}

fail() {
    printf 'microphone-netem: %s\n' "$*" >&2
    exit 1
}

need_linux_tc() {
    [[ "$(uname -s)" == "Linux" ]] || fail "Linux is required"
    command -v tc >/dev/null 2>&1 || fail "tc is required (install iproute2)"
    [[ -d "/sys/class/net/${INTERFACE}" ]] || fail "interface does not exist: ${INTERFACE}"
}

require_explicit_sudo() {
    [[ "${EUID}" -eq 0 && -n "${SUDO_USER:-}" ]] \
        || fail "invoke this mutating command explicitly with sudo; the script will not elevate itself"
}

scenario_args() {
    case "${SCENARIO}" in
        mild) NETEM_ARGS=(delay 10ms 3ms distribution normal loss 0.2%) ;;
        wifi) NETEM_ARGS=(delay 35ms 12ms distribution normal loss 1% reorder 0.2% 50%) ;;
        congested) NETEM_ARGS=(delay 80ms 25ms distribution normal loss 3% rate 256kbit) ;;
        outage) NETEM_ARGS=(loss 100%) ;;
        *) fail "unknown scenario '${SCENARIO}'; expected mild, wifi, congested, or outage" ;;
    esac
}

parse_ports() {
    IFS=',' read -r -a PORTS <<<"${PORTS_CSV}"
    [[ ${#PORTS[@]} -gt 0 ]] || fail "at least one UDP port is required"
    local port
    for port in "${PORTS[@]}"; do
        [[ "${port}" =~ ^[0-9]+$ ]] || fail "invalid UDP port: ${port}"
        (( port >= 1 && port <= 65535 )) || fail "UDP port out of range: ${port}"
    done
}

state_file() {
    local safe_interface="${INTERFACE//[^a-zA-Z0-9_.-]/_}"
    printf '%s/%s.state\n' "${STATE_ROOT}" "${safe_interface}"
}

show_plan() {
    scenario_args
    parse_ports
    printf 'Interface: %s (egress)\nScenario: %s\nUDP destination ports: %s\n' \
        "${INTERFACE}" "${SCENARIO}" "${PORTS_CSV}"
    printf 'Netem arguments:'
    printf ' %q' "${NETEM_ARGS[@]}"
    printf '\n\nCommands that apply/run would execute:\n'
    printf '  tc qdisc replace dev %q root handle 1: prio bands 3\n' "${INTERFACE}"
    printf '  tc qdisc add dev %q parent 1:3 handle 30: netem' "${INTERFACE}"
    printf ' %q' "${NETEM_ARGS[@]}"
    printf '\n'
    local port
    for port in "${PORTS[@]}"; do
        printf '  tc filter add dev %q protocol ip parent 1: prio 10 u32 match ip protocol 17 0xff match ip dport %q 0xffff flowid 1:3\n' \
            "${INTERFACE}" "${port}"
    done
}

install_qdisc() {
    require_explicit_sudo
    scenario_args
    parse_ports
    local state
    state="$(state_file)"
    [[ ! -e "${state}" ]] || fail "managed state already exists at ${state}; clear it first"

    local existing
    existing="$(tc qdisc show dev "${INTERFACE}")"
    if grep -q ' root ' <<<"${existing}" && ! grep -q '^qdisc noqueue 0: root' <<<"${existing}"; then
        printf '%s\n' "${existing}" >&2
        fail "refusing to replace an existing non-noqueue root qdisc"
    fi

    mkdir -p -m 0755 "${STATE_ROOT}"
    local installed=0
    rollback_partial() {
        if [[ "${installed}" -eq 1 ]]; then
            tc qdisc del dev "${INTERFACE}" root >/dev/null 2>&1 || true
        fi
        rm -f -- "${state}"
    }
    trap rollback_partial ERR INT TERM

    tc qdisc replace dev "${INTERFACE}" root handle 1: prio bands 3
    installed=1
    tc qdisc add dev "${INTERFACE}" parent 1:3 handle 30: netem "${NETEM_ARGS[@]}"
    local port
    for port in "${PORTS[@]}"; do
        tc filter add dev "${INTERFACE}" protocol ip parent 1: prio 10 u32 \
            match ip protocol 17 0xff \
            match ip dport "${port}" 0xffff \
            flowid 1:3
    done
    umask 077
    cat >"${state}" <<EOF
owner=noland-microphone-netem-v1
interface=${INTERFACE}
scenario=${SCENARIO}
ports=${PORTS_CSV}
sudo_user=${SUDO_USER}
EOF
    trap - ERR INT TERM
    printf 'microphone-netem: applied %s to %s UDP dports %s\n' \
        "${SCENARIO}" "${INTERFACE}" "${PORTS_CSV}" >&2
    printf 'microphone-netem: cleanup with: sudo %q clear --interface %q\n' "$0" "${INTERFACE}" >&2
}

remove_qdisc() {
    require_explicit_sudo
    local state
    state="$(state_file)"
    [[ -f "${state}" ]] || fail "no managed netem state for ${INTERFACE} at ${state}"
    grep -qx 'owner=noland-microphone-netem-v1' "${state}" \
        || fail "state file is not owned by this tool: ${state}"
    grep -qx "interface=${INTERFACE}" "${state}" \
        || fail "state file interface does not match ${INTERFACE}"

    local current
    current="$(tc qdisc show dev "${INTERFACE}")"
    grep -q '^qdisc prio 1: root' <<<"${current}" \
        || fail "current root qdisc is not this tool's prio handle; refusing to delete it"
    grep -q '^qdisc netem 30: parent 1:3' <<<"${current}" \
        || fail "managed netem child handle is absent; refusing ambiguous cleanup"
    tc qdisc del dev "${INTERFACE}" root
    rm -f -- "${state}"
    printf 'microphone-netem: cleared managed qdisc from %s\n' "${INTERFACE}" >&2
}

run_as_sudo_user() {
    command -v runuser >/dev/null 2>&1 \
        || fail "runuser is required so the test command does not run as root"
    local user_id
    user_id="$(id -u "${SUDO_USER}")"
    local runtime_dir="/run/user/${user_id}"
    runuser -u "${SUDO_USER}" -- env \
        "XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-${runtime_dir}}" \
        "DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-unix:path=${runtime_dir}/bus}" \
        "${RUN_COMMAND[@]}"
}

show_status() {
    printf '%s\n' "$(tc qdisc show dev "${INTERFACE}")"
    local state
    state="$(state_file)"
    if [[ -f "${state}" ]]; then
        printf '\nManaged state (%s):\n' "${state}"
        cat "${state}"
    else
        printf '\nNo managed state for %s.\n' "${INTERFACE}"
    fi
}

if [[ "${COMMAND}" == "apply" || "${COMMAND}" == "plan" || "${COMMAND}" == "run" ]]; then
    [[ $# -gt 0 ]] || fail "${COMMAND} requires a scenario"
    SCENARIO="$1"
    shift
fi

while [[ $# -gt 0 ]]; do
    case "$1" in
        --interface) [[ $# -ge 2 ]] || fail "--interface requires a value"; INTERFACE="$2"; shift 2 ;;
        --ports) [[ $# -ge 2 ]] || fail "--ports requires a value"; PORTS_CSV="$2"; shift 2 ;;
        --)
            shift
            RUN_COMMAND=("$@")
            break
            ;;
        -h|--help) usage; exit 0 ;;
        *) fail "unknown argument: $1" ;;
    esac
done

case "${COMMAND}" in
    help|-h|--help) usage ;;
    plan)
        need_linux_tc
        show_plan
        ;;
    status)
        need_linux_tc
        show_status
        ;;
    apply)
        need_linux_tc
        install_qdisc
        ;;
    clear)
        need_linux_tc
        remove_qdisc
        ;;
    run)
        need_linux_tc
        [[ ${#RUN_COMMAND[@]} -gt 0 ]] || fail "run requires -- COMMAND..."
        install_qdisc
        trap 'remove_qdisc' EXIT
        trap 'exit 130' INT
        trap 'exit 143' TERM
        run_as_sudo_user
        ;;
    *) fail "unknown command: ${COMMAND}" ;;
esac
