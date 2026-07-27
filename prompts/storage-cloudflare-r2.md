You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up Cloudflare R2

1. **Log in to Cloudflare Dashboard** at https://dash.cloudflare.com
2. **Create an R2 bucket**:
   - Go to **R2** → **Create bucket**
   - Name it `noland-backups`
3. **Create an API Token**:
   - Go to **R2** → **Manage R2 API Tokens**
   - Create a token with **Object Read & Write** permission
   - Select your bucket in the permission scope
   - Copy the **Access Key ID** and **Secret Access Key**
4. Find your **Account ID** at the top of the R2 overview page
5. In Noland, paste the Account ID, Access Key ID, Secret Access Key, and bucket name

✅ R2 has **zero egress fees** — you only pay for storage ($0.015/GB/month). Perfect for frequent restores.
