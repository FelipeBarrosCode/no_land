# Noland Connect Documentation

This folder is the source of truth for how Noland Connect works end-to-end.

## Start Here

- `docs/architecture.md`: system layout, module boundaries, and runtime model
- `docs/flows.md`: onboarding, provisioning, post-WireGuard setup, reboot, lifecycle, backup, mic
- `docs/shared-storage-high-level.md`: high-level shared storage flow, diagrams, and tools/components used
- `docs/schemas.md`: persisted state schema, event schema, and key data contracts
- `docs/api-reference.md`: frontend/backend command surface and grouping
- `docs/configuration.md`: environment variables, defaults, and tuning knobs
- `docs/operations.md`: build/release workflows, release artifacts, and operational runbook
- `docs/noland-connect-system.md`: full production-grade deep system documentation

## Related project docs outside this folder

- `README.md`: repo setup, stack, and top-level project overview
- `PROVISIONING_STEPS.md`: provisioning checklist notes
- `KVM_GOLDEN_IMAGE_SETUP.md`: VM image preparation notes

If a behavior changes in code, update the relevant file in this folder in the same PR.
