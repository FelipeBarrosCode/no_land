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

Example values:

- `CLOUDFLARE_R2_BUCKET=no-land-downloads`
- `CLOUDFLARE_R2_PUBLIC_BASE_URL=https://downloads.noland.app`

## Resulting Partner Center URL

When the secrets are configured, the release job generates a Windows Store package URL like:

- `https://downloads.noland.app/no_land/v0.0.2/Noland.Connect_0.1.0_x64-store-setup.exe`

The release job also publishes helper files in the GitHub release:

- `windows-store-x64-package-url.txt`
- `windows-store-x64-object-key.txt`
- `windows-store-x64-submission.json`
- `windows-store-x64-submission.md`

Use the generated package URL in Microsoft Partner Center.
