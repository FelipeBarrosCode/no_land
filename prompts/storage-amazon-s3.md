You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up Amazon S3

1. **Log in to AWS Console** at https://console.aws.amazon.com
2. **Create an S3 bucket**:
   - Go to **S3** → **Create bucket**
   - Choose a region close to your instances
   - Enable **Block all public access**
3. **Create an IAM user** with programmatic access:
   - Go to **IAM** → **Users** → **Create user**
   - Attach the `AmazonS3FullAccess` policy (or create a custom policy scoped to your bucket)
   - Under **Security credentials**, create an **Access Key**
   - Copy the **Access Key ID** and **Secret Access Key**
4. In Noland, paste the Access Key ID, Secret Access Key, region code (`us-east-1`, `us-west-2`, etc.), and bucket name

✅ Your data is stored in S3 Standard class by default. For lower cost, create a lifecycle rule to transition to S3 Standard-IA or Glacier.
