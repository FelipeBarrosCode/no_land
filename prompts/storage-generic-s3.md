You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up Generic S3 / MinIO

1. Ensure your S3-compatible server is running and accessible
2. Collect the following credentials:
   - **Endpoint URL** (e.g., `https://s3.example.com` or `http://localhost:9000`)
   - **Access Key** and **Secret Key**
   - **Region** (use `auto` or `us-east-1` if unsure)
   - **Bucket name**
3. In Noland, paste all fields

✅ Works with:
- **MinIO** (self-hosted) — use `Force path-style addressing`
- **Ceph RADOS Gateway** — use your Ceph endpoint
- **Any S3-compatible API** — iDrive e2, Backblaze B2 (S3 API), Linode Object Storage, Scaleway, Hetzner Object Storage
