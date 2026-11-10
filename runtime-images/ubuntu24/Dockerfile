# CI must supply an immutable Ubuntu 24.04 digest and an Ubuntu archive snapshot.
ARG UBUNTU_IMAGE
FROM ${UBUNTU_IMAGE} AS probe-build
ARG APT_SNAPSHOT
RUN test -n "$APT_SNAPSHOT" \
    && sed -i "s|http://archive.ubuntu.com/ubuntu/|http://snapshot.ubuntu.com/ubuntu/${APT_SNAPSHOT}/|g; s|http://security.ubuntu.com/ubuntu/|http://snapshot.ubuntu.com/ubuntu/${APT_SNAPSHOT}/|g" /etc/apt/sources.list.d/ubuntu.sources \
    && printf 'Acquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/99snapshot \
    && apt-get update && apt-get install -y --no-install-recommends gcc libc6-dev
COPY glibc-probe.c /probe.c
RUN gcc -O0 -fno-builtin -Wall -Werror /probe.c -o /noland-glibc-probe

FROM ${UBUNTU_IMAGE}
ARG APT_SNAPSHOT
ENV DEBIAN_FRONTEND=noninteractive
COPY --from=probe-build /noland-glibc-probe /usr/local/bin/noland-glibc-probe
COPY packages.txt /usr/share/noland-runtime/packages.txt
RUN test -n "$APT_SNAPSHOT" \
    && sed -i "s|http://archive.ubuntu.com/ubuntu/|http://snapshot.ubuntu.com/ubuntu/${APT_SNAPSHOT}/|g; s|http://security.ubuntu.com/ubuntu/|http://snapshot.ubuntu.com/ubuntu/${APT_SNAPSHOT}/|g" /etc/apt/sources.list.d/ubuntu.sources \
    && printf 'Acquire::Check-Valid-Until "false";\n' > /etc/apt/apt.conf.d/99snapshot \
    && apt-get update \
    && grep -v '^#' /usr/share/noland-runtime/packages.txt | xargs apt-get install -y --no-install-recommends \
    && mkdir -p /usr/share/noland-runtime/licenses /app /home/runtime /opt/noland-driver /run/noland /tmp/.X11-unix /dev/shm \
    && dpkg-query -W -f='${Package}\t${Version}\t${Architecture}\n' > /usr/share/noland-runtime/package-versions.tsv \
    && cp -a /usr/share/doc/. /usr/share/noland-runtime/licenses/ \
    && rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/* \
    && printf 'noland:x:1000:1000:Runtime:/home/runtime:/bin/sh\n' >> /etc/passwd \
    && printf 'noland:x:1000:\n' >> /etc/group \
    && chown 1000:1000 /home/runtime
USER 1000:1000
# No systemd PID 1, Xorg, Sunshine, audio server, DKMS, Steam or 32-bit profile.
