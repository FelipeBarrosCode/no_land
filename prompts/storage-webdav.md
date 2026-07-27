You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up WebDAV Storage

1. Ensure your WebDAV server is running and accessible
2. Collect the following:
   - **URL** — the full WebDAV endpoint URL
   - **Vendor** — Nextcloud, ownCloud, or Other
   - **Username** — your WebDAV user
   - **Password** — an app-specific password (NOT your main account password!)

### Nextcloud setup
1. Log in to your Nextcloud instance
2. Go to **Settings** → **Security** → **Devices & sessions**
3. Create an **app password** named "Noland"
4. Your WebDAV URL is: `https://your-nextcloud.com/remote.php/dav/files/YOUR_USERNAME/`
5. In Noland, select **Nextcloud** as vendor, paste the URL, username, and app password

### ownCloud setup
1. Same process as Nextcloud
2. WebDAV URL: `https://your-owncloud.com/remote.php/dav/files/YOUR_USERNAME/`

### Other WebDAV
1. Use your server's WebDAV endpoint URL
2. Create a dedicated app password if your server supports it

✅ Your application data is stored on your own infrastructure. No third-party cloud provider costs. Ensure your server has sufficient storage and reliable connectivity.
