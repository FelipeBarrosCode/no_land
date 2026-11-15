# Opt-in native-app save/cache writes

Existing records and unconfigured apps still mount their entire source directory
read-only at `/app`. No host/image update or CPU/GPU settings are involved.

For a native ELF application that writes saves/caches beside its executable:

```sh
noland-runtime configure-writes --app-id APP_ID \
  --writable-path user --writable-path cache.bin --approve
noland-runtime launch --app-id APP_ID
```

Select **every existing top-level save/cache entry** that must be writable. Names
are literal, single relative entries; nested paths, symlinks, duplicates and the
executable are rejected. Missing entries are allowed for future cache creation.
AppImages are not supported by this opt-in.

The JSON response reports `policy`, `writablePaths` and `dataRoot`. The latter is
`~/.local/share/noland-runtime/portable/APP_ID/view`, outside the app's HOME.
Initial configuration copies only selected existing entries, streaming SHA256
verification and fsync before atomic publication. Originals are never written.
Insufficient space or unsupported source entries fail configuration.

At launch, original assets are read-only at `/home/runtime/.noland-source`.
This mountpoint is precreated in private per-app HOME storage, not the runtime
image; no new directory in the image root is required. Unselected entries in
the private `/app` view alias those assets. The executable is a read-only file
bind at its original `/app/NAME`, not a symlink, preserving `/proc/self/exe` lookup.
The private working root is writable so new cache filenames can be created
without altering originals. Existing asset aliases still resolve read-only.
The runtime image remains read-only; HOME retains its existing per-app binding.
This is portability for trusted apps, **not a security sandbox**.

Configuration and supervisors share a per-app exclusive lock. Close an already
running old-launcher instance before first configuring; old binaries do not hold
this new lock. New supervisors hold it until bwrap exits. Reconfiguration with
the identical selection and relaunch never reseed or overwrite private data.
Changed source identities/layout (including nested seeded entries), modified
asset aliases, or different selections fail closed and preserve private data.
There is intentionally no automatic refresh/reset/migration command.
Failed unpublished seeds are removed; existing published views are never deleted.

Doctor and AppImage extraction retain their original read-only paths.
