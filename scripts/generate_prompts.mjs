import fs from 'fs';
import path from 'path';

const prompts = {
  "moonlight-card": `Act as a cloud gaming specialist. Explain why the Moonlight Game Streaming download button is placed on the main dashboard, how this client-side app decodes high-bitrate video streams, and why installing it locally is the first step to connecting to remote server hosts.`,
  "wireguard-card": `Act as a network security specialist. Explain what the WireGuard connection option on the dashboard means, why it is described as a bare-bones protocol, and what the manual setup process entails before starting my cloud gaming session.`,
  "tailscale-card": `Act as a mesh networking engineer. Explain what the Tailscale connection card on the dashboard signifies, why it is promoted as a quicker configuration-free alternative, and how setting it up streamlines the initial connection phase.`,
  "set-server-card": `Act as a cloud brokerage coordinator. Explain what the Set Server dashboard panel is for, why discovering GPU offers is crucial for cloud gaming, and how choosing an offer sets my server preferences before launching.`,
  "rented-servers-section": `Act as a virtualization systems engineer. Explain the Rented Servers dashboard module, how it monitors running GPU cloud instances, what states like Ready, Starting, and Paused mean, and how this section lets me perform administrative tasks on my active leases.`,
  "selected-server-section": `Act as a server hardware configuration specialist. Explain the Selected Server overview card, what specifications like location distance, reliability, storage size, price per hour, and GPU models mean, and why I should review these specs before clicking Play.`,
  "play-button-section": `Act as an automated orchestration developer. Explain the Play button panel, what actions are triggered when starting a session, and how Noland Connect provisions Sunshine, registers SSH keys, pairs Moonlight, and displays connection guidance.`,
  "wireguard-modal-info": `Act as a VPN protocol architect. Detail the WireGuard Connection Info popup: how point-to-point tunnels work, how cryptographic key-pairs secure the link, why I must manually import the generated .conf profile, and how it differs from public mesh networks.`,
  "tailscale-modal-info": `Act as a Software-Defined Network (SDN) engineer. Detail the Tailscale Connection Info popup: how virtual private mesh networks operate, why it simplifies remote node discovery, and how adding my API key automatically joins the remote instance to my tailnet.`,
  "server-picker-modal-header": `Act as a server procurement advisor. Explain the Select Server Market modal header, how it displays available GPU offers in real time, and how leasing a machine directly from this panel provisions my remote streaming server.`,
  "server-search-preferences": `Act as a database search systems engineer. Explain the Search Filters component inside the server selection modal, what parameters like region codes, minimum/maximum price limits, and verified-only checks do, and how adjusting storage in GB affects deployment.`,
  "server-instance-card": `Act as a hardware benchmarking specialist. Explain how to read an individual Instance Offer card in the picker modal, what host labels, AVX support, remaining runtime hours, download/upload speeds, reliability scores, and VRAM sizes tell me about the machine's streaming suitability.`,
  "help-step-1": `Act as an arcade streaming tutor. Explain Step 1 (Open the app) of the onboarding guide: how Noland Connect acts as a control deck to bridge my local machine with remote GPU gaming hardware.`,
  "help-step-2": `Act as a cloud hardware broker. Explain Step 2 (Open Vast.ai) of the guide: what the Vast.ai GPU marketplace is, and why it is the provider of our remote streaming servers.`,
  "help-step-3": `Act as an authentication guide. Explain Step 3 (Create your Vast.ai account) of the onboarding: how account registration sets up my credential profiles and server leasing access.`,
  "help-step-4": `Act as a cloud billing consultant. Explain Step 4 (Add billing) of the onboarding: why adding billing details to the hardware provider is necessary to pay for active GPU compute hours.`,
  "help-step-5": `Act as an API key manager. Explain Step 5 (Get your API key) of the onboarding: what a Vast.ai API key is and how Noland Connect uses it to automate server search and creation.`,
  "help-step-6": `Act as a streaming protocol engineer. Explain Step 6 (Download Moonlight) of the onboarding: what Moonlight is and why its low-latency game streaming is selected for this setup.`,
  "help-step-7": `Act as a virtual network architect. Explain Step 7 (Install your connection app) of the onboarding: the differences between WireGuard and Tailscale VPN clients.`,
  "help-step-8": `Act as a credentials specialist. Explain Step 8 (Get a Tailscale API key) of the onboarding: why adding a Tailscale API key unlocks a config-free mesh VPN connection.`,
  "help-step-9": `Act as a server quality analyst. Explain Step 9 (Select a server) of the onboarding: how to weigh GPU performance, location latency, and hourly price.`,
  "help-step-10": `Act as a VPN support representative. Explain Step 10 (Connect when asked) of the onboarding: how to proceed when Noland prompts to hand off WireGuard files or connect via Tailscale.`,
  "help-step-11": `Act as a systems pairing technician. Explain Step 11 (Follow the instructions) of the onboarding: the pairing exchange where Moonlight links with Sunshine via a security PIN.`,
  "help-step-12": `Act as a server administrator. Explain the Final Step (Computer password) of the onboarding: why the remote streaming desktop has a default login password of 'password' and how to enter it.`,
  "settings-page": `Act as a DevOps cloud architect. Explain the entire Settings panel in detail:
1. Vast.ai API Key: Configures automated GPU orchestrations.
2. Tailscale API Key: Authenticates mesh network VPN configurations.
3. Connection Provider: Switches between WireGuard peer tunnels and Tailscale networks.
4. Server Filters: Declares minimum requirements for GPU RAM, system reliability, storage sizes, and templates.
5. Moonlight Preferences: Controls streaming quality (bitrate, resolution, frame rate, and codecs).
6. SSH Credentials: Manages remote console key access.
7. Shared Storage Settings: Configures state synchronization and backup scripts.
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
