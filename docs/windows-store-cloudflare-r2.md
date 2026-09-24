# Windows Store installer hosting via Cloudflare R2

This project can publish the Microsoft Store desktop installer to Cloudflare R2 during the `release` job.

## What CI uploads

The release pipeline uploads the curated Windows Store installer asset:

- `Noland.Connect_<version>_x64-store-setup.exe`

The object key format is:

- `no_land/<release-tag>/Noland.Connect_<version>_x64-store-setup.exe`

Example:

- `no_land/v0.0.2/Noland.Connect_0.1.0_x64-store-setup.exe`

## Required Cloudflare setup

1. Create an R2 bucket.
   - Suggested bucket name: `no-land-downloads`
2. Attach a public custom domain to that bucket.
   - Example public base URL: `https://downloads.noland.app`
3. Create a Cloudflare API token with access to that bucket.
   - Minimum recommended permissions:
     - `Account` → `Workers R2 Storage: Edit`
     - `Account` → `Account Settings: Read`
4. Copy your Cloudflare account ID.

## Required GitHub Actions secrets

Add these secrets to the `Secrets` environment used by `Code/noland/no_land/.github/workflows/release.yml`:

- `CLOUDFLARE_API_TOKEN`
- `CLOUDFLARE_ACCOUNT_ID`
- `CLOUDFLARE_R2_BUCKET`
- `CLOUDFLARE_R2_PUBLIC_BASE_URL`
- `RELEASE_PAT`

Example values:

- `CLOUDFLARE_R2_BUCKET=no-land-downloads`
- `CLOUDFLARE_R2_PUBLIC_BASE_URL=https://downloads.noland.app`

`RELEASE_PAT` should be a GitHub token that can:

- push tags for commits that modify workflow files
- create/update GitHub releases

A classic PAT with `repo` and `workflow` scopes works. A fine-grained token also works if it has repository contents write access and workflow permission for this repository.

## Resulting Partner Center URL

When the secrets are configured, the release job generates a Windows Store package URL like:

- `https://downloads.noland.app/no_land/v0.0.2/Noland.Connect_0.1.0_x64-store-setup.exe`

The release job also publishes helper files in the GitHub release:

- `windows-store-x64-package-url.txt`
- `windows-store-x64-object-key.txt`
- `windows-store-x64-submission.json`
- `windows-store-x64-submission.md`

Use the generated package URL in Microsoft Partner Center.

## Azure Artifact Signing

Windows installers are Authenticode-signed after the architecture-specific
builds complete. Both x64 and ARM64 installers are signed from a supported x64
Windows runner, verified with `Get-AuthenticodeSignature`, and then have their
Tauri updater signatures regenerated before publication.

The signing job uses these Artifact Signing resources:

- Endpoint: `https://eus.codesigning.azure.net/`
- Signing account: `Noland`
- Certificate profile: `Noland`
- Timestamp service: `http://timestamp.acs.microsoft.com`

Configure these secrets on the GitHub `Secrets` environment:

- `AZURE_CLIENT_ID`
- `AZURE_TENANT_ID`
- `AZURE_SUBSCRIPTION_ID`

The Entra service principal must have the **Artifact Signing Certificate
Profile Signer** role on the `Noland` certificate profile. Its GitHub OIDC
federated credential must allow this environment subject:

`repo:FelipeBarrosCode/no_land:environment:Secrets`

No Azure client secret or exported signing certificate is used. Unsigned
Windows build artifacts are staged under an `unsigned-` artifact name, which
the release job deliberately excludes.
