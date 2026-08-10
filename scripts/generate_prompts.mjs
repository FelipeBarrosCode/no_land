import fs from 'fs';
import path from 'path';

const prompts = {
  "moonlight-card": `Act as a cloud gaming specialist. Explain how Noland's embedded Moonlight-compatible client decodes the remote stream inside the app and why no external Moonlight installation is required.`,
  "wireguard-card": `Act as a network security specialist. Explain how Noland's embedded GotaTun engine automatically creates and manages its WireGuard-compatible tunnel on Linux, macOS, and Windows without external VPN apps or command-line tools.`,
  "tailscale-card": `Act as a cloud gaming assistant. Explain that Tailscale belongs to a retired Noland connection flow and current builds use the embedded GotaTun tunnel without a Tailscale account, API key, or local app.`,
  "set-server-card": `Act as a cloud brokerage coordinator. Explain what the Set Server dashboard panel is for, why discovering GPU offers is crucial for cloud gaming, and how choosing an offer sets my server preferences before launching.`,
  "rented-servers-section": `Act as a virtualization systems engineer. Explain the Rented Servers dashboard module, how it monitors running GPU cloud instances, what states like Ready, Starting, and Paused mean, and how this section lets me perform administrative tasks on my active leases.`,
  "selected-server-section": `Act as a server hardware configuration specialist. Explain the Selected Server overview card, what specifications like location distance, reliability, storage size, price per hour, and GPU models mean, and why I should review these specs before clicking Play.`,
  "play-button-section": `Act as an automated orchestration developer. Explain that Play starts provisioning, Sunshine setup, the embedded GotaTun tunnel, connectivity checks, in-app pairing, and streaming automatically.`,
  "wireguard-modal-info": `Act as a VPN protocol architect. Explain that Noland automatically creates, activates, monitors, and repairs its WireGuard-compatible tunnel with embedded GotaTun; the user never downloads or imports a profile.`,
  "tailscale-modal-info": `Act as a cloud gaming assistant. Explain that this is a legacy prompt and current Noland builds do not require Tailscale because the managed GotaTun tunnel is included.`,
  "server-picker-modal-header": `Act as a server procurement advisor. Explain the Select Server Market modal header, how it displays available GPU offers in real time, and how leasing a machine directly from this panel provisions my remote streaming server.`,
  "server-search-preferences": `Act as a cloud gaming assistant. Explain that the server picker only searches by country and then provides full-text filtering across all fields in the returned offers, including state, city, GPU, CPU, host, price, reliability, network speed, and offer type.`,
  "server-instance-card": `Act as a hardware benchmarking specialist. Explain how to read an individual Instance Offer card in the picker modal, what host labels, AVX support, remaining runtime hours, download/upload speeds, reliability scores, and VRAM sizes tell me about the machine's streaming suitability.`,
  "help-step-1": `Act as an arcade streaming tutor. Explain Step 1 (Open the app) of the onboarding guide: how Noland Connect acts as a control deck to bridge my local machine with remote GPU gaming hardware.`,
  "help-step-2": `Act as a cloud hardware broker. Explain Step 2 (Open Vast.ai) of the guide: what the Vast.ai GPU marketplace is, and why it is the provider of our remote streaming servers.`,
  "help-step-3": `Act as an authentication guide. Explain Step 3 (Create your Vast.ai account) of the onboarding: how account registration sets up my credential profiles and server leasing access.`,
  "help-step-4": `Act as a cloud billing consultant. Explain Step 4 (Add billing) of the onboarding: why adding billing details to the hardware provider is necessary to pay for active GPU compute hours.`,
  "help-step-5": `Act as an API key manager. Explain Step 5 (Get your API key) of the onboarding: what a Vast.ai API key is and how Noland Connect uses it to automate server search and creation.`,
  "help-step-6": `Act as a streaming protocol engineer. Explain Step 6 (Use the embedded streaming client): Noland includes its Moonlight-compatible client and requires no external Moonlight installation.`,
  "help-step-7": `Act as a virtual network architect. Explain Step 7 (Approve the secure connection): GotaTun is embedded and no WireGuard, Tailscale, or networking-tool installation is required.`,
  "help-step-8": `Act as a cloud gaming assistant. Explain Step 8 (Automatic tunnel setup): Noland generates, starts, and verifies the secure tunnel without a VPN account or API key.`,
  "help-step-9": `Act as a server quality analyst. Explain Step 9 (Select a server) of the onboarding: how to weigh GPU performance, location latency, and hourly price.`,
  "help-step-10": `Act as a cloud gaming assistant. Explain Step 10 (Automatic connection): provisioning and the embedded tunnel start when Play is clicked, with only an operating-system elevation approval potentially required.`,
  "help-step-11": `Act as a systems pairing technician. Explain Step 11 (Complete in-app pairing): Noland's embedded client pairs with Sunshine without an external Moonlight app.`,
  "help-step-12": `Act as a server administrator. Explain the Final Step (Computer password) of the onboarding: why the remote streaming desktop has a default login password of 'password' and how to enter it.`,
  "settings-page": `Act as a DevOps cloud architect. Explain the entire Settings panel in detail:
1. Vast.ai API Key: Configures automated GPU orchestrations.
2. Managed Connection: Uses embedded GotaTun automatically without VPN-provider settings.
3. Server Filters: Declares minimum requirements for GPU RAM, system reliability, storage sizes, and templates.
4. Streaming Preferences: Controls bitrate, resolution, frame rate, and codecs for Noland's embedded client.
5. SSH Credentials: Manages remote console key access.
6. Shared Storage Settings: Configures state synchronization and backup scripts.
Provide a complete walkthrough of how each setting influences Noland's automation engine and local streaming client.`
};

const promptsDir = path.join(process.cwd(), 'prompts');

if (!fs.existsSync(promptsDir)) {
  fs.mkdirSync(promptsDir, { recursive: true });
}

for (const [filename, content] of Object.entries(prompts)) {
  fs.writeFileSync(path.join(promptsDir, `${filename}.md`), content.trim());
}

console.log('Successfully created ' + Object.keys(prompts).length + ' prompt files.');
