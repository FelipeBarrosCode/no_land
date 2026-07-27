You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up Google Cloud Storage

1. **Go to Google Cloud Console** at https://console.cloud.google.com
2. **Create a project** (or select an existing one)
3. **Create a bucket**:
   - Go to **Cloud Storage** → **Buckets** → **Create**
   - Choose a location type and storage class (Standard recommended)
   - Name it `noland-backups`
4. **Create a Service Account**:
   - Go to **IAM & Admin** → **Service Accounts** → **Create Service Account**
   - Assign the **Storage Object Admin** role
   - Create a JSON key and download it
5. In Noland, paste the **Service Account JSON** and enter your bucket name

✅ GCS Standard storage is $0.02/GB/month. Use object lifecycle policies to auto-transition older backups to Nearline or Archive for cost savings.
