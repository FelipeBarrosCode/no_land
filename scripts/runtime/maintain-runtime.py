#!/usr/bin/python3 -I
"""Fixed-function runtime package maintenance. Never accepts an app path/command."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import uuid

IMAGES = Path('/opt/noland/runtime/images')
CONFIG = Path('/etc/noland-runtime')
PROFILE = 'ubuntu24-x86_64'
ENV = {'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LANG': 'C.UTF-8',
       'DEBIAN_FRONTEND': 'noninteractive', 'HOME': '/root'}


def arguments(args, catalog):
    if len(args) < 2 or args[0] not in ('update', 'install'):
        raise ValueError('Expected update/install and a provisioned image ID')
    action, image, *packages = args
    if not re.fullmatch(r'ubuntu24-[a-zA-Z0-9_-]{1,80}', image):
        raise ValueError('Invalid system runtime image ID')
    allowed = set(catalog['libraries'].values())
    if catalog.get('schema') != 1 or len(packages) > 64:
        raise ValueError('Unsupported prerequisite request')
    if action == 'update' and packages or action == 'install' and not packages:
        raise ValueError('Invalid package selection')
    if any(not re.fullmatch(r'[a-z0-9][a-z0-9+.-]*', p) or p not in allowed
           for p in packages):
        raise ValueError('Package is not in the approved runtime catalog')
    return action, image, sorted(set(packages))


def protected(path, directory=False):
    info = path.lstat()
    if (info.st_uid != 0 or info.st_mode & 0o022 or
            not (stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode))):
        raise RuntimeError('Runtime maintenance requires protected root-owned paths')


def sync_directory(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def atomic_json(path, value):
    temporary = path.with_name('.maintenance-' + uuid.uuid4().hex)
    try:
        with temporary.open('x') as output:
            os.chmod(temporary, 0o644)
            json.dump(value, output, sort_keys=True)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        sync_directory(path.parent)
    finally:
        temporary.unlink(missing_ok=True)


def run(args, log, capture=False, timeout=1800):
    # Own the complete subprocess group, including dpkg children, until stopped.
    process = subprocess.Popen(args, env=ENV, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE if capture else log,
                               stderr=log, start_new_session=True)
    try:
        output, _ = process.communicate(timeout=timeout)
    except BaseException:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
        raise
    if process.returncode:
        raise RuntimeError('Runtime package operation failed; previous image preserved')
    return output or b''


def inventory(image, log):
    return run(['/usr/sbin/chroot', str(image), '/usr/bin/dpkg-query', '-W',
                '-f=${binary:Package}\t${Version}\n'], log, capture=True)


def validate_image(image, log):
    text = (image / 'etc/os-release').read_text()
    if 'ID=ubuntu' not in text.splitlines() or 'VERSION_ID="24.04"' not in text.splitlines():
        raise RuntimeError('Only Ubuntu 24.04 runtime images are supported')
    for entry in ['usr/bin/env', 'usr/bin/dbus-run-session', 'usr/bin/glxinfo',
                  'usr/bin/vulkaninfo', 'usr/bin/glxgears', 'usr/bin/pactl',
                  'usr/local/bin/noland-glibc-probe']:
        if not (image / entry).is_file():
            raise RuntimeError('Updated runtime is missing a required component')
    # This is a fixed, root-provisioned probe, never the dropped executable.
    run(['/usr/sbin/chroot', str(image), '/usr/local/bin/noland-glibc-probe'], log)


def maintain(action, image_id, packages, log):
    if not re.fullmatch(r'ubuntu24-[a-zA-Z0-9_-]{1,80}', image_id):
        raise ValueError('Invalid system runtime image ID')
    for path in [Path('/opt'), Path('/opt/noland'), IMAGES.parent, IMAGES, CONFIG]:
        protected(path, directory=True)
    source = IMAGES / image_id
    protected(source, directory=True)
    protected(source / 'etc/noland-runtime-complete')
    validate_image(source, log)
    # Copy-on-write where supported; otherwise budget an independent image copy.
    size = int(run(['/usr/bin/du', '-sx', '--block-size=1', str(source)], log,
                   capture=True).split()[0])
    if shutil.disk_usage(IMAGES).free < size + 1024 * 1024 * 1024:
        raise RuntimeError('Insufficient free storage for an inactive image and package downloads')
    stage = Path(tempfile.mkdtemp(prefix='.maintenance-', dir=IMAGES))
    try:
        before = inventory(source, log)
        run(['/bin/cp', '-a', '--reflink=auto', '--one-file-system',
             str(source) + '/.', str(stage)], log)
        # Do not use caller environment, app repositories, app scripts or PPAs.
        sources = stage / 'etc/apt/sources.list.d/noland-maintenance.sources'
        sources.write_text('Types: deb\nURIs: https://archive.ubuntu.com/ubuntu\n'
                           'Suites: noble noble-updates\nComponents: main universe restricted multiverse\n'
                           'Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n\n'
                           'Types: deb\nURIs: https://security.ubuntu.com/ubuntu\n'
                           'Suites: noble-security\nComponents: main universe restricted multiverse\n'
                           'Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n')
        resolver = stage / 'etc/resolv.conf'
        resolver.unlink(missing_ok=True)
        shutil.copyfile('/etc/resolv.conf', resolver)
        policy = stage / 'usr/sbin/policy-rc.d'
        policy.write_text('#!/bin/sh\nexit 101\n')
        policy.chmod(0o755)
        apt = ['/usr/sbin/chroot', str(stage), '/usr/bin/apt-get',
               '-o', 'Dir::Etc::sourcelist=/etc/apt/sources.list.d/noland-maintenance.sources',
               '-o', 'Dir::Etc::sourceparts=-', '-o', 'Acquire::Retries=3',
               '-o', 'Acquire::http::Timeout=30', '-o', 'Acquire::https::Timeout=30']
        run(apt + ['update'], log)
        if action == 'update':
            run(apt + ['upgrade', '-y', '--with-new-pkgs', '--no-remove'], log)
        else:
            run(apt + ['install', '-y', '--no-install-recommends', '--no-remove', '--'] + packages, log)
        after = inventory(stage, log)
        validate_image(stage, log)
        if before == after:
            return {'imageId': image_id, 'changed': False}
        run(apt + ['clean'], log)
        new_id = 'ubuntu24-update-' + uuid.uuid4().hex
        digest = hashlib.sha256(after).hexdigest()
        (stage / 'etc/noland-runtime-complete').write_text(digest + '\n')
        atomic_json(stage / 'etc/noland-runtime-maintenance.json', {
            'profile': PROFILE, 'id': new_id, 'previousId': image_id,
            'packagesSha256': digest, 'action': action, 'packages': packages})
        # Flush the staged image before publishing its immutable identity.
        run(['/bin/sync', '-f', str(stage)], log)
        stage.chmod(0o755)
        destination = IMAGES / new_id
        os.rename(stage, destination)
        sync_directory(IMAGES)
        atomic_json(CONFIG / 'image.json', {'id': new_id, 'profile': PROFILE,
                                           'previousId': image_id, 'packagesSha256': digest})
        return {'imageId': new_id, 'changed': True}
    finally:
        # Only this invocation's inactive stage is eligible for cleanup.
        if stage.exists():
            shutil.rmtree(stage)


def main():
    if os.geteuid() != 0:
        raise RuntimeError('This fixed helper must be called through sudo')
    os.umask(0o077)
    catalog = CONFIG / 'prerequisites.json'
    protected(catalog)
    action, image, packages = arguments(sys.argv[1:], json.loads(catalog.read_text()))
    fd = os.open('/var/lock/noland-runtime-install.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'a+') as lock:
        deadline = time.monotonic() + 1800
        while True:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    raise RuntimeError('Another runtime maintenance operation is active')
                time.sleep(0.2)
        fd = os.open('/var/log/noland-runtime-maintenance.log',
                     os.O_APPEND | os.O_CREAT | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'ab') as log:
            result = maintain(action, image, packages, log)
        print(json.dumps(result))


if __name__ == '__main__':
    try:
        main()
    except Exception as error:
        print('Runtime maintenance failed: ' + str(error), file=sys.stderr)
        sys.exit(1)
