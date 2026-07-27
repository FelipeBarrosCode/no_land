You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up Wasabi

1. **Sign up for Wasabi** at https://wasabi.com
2. **Create a bucket** in the Wasabi Console:
   - Go to **Buckets** → **Create Bucket**
   - Choose a region (e.g., `us-east-1`, `eu-central-1`)
   - Name it `noland-backups`
3. **Create an Access Key**:
   - Go to **Access Keys** → **Create New Access Key**
   - Copy the **Access Key** and **Secret Key**
4. In Noland, paste the Access Key, Secret Key, select your region, and enter the bucket name

✅ Wasabi charges $6.99/TB/month with **no egress fees** and no API request charges. Good alternative to S3 for frequent restores.
