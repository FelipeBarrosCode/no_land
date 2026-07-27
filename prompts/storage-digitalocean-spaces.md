You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up DigitalOcean Spaces

1. **Log in to DigitalOcean** at https://cloud.digitalocean.com
2. **Create a Space**:
   - Go to **Spaces** → **Create Space**
   - Choose a datacenter region (e.g., `nyc3`, `sfo3`, `ams3`)
   - Enable **Restrict File Listing**
   - Name it `noland-backups`
3. **Create an Access Key**:
   - Go to **API** → **Spaces Keys**
   - Generate a new key
   - Copy the **Access Key** and **Secret Key**
4. In Noland, paste the Access Key, Secret Key, select your region, and enter your Space name

✅ DigitalOcean Spaces charges $5/month for 250 GB storage + 1 TB outbound transfer. Good for moderate-sized game libraries.
