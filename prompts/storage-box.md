You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up your own Box OAuth app

Create your own Box custom app to connect your account. This takes about 5 minutes and requires a Box account (free or paid).

### Step 1: Create a Box app
1. Go to https://developer.box.com
2. Click **My Apps** in the top navigation
3. Click **Create New App** → **Custom App**
4. Fill in:
   - App Name: "Noland Storage"
   - Purpose: **Other**
   - Authentication Method: **OAuth 2.0 with PKCE**
5. Click **Create App**

### Step 2: Configure the app
1. In your app's configuration page, scroll to **OAuth 2.0 Credentials**
2. Copy the **Client ID** — you will paste this into Noland
3. Copy the **Client Secret** — paste into Noland
4. Under **Redirect URIs**, add:
   - `http://127.0.0.1:17770/callback`
5. Click **Save Changes**

### Step 3: Configure application scopes
1. Scroll to **Application Scopes**
2. Enable these:
   - **Read all files and folders stored in Box**
   - **Write all files and folders stored in Box**
3. (Optional) If you want Noland to create a dedicated folder, also enable **Manage users** scopes
4. Click **Save Changes**

### Step 4: Authorize the app
1. At the top of the configuration page, find the **Authorization** section
2. Click **Review and Submit**
3. Confirm the scopes and submit

### Step 5: Connect in Noland
1. Open Noland → Settings → Shared Storage → Box
2. Click **Configure Box**
3. Enter your **Client ID** and **Client Secret**
4. Click **Continue with Box** to authorize
5. Sign in and grant permission in your browser
6. Return to Noland and click **Complete Authorization**

✅ Your backups are stored in Box with enterprise-grade encryption. Free accounts have 10 GB. Business plans offer unlimited storage. Data is encrypted before upload.
