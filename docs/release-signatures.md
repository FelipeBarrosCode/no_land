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

## Linux: OpenPGP signatures and transparency proofs

Linux has no macOS-style notarization service. Releases use both detached OpenPGP
signatures and GitHub's Sigstore-backed artifact attestations.

### OpenPGP package signatures

The protected `Secrets` environment supplies `LINUX_GPG_PRIVATE_KEY` and
`LINUX_GPG_PASSPHRASE`. The release script requires this independently pinned
primary signing-key fingerprint:

```text
C30A DA95 9FA2 5A24 5A1E  902D 7EA0 DDFF 6AD7 7028
```

Each `.AppImage`, `.deb`, `.rpm`, and `.pkg.tar.zst` receives a detached ASCII
armored `.asc` signature. `SHA256SUMS` covers the final packages, package
signatures, public key, SBOM, and other release files, and has its own detached
`SHA256SUMS.asc` signature. The manifest cannot include its own signature or the
later Sigstore bundle without creating a circular dependency.

The public key is published as `linux-signing-key.asc`. Compare its fingerprint
against the independently published value above before trusting it:

```sh
gpg --show-keys --with-fingerprint linux-signing-key.asc
gpg --import linux-signing-key.asc
gpg --verify Noland-package.deb.asc Noland-package.deb
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --check SHA256SUMS
```

CI imports the private key into a temporary, restricted keyring, supplies the
passphrase through stdin rather than process arguments, and removes the keyring
and its agent afterward. No private key or passphrase is printed. Verification
uses a fresh public-only keyring, checks the pinned fingerprint, verifies every
Linux signature and the signed manifest, and blocks publication on any failure.
The publication job requires only the public key; it receives no GPG secrets.

These are detached package signatures, **not embedded RPM header signatures or
an APT repository `Release.gpg` trust root**. No APT repository is published by
this workflow. Existing Tauri updater signatures remain enabled independently.

### Sigstore transparency proofs

A short-lived OIDC identity signs an attestation binding
the exact SHA-256 digest of every curated file, including each `.deb`, `.rpm`,
`.AppImage`, Arch archive, SBOM and `SHA256SUMS`, to this repository, release
workflow, source ref and commit. Public attestations carry transparency evidence.
Sigstore needs no additional long-lived signing secret.

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
The bundle and `SHA256SUMS.asc` are deliberately not listed in the manifest,
avoiding circular digests. Both are verified cryptographically; Sigstore also
attests `SHA256SUMS.asc` along with every other release file.

CI checks every manifest hash and attestation before uploading the release set,
and repeats verification after downloading it immediately before tagging and
publication. Extra unsigned files, digest mismatches, invalid proofs, unexpected
workflow identity or missing Linux package formats block publication.

Trial builds from a recovery branch are signed with that branch's source ref;
the production verification command above intentionally rejects them as `main`
releases. Physical-device smoke testing remains separate from package signing.
