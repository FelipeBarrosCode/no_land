# Release signature verification

## Windows: Authenticode and trusted timestamps

The protected `Secrets` environment supplies `AZURE_TENANT_ID`,
`AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `TRUSTED_SIGNING_ENDPOINT` and
`TRUSTED_SIGNING_ACCOUNT_NAME`. No private certificate material is downloaded.
The optional `TRUSTED_SIGNING_CERTIFICATE_PROFILE_NAME` repository variable
selects a profile; the existing `Noland` profile remains the default.

Both x64 and ARM64 releases:

1. Build without packaging, then sign the application and project-owned network
   and microphone helper executables through Azure Artifact Signing.
2. Verify Authenticode validity and the timestamp certificate, restore the signed
   files, and bundle with `--no-binary-patching` so Tauri cannot invalidate them.
   The current Windows distribution uses one NSIS bundle type; it does not need
   the binary-patching feature for differentiating multiple installer types.
3. Sign and verify the completed installers, then regenerate updater signatures
   over those final bytes. The existing installer smoke test also checks the
   installed application and both project sidecars.

PowerShell verification for a downloaded installer:

```powershell
$signature = Get-AuthenticodeSignature -LiteralPath .\Noland-setup.exe
$signature | Format-List Status, SignerCertificate, TimeStamperCertificate
```

`Status` must be `Valid`, with a signer and timestamp certificate. Updater `.sig`
files are separate cryptographic signatures; they are not Authenticode.

## Linux: keyless signatures and transparency proofs

Linux has no macOS-style notarization service. This repository has no configured
OpenPGP private signing key. Instead, releases use GitHub's Sigstore-backed
artifact attestations: a short-lived OIDC identity signs an attestation binding
the exact SHA-256 digest of every curated file, including each `.deb`, `.rpm`,
`.AppImage`, Arch archive, SBOM and `SHA256SUMS`, to this repository, release
workflow, source ref and commit. Public attestations carry transparency evidence.
This needs no additional long-lived signing secret.

Each release includes `provenance.sigstore.jsonl`. Verify a downloaded package:

```sh
gh attestation verify Noland-package.AppImage \
  --repo FelipeBarrosCode/no_land \
  --signer-workflow FelipeBarrosCode/no_land/.github/workflows/release.yml \
  --source-ref refs/heads/main \
  --deny-self-hosted-runners \
  --bundle provenance.sigstore.jsonl
```

Add `--source-digest <full-release-commit>` to pin a specific release commit.
Verify `SHA256SUMS` the same way before using `sha256sum --check SHA256SUMS`.
The bundle is deliberately not listed in the manifest it authenticates, avoiding
a circular digest. Its own signature is cryptographically verified by the CLI.

CI checks every manifest hash and attestation before uploading the release set,
and repeats verification after downloading it immediately before tagging and
publication. Extra unsigned files, digest mismatches, invalid proofs, unexpected
workflow identity or missing Linux package formats block publication.

These are **Sigstore provenance signatures**, not embedded RPM GPG signatures or
an APT repository `Release.gpg` trust root. Native RPM/APT GPG trust additionally
requires a deliberately provisioned, stable OpenPGP signing key and published
fingerprint. Do not generate a new key per build or commit a private key. Tauri's
existing AppImage updater signatures remain enabled independently.

Trial builds from a recovery branch are signed with that branch's source ref;
the production verification command above intentionally rejects them as `main`
releases. Physical-device smoke testing remains separate from package signing.
