You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up your own Dropbox OAuth app

Create your own Dropbox app to connect your account. This takes about 3 minutes and requires a Dropbox account.

### Step 1: Create a Dropbox app
1. Go to https://www.dropbox.com/developers/apps
2. Click **Create app**
3. Choose:
   - API: **Scoped access**
   - Type of access: **App folder** (Noland only sees its own folder, not your other files)
   - Name your app: "Noland Storage"
4. Click **Create app**

### Step 2: Configure permissions
1. In your app's settings page, go to the **Permissions** tab
2. Enable these scopes:
   - `files.metadata.read` — see file info
   - `files.content.read` — download files (for restore)
   - `files.content.write` — upload files (for backup)
3. Click **Submit**

### Step 3: Get your credentials
1. Go to the **Settings** tab
2. Copy the **App key** — this is your Client ID
3. Copy the **App secret** — this is your Client Secret
4. Under **OAuth 2** → **Redirect URIs**, add:
   - `http://127.0.0.1:17770/callback`
5. Click **Add**

### Step 4: Connect in Noland
1. Open Noland → Settings → Shared Storage → Dropbox
2. Click **Configure Dropbox**
3. Enter your **App key** (Client ID) and **App secret** (Client Secret)
4. Click **Continue with Dropbox** to authorize
5. Sign in and grant permission in your browser
6. Return to Noland and click **Complete Authorization**

✅ Your backups are stored in **Apps → Noland Storage** in your Dropbox. The app cannot access anything else in your Dropbox. Free accounts have 2 GB — upgrading to Plus (2 TB) is recommended for game backups.
