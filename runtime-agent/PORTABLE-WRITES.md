# Writable application storage

Newly registered native ELF apps and extracted AppImages get a **fully writable,
persistent private application directory** automatically. No per-game save/cache
whitelist is necessary. Apps can create, change, rename and remove their own
assets, helpers, configuration and executable files (normal Linux restrictions
on overwriting a currently executing inode still apply).

The first launch imports the complete application directory into
`~/.local/share/noland-runtime/workspaces/APP_ID/view`, mounted read/write at
`/app`. On compatible Linux filesystems, imports use reflink copy-on-write
clones; otherwise they stream verified copies. Hard links to originals are
never used. A fallback import can require space for the entire application.
Every copied file is SHA256-verified and flushed, directories and receipts are
flushed, and publication is atomic. Special files and escaping/cyclic symlinks
are rejected without publishing a partial workspace. Internal relative
symlinks, including AppImage `AppRun` links, are preserved. File execution bits
are retained, owner write access is granted, and privileged mode bits, ownership
and capabilities are not imported.

Subsequent launches reuse private data without rescanning or reseeding the
original. Self-updates, new files, deleted caches and save progress persist;
removing or changing the original package does not replace those private files.
Prerequisite inspection uses the actual private executable/libraries once
imported. A missing/damaged private executable fails with a diagnostic, never
an automatic reset. Receipts stay outside the application mount.

Per-app HOME and XDG config/data/cache locations remain writable and persistent.
The **shared Linux image, host system, drivers and original packages do not
become writable**. System libraries still use the existing explicitly approved
runtime maintenance workflow. There are no new CPU/GPU limits, host packages,
FUSE services or required bubblewrap overlay features. This is compatibility
for trusted applications, **not a security sandbox**.

## Existing applications

Missing `writableApp` fields retain the previous policy; re-registering an
existing path does not silently change it. To import an existing application:

```sh
noland-runtime configure-writes --app-id APP_ID --all --approve
noland-runtime launch --app-id APP_ID
```

If the app already has a selected-path private view, migration imports its
**current private saves and new files**, preserves selected-path deletions and
copies remaining assets from the original. The old view and original files are
left intact. A snapshot commitment detects the crash gap between workspace
publication and the app-record switch: newer legacy progress is never hidden by
silently adopting a stale copy. Changed/ambiguous migrations fail closed and
retain both copies. Full mode cannot be silently downgraded to selected paths.

Selected-path migrations also publish `workspaces/APP_ID/legacy-import.json`
before the workspace receipt. Its exact bytes are SHA256-pinned by the receipt's
`legacyImportSha256`. The proof records the unchanged legacy manifest/root and
only the private entries actually imported, not executable placeholders or
immutable asset aliases. Proofs are limited to 16 MiB and 65,536 entries. This
lets backup reconciliation distinguish exact historical paths from new or
unmigrated data without deleting the old view or changing application identity.

For a migration completed by an older runtime without this evidence, close the
app and explicitly repair its records:

```sh
noland-runtime reconcile-records --app-id APP_ID --approve
```

Repair validates the existing legacy manifest and repeats the original import
inventory against the saved snapshot. Both original and legacy data must still
match; newer saves in the fully writable workspace do not prevent repair.
Existing proof/pins are never refreshed to accept later legacy changes. Normal
launch or `configure-writes --all` reuse does not create missing evidence.
Registration, private HOME, current workspace bytes and all old data stay intact.

## Legacy selected-path views

The previous explicit policy remains available for old read-only records:

```sh
noland-runtime configure-writes --app-id APP_ID \
  --writable-path user --writable-path cache.bin --approve
noland-runtime launch --app-id APP_ID
```

Select every existing top-level save/cache entry that must be writable. Names
are literal, single relative entries; nested paths, symlinks, duplicates and the
executable are rejected. Missing entries are allowed for future cache creation.
AppImages are not supported by this legacy selected-path opt-in; use full mode.

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

Configuration and supervisors share a per-app exclusive lock. Close an already
running old-launcher instance before first configuring; old binaries do not hold
this new lock. New supervisors hold it until bwrap exits. Reconfiguration with
the identical selection and relaunch never reseed or overwrite private data.
Changed source identities/layout (including nested seeded entries), modified
asset aliases, or different selections fail closed and preserve private data.
There is intentionally no automatic refresh or reset.
Failed unpublished seeds are removed; existing published views are never deleted.

Doctor and AppImage extraction retain their original read-only source paths.
