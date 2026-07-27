You are a helpful assistant. The goal is to guide me on how to set up this cloud provider with the correct information onto my Noland.

## Setting up your own Microsoft OneDrive OAuth app

Create your own Microsoft Entra ID (Azure AD) app registration to connect OneDrive. This takes about 5 minutes and requires a Microsoft account.

### Step 1: Register an application in Azure
1. Go to https://portal.azure.com → **Microsoft Entra ID** (or search "App registrations")
2. Click **App registrations** → **New registration**
3. Fill in:
   - Name: "Noland Storage"
   - Supported account types: **Accounts in any organizational directory and personal Microsoft accounts**
   - Redirect URI: Choose **Web** and enter `http://127.0.0.1:17770/callback`
4. Click **Register**

### Step 2: Copy your credentials
1. After registration, you will see the Overview page
2. Copy the **Application (client) ID** — this is your Client ID
3. Go to **Certificates & secrets** → **New client secret**
4. Description: "Noland Connect"
5. Expires: 24 months (recommended)
6. Click **Add**
7. Copy the **Value** immediately — this is your Client Secret (it won't be shown again)

### Step 3: Configure API permissions
1. Go to **API permissions** → **Add a permission**
2. Select **Microsoft Graph** → **Delegated permissions**
3. Add these permissions:
   - `Files.ReadWrite.AppFolder` — read/write Noland's own folder
   - `User.Read` — sign in and read your profile
   - `offline_access` — refresh tokens when you are not using the app
4. Click **Add permissions**
5. (Optional) Click **Grant admin consent** if this is a work/school account

### Step 4: Connect in Noland
1. Open Noland → Settings → Shared Storage → OneDrive
2. Click **Configure OneDrive**
3. Enter your **Client ID** and **Client Secret**
4. Select your drive type (Personal, Business, or SharePoint)
5. Click **Continue with OneDrive** to authorize
6. Sign in with your Microsoft account and grant permissions
7. Return to Noland and click **Complete Authorization**

✅ Your data is encrypted before upload. Noland uses an app-specific folder in your OneDrive.
