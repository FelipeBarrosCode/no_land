# Noland Connect - AI Explanation Prompts

This folder contains the complete collection of AI prompts used across the Noland Connect platform to explain various components, features, and settings. Copy any of these prompts and paste them into your AI provider (like ChatGPT, Claude, Gemini, etc.) to get an in-depth, technical explanation of what that section does and how it functions.

---

## 1. Embedded Streaming Card (Panel 1)
**Topic:** Noland embedded streaming client
```text
Act as a cloud gaming specialist. Explain how Noland's embedded Moonlight-compatible client decodes the remote stream inside the app and why no external Moonlight installation is required.
```

---

## 2. Managed Tunnel Card (Panel 2 Top)
**Topic:** Embedded GotaTun connection
```text
Act as a network security specialist. Explain how Noland's embedded GotaTun engine automatically creates and manages its WireGuard-compatible tunnel on Linux, macOS, and Windows without external VPN apps or command-line tools.
```

---

## 3. Legacy Tailscale Prompt
**Topic:** Retired connection flow
```text
Act as a cloud gaming assistant. Explain that Tailscale belongs to a retired Noland connection flow and current builds use the embedded GotaTun tunnel without a Tailscale account, API key, or local app.
```

---

## 4. Set Server Card (Panel 3)
**Topic:** Set Server offering
```text
Act as a cloud brokerage coordinator. Explain what the Set Server dashboard panel is for, why discovering GPU offers is crucial for cloud gaming, and how choosing an offer sets my server preferences before launching.
```

---

## 5. Rented Servers Section
**Topic:** Rented Servers list
```text
Act as a virtualization systems engineer. Explain the Rented Servers dashboard module, how it monitors running GPU cloud instances, what states like Ready, Starting, and Paused mean, and how this section lets me perform administrative tasks on my active leases.
```

---

## 6. Selected Server Section
**Topic:** Selected Server specs
```text
Act as a server hardware configuration specialist. Explain the Selected Server overview card, what specifications like location distance, reliability, storage size, price per hour, and GPU models mean, and why I should review these specs before clicking Play.
```

---

## 7. Play Button Card
**Topic:** Play Execution
```text
Act as an automated orchestration developer. Explain that Play starts provisioning, Sunshine setup, the embedded GotaTun tunnel, connectivity checks, in-app pairing, and streaming automatically.
```

---

## 8. WireGuard Connection Modal
**Topic:** WireGuard Modal info
```text
Act as a VPN protocol architect. Explain that Noland automatically creates, activates, monitors, and repairs its WireGuard-compatible tunnel with embedded GotaTun; the user never downloads or imports a profile.
```

---

## 9. Tailscale Connection Modal
**Topic:** Tailscale Modal info
```text
Act as a cloud gaming assistant. Explain that this is a legacy prompt and current Noland builds do not require Tailscale because the managed GotaTun tunnel is included.
```

---

## 10. Select Server Modal - Main Header
**Topic:** Server Picker Market Header
```text
Act as a server procurement advisor. Explain the Select Server Market modal header, how it displays available GPU offers in real time, and how leasing a machine directly from this panel provisions my remote streaming server.
```

---

## 11. Select Server Modal - Country and Full-Text Search
**Topic:** Offer search
```text
Act as a cloud gaming assistant. Explain that the server picker only searches by country and then provides full-text filtering across all fields in the returned offers, including state, city, GPU, CPU, host, price, reliability, network speed, and offer type.
```

---

## 12. Select Server Modal - Offer Cards
**Topic:** Instance Offer Card specs
```text
Act as a hardware benchmarking specialist. Explain how to read an individual Instance Offer card in the picker modal, what host labels, AVX support, remaining runtime hours, download/upload speeds, reliability scores, and VRAM sizes tell me about the machine's streaming suitability.
```

---

## 13. Help Guide Onboarding steps

### Step 1: Open the app
```text
Act as a cloud gaming tutor. Explain Step 1 (Open the app) of the onboarding guide: how Noland Connect acts as an arcade control deck to bridge my local machine with remote GPU gaming hardware.
```

### Step 2: Open Vast.ai
```text
Act as a cloud hardware broker. Explain Step 2 (Open Vast.ai) of the guide: what the Vast.ai GPU marketplace is, and why it is the provider of our remote streaming servers.
```

### Step 3: Create account
```text
Act as an authentication guide. Explain Step 3 (Create your Vast.ai account) of the onboarding: how account registration sets up my credential profiles and server leasing access.
```

### Step 4: Billing Setup
```text
Act as a cloud billing consultant. Explain Step 4 (Add billing) of the onboarding: why adding billing details to the hardware provider is necessary to pay for active GPU compute hours.
```

### Step 5: Vast API Key
```text
Act as an API key manager. Explain Step 5 (Get your API key) of the onboarding: what a Vast.ai API key is and how Noland Connect uses it to automate server search and creation.
```

### Step 6: Embedded streaming client
```text
Act as a streaming protocol engineer. Explain that Noland includes its Moonlight-compatible client and requires no external Moonlight installation.
```

### Step 7: Managed secure connection
```text
Act as a virtual network architect. Explain that GotaTun is embedded and no WireGuard, Tailscale, or networking-tool installation is required.
```

### Step 8: Automatic tunnel setup
```text
Act as a cloud gaming assistant. Explain that Noland generates, starts, and verifies its secure tunnel without a VPN account or API key.
```

### Step 9: Server Selection
```text
Act as a server quality analyst. Explain Step 9 (Select a server) of the onboarding: how to weigh GPU performance, location latency, and hourly price.
```

### Step 10: Automatic connection
```text
Act as a cloud gaming assistant. Explain that provisioning and the embedded tunnel start when Play is clicked, with only an operating-system elevation approval potentially required.
```

### Step 11: Setup Verification
```text
Act as a systems pairing technician. Explain Noland's in-app pairing exchange with Sunshine and make clear that no external Moonlight app is required.
```

### Step 12: Host Password
```text
Act as a server administrator. Explain the Final Step (Computer password) of the onboarding: why the remote streaming desktop has a default login password of 'password' and how to enter it.
```

---

## 14. Entire Settings Page Explanation (Major Prompt)
**Topic:** Noland Settings configuration
```text
Act as a DevOps cloud architect. Explain the entire Settings panel in detail:
1. Vast.ai API Key: Configures automated GPU orchestrations.
2. Managed Connection: Uses embedded GotaTun automatically without VPN-provider settings.
3. Server Filters: Declares minimum requirements for GPU RAM, system reliability, storage sizes, and templates.
4. Streaming Preferences: Controls bitrate, resolution, frame rate, and codecs for Noland's embedded client.
5. SSH Credentials: Manages remote console key access.
6. Shared Storage Settings: Configures state synchronization and backup scripts.
Provide a complete walkthrough of how each setting influences Noland's automation engine and local streaming client.
```
