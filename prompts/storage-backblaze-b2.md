You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up Backblaze B2

1. **Create a Backblaze account** at https://www.backblaze.com
2. **Create a bucket** in the B2 Cloud Storage section:
   - Go to **Buckets** → **Create a Bucket**
   - Name it `noland-backups` (or any name you prefer)
   - Set it to **Private**
3. **Create an Application Key**:
   - Go to **App Keys** → **Add a New Application Key**
   - Give it a name like `noland-storage`
   - Select your bucket from the dropdown
   - Set access to **Read and Write**
   - Copy the **keyID** and **applicationKey** — you'll paste them in Noland
4. In Noland, paste the Key ID and Application Key, then enter your bucket name

✅ Your files will be stored in Backblaze B2 with server-side encryption. Costs are typically $6/TB/month.
