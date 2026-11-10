# AppImage catalog entry: root permissions

Catalog submission: https://github.com/AppImage/appimage.github.io/pull/7478

The last catalog check tested v0.1.23 and reported `AppRun: Permission denied`.
Its own log showed that AppRun was already executable (`0755`) and AppDir lint
had passed. Changing only AppRun's executable bit cannot fix this case.

A read-only HTTPS range inspection of the published SquashFS metadata confirmed
the same defect in both v0.1.23 and v0.1.27 (the latest release on Nov 10, 2026):

| Entry | Owner | Mode |
| --- | --- | --- |
| SquashFS root directory | root:root | **0700** |
| AppRun | root:root | 0755 |
| .DirIcon | root:root | 0644, valid PNG |

The filesystem starts at byte 944632 in both x86_64 AppImages. The root inode in
each archive explicitly stores mode 0700; this was not inferred from an extracted
directory or a FUSE mount. No application binary was executed during inspection.

The FUSE runtime can mask this directory-permission problem. Firejail mounts the
SquashFS through the kernel, where a non-root process cannot traverse a directory
owned by root with mode 0700. The same distinction is documented in
https://github.com/netblue30/firejail/issues/2690.

## Packaging fix

`scripts/fix-appimage-metadata.sh` now:

- Sets the AppDir root and AppRun to 0755 without a recursive 0777 permission change.
- Explicitly passes `-root-mode 755` to mksquashfs via the pinned appimagetool.
- Checks both modes again after extracting the repacked candidate.
- Replaces the original artifact only after all metadata/permission checks pass.

Offline regression fixtures start with mode 0700, exercise both architectures,
and reject a repacker that reintroduces restrictive root or launcher permissions.
They do not compile or launch the application or download real packaging tools.

## Catalog status

This source fix does not alter already-published v0.1.27 assets. A corrected,
properly signed AppImage must be published before requesting a catalog retest.
Do not overwrite existing signed artifacts or weaken Firejail's sandbox to hide
the problem. The catalog entry already points to the correct repository URL;
the fix belongs in the application's packaging, not the catalog data file.
