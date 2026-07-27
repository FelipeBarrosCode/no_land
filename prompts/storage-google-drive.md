You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up your own Google Drive OAuth app

Since Noland does not use a central OAuth service, you create your own Google Cloud project to connect your Drive. This takes about 5 minutes and is completely free.

### Step 1: Create a Google Cloud project
1. Go to https://console.cloud.google.com
2. Click the project dropdown at the top → **New Project**
3. Name it "Noland Storage" and click Create
4. Wait for the project to be created, then select it

### Step 2: Enable the Drive API
1. In the sidebar, go to **APIs & Services** → **Library**
2. Search for "Google Drive API"
3. Click **Enable**

### Step 3: Configure the OAuth consent screen
1. Go to **APIs & Services** → **OAuth consent screen**
2. Choose **External** (unless you are in a Google Workspace org)
3. Fill in:
   - App name: "Noland Storage"
   - User support email: your email
   - Developer contact: your email
4. Click **Save and Continue**
5. On the Scopes page, click **Add or Remove Scopes** and add:
   - `https://www.googleapis.com/auth/drive.file` (create and manage Noland's own files)
   - `https://www.googleapis.com/auth/userinfo.email` (see your email)
6. Click **Save and Continue**
7. Add your email as a test user (required since the app is in testing mode)
8. Click **Save and Continue**

### Step 4: Create OAuth credentials
1. Go to **APIs & Services** → **Credentials**
2. Click **Create Credentials** → **OAuth client ID**
3. Application type: **Desktop app**
4. Name: "Noland Connect"
5. Click **Create**
6. Copy the **Client ID** and **Client Secret** shown in the popup

### Step 5: Add the redirect URI
1. Click the pencil icon next to your new OAuth client
2. Under **Authorized redirect URIs**, add:
   - `http://127.0.0.1:17770/callback`
3. Click **Save**

### Step 6: Connect in Noland
1. Open Noland → Settings → Shared Storage → Google Drive
2. Click **Configure Google Drive**
3. Enter your **Client ID** and **Client Secret**
4. Click **Continue with Google Drive** to authorize
5. Complete the sign-in in your browser
6. Return to Noland and click **Complete Authorization**

✅ Your Noland backups will be stored in a dedicated "Noland Shared Storage" folder in your Drive. Only you can access it — Noland uses your own OAuth app.
