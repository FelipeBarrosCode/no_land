You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up Azure Blob Storage

1. **Go to Azure Portal** at https://portal.azure.com
2. **Create a Storage Account**:
   - Go to **Storage accounts** → **Create**
   - Choose a resource group and region
   - Performance: **Standard**
   - Redundancy: **LRS** (lowest cost) or **GRS** (geo-redundant)
3. **Create a Container** inside the storage account:
   - Go to the storage account → **Containers** → **+ Container**
   - Name it `noland-backups`
   - Access level: **Private**
4. **Get the Access Key**:
   - In the storage account, go to **Security + networking** → **Access keys**
   - Copy the **Storage account name** and one of the **keys**
5. In Noland, paste the Account Name, Account Key, container name, and optional prefix

✅ Azure Blob Hot tier is ~$0.018/GB/month. Use lifecycle management to auto-move older backups to Cool or Archive tiers. All data is encrypted at rest.
