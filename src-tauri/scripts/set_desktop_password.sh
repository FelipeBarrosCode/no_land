#!/bin/sh
# Password records arrive only over stdin. Never enable tracing or echo input.
set +x
set -eu
target="$1"
uid=$(id -u -- "$target" 2>/dev/null) || exit 41
[ "$uid" -gt 0 ] || exit 41
command -v chpasswd >/dev/null 2>&1 || exit 42
command -v passwd >/dev/null 2>&1 || exit 42
# PAM diagnostics can contain sensitive data; expose only an exit category.
chpasswd >/dev/null 2>&1 || exit 43
status=$(passwd -S "$target" 2>/dev/null) || exit 44
printf '%s\n' "$status" | grep -Eq '^[^[:space:]]+[[:space:]]+P([[:space:]]|$)' || exit 44
printf '%s\n' NOLAND_DESKTOP_PASSWORD_SET
