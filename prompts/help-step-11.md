## Configure Shared Storage for backups

Shared Storage keeps your applications, saves, settings, and mods available across instances.

### Connecting a provider

1. Go to **Settings → Shared Storage**
2. Click **"Connect Storage Provider"**
3. Choose a provider from the list:

**Object Storage** (pay per GB, no egress fees on R2/Wasabi):
- Backblaze B2 — $6/TB/month, S3-compatible
- Amazon S3 — standard cloud storage
- Cloudflare R2 — zero egress fees
- Wasabi — $7/TB/month, no egress
- DigitalOcean Spaces — $5/month for 250 GB

**Cloud Drives** (use your existing account):
- Google Drive, OneDrive, Dropbox, Box

**Enterprise / Self-hosted:**
- Azure Blob, Google Cloud Storage, SFTP, WebDAV

4. Click the robot icon (🤖) next to any provider for a detailed setup guide
5. For OAuth providers (Google, Microsoft, Dropbox, Box), click Continue to authorize in your browser
6. For API key providers (S3, B2, R2), enter your credentials

### After connecting
- Test your connection to verify everything works
- Your repository ID is shown on the connected state
- All application data is encrypted before upload with your repository master key
