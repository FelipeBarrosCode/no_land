import fs from 'fs';
import path from 'path';

const contextString = "Context: The user is currently using 'Noland' (or Noland Connect), a platform that automates the process of renting cloud GPUs (via Vast.ai) and seamlessly connecting them for high-end remote game streaming. ";

const prompts = {
  "moonlight-card": `Act as a helpful cloud gaming assistant. Explain that Noland Connect includes its own embedded Moonlight-compatible streaming client, so no external Moonlight installation is required. Describe how the in-app stream provides a low-latency, high-quality experience and give practical guidance on resolution, bitrate, and frame-rate settings.`,

  "wireguard-card": `Act as a helpful cloud gaming assistant. Explain that Noland automatically creates a WireGuard-compatible connection with its embedded GotaTun engine on Linux, macOS, and Windows. Make clear that no WireGuard or GotaTun installation, command-line tool, downloaded configuration, or manual profile import is required; the user may only need to approve an operating-system elevation prompt.`,

  "tailscale-card": `Act as a helpful cloud gaming assistant. Explain that Tailscale belongs to a legacy Noland connection flow and that current builds use the embedded GotaTun engine instead. Do not instruct the user to create a Tailscale account, provide an API key, or install a VPN app.`,

  "set-server-card": `Act as a helpful cloud gaming assistant. Explain to the user how to properly search for and set their preferred server for cloud gaming. Walk them through what server specs to prioritize (like location and GPU) and how choosing the right server directly impacts their streaming latency and graphical quality.`,

  "rented-servers-section": `Act as a helpful cloud gaming assistant. Explain to the user how to manage their rented servers in Noland Connect. Describe what actions they should take when a server is listed as 'Ready' or 'Starting', and how they can use this section to quickly jump into or stop their gaming sessions.`,

  "selected-server-section": `Act as a helpful cloud gaming assistant. Explain to the user how to review their selected server's specifications. Walk them through which details to verify (such as price, location, and GPU) before they hit Play, and how this final check ensures they get the gaming experience they expect.`,

  "play-button-section": `Act as a helpful cloud gaming assistant. Explain that clicking Play immediately starts server provisioning, Sunshine setup, the embedded secure tunnel, connectivity checks, in-app pairing, and stream preparation. The user may need to approve an operating-system permission prompt, but should not install or activate an external VPN or streaming app.`,

  "wireguard-modal-info": `Act as a helpful cloud gaming assistant. Explain that Noland's embedded GotaTun engine automatically creates, activates, monitors, and repairs the WireGuard-compatible tunnel. Tell the user to approve an operating-system elevation prompt if shown, and never instruct them to download a configuration or open a separate WireGuard app.`,

  "tailscale-modal-info": `Act as a helpful cloud gaming assistant. Explain that this is a legacy connection prompt and that current Noland builds do not need Tailscale, a Tailscale API key, or a locally installed VPN client because the managed GotaTun tunnel is built in.`,

  "server-picker-modal-header": `Act as a helpful cloud gaming assistant. Explain to the user how to navigate the Select Server Market. Walk them through how to browse the available GPU offers and how their selections here will impact both their hourly costs and their gaming performance.`,

  "server-search-preferences": `Act as a helpful cloud gaming assistant. Explain the server picker's two controls: country fetches that country's Vast.ai offers, while full-text search filters all fields of the returned page, such as state, city, GPU, CPU, host, price, reliability, network speed, and offer type.`,

  "server-instance-card": `Act as a helpful cloud gaming assistant. Explain to the user how to evaluate an individual server offer card. Tell them which metrics matter most (like internet speed, reliability, and VRAM) and how those numbers will impact their actual gameplay experience.`,

  "help-step-1": `Act as a helpful cloud gaming assistant. Explain Step 1 (Open the app) of the Noland Connect onboarding guide. Tell the user what the Noland app does, how to use it, and how it serves as the control center for their game streaming experience.`,

  "help-step-2": `Act as a helpful cloud gaming assistant. Explain Step 2 (Open Vast.ai) of the onboarding guide. Walk the user through what Vast.ai is, why they need to visit it, and how it provides the raw server power for their gaming sessions.`,

  "help-step-3": `Act as a helpful cloud gaming assistant. Explain Step 3 (Create your Vast.ai account) of the onboarding guide. Provide clear instructions on how to register and explain why having this account is necessary to start renting servers.`,

  "help-step-4": `Act as a helpful cloud gaming assistant. Explain Step 4 (Add billing) of the onboarding guide. Walk the user through how to add billing details on Vast.ai, and explain how the pay-as-you-go hourly pricing impacts their costs.`,

  "help-step-5": `Act as a helpful cloud gaming assistant. Explain Step 5 (Get your API key) of the onboarding guide. Tell the user exactly where to find their Vast.ai API key, how to paste it into Noland, and how this automates the entire server renting process.`,

  "help-step-6": `Act as a helpful cloud gaming assistant. Explain Step 6 (Use the embedded streaming client) of the onboarding guide. Make clear that Noland includes a Moonlight-compatible client and handles Sunshine pairing and streaming in the app without an external Moonlight installation.`,

  "help-step-7": `Act as a helpful cloud gaming assistant. Explain Step 7 (Approve the secure connection) of the onboarding guide. Tell the user that the GotaTun connection engine is included and that they do not install WireGuard, GotaTun, Tailscale, or networking command-line tools; they may only need to approve elevation for the managed adapter.`,

  "help-step-8": `Act as a helpful cloud gaming assistant. Explain Step 8 (Automatic tunnel setup) of the onboarding guide. Tell the user that Noland generates, starts, and verifies its embedded GotaTun tunnel automatically and does not require a VPN account or API key.`,

  "help-step-9": `Act as a helpful cloud gaming assistant. Explain Step 9 (Select a server) of the onboarding guide. Give the user tips on how to pick a reliable, low-latency server and how their choice will impact the smoothness of their game stream.`,

  "help-step-10": `Act as a helpful cloud gaming assistant. Explain Step 10 (Automatic connection) of the onboarding guide. After the user clicks Play, provisioning and the secure tunnel start automatically; they only need to keep Noland open and approve an operating-system permission prompt if it appears.`,

  "help-step-11": `Act as a helpful cloud gaming assistant. Explain Step 11 (Complete in-app pairing) of the onboarding guide. Walk the user through Noland's in-app Sunshine pairing handoff and make clear that an external Moonlight app is not required.`,

  "help-step-12": `Act as a helpful cloud gaming assistant. Explain the Final Step (Computer password) of the onboarding guide. Tell the user what the default Windows password is ('password'), when they need to enter it, and how it gets them to their remote desktop.`,

  "settings-page": `Act as a helpful cloud gaming assistant. Explain how to configure the Vast.ai API key, marketplace filters, and embedded streaming preferences. Make clear that current Noland builds use the embedded GotaTun tunnel and do not require VPN-provider settings, external streaming or VPN apps, or networking command-line tools.`
};

const promptsDir = path.join(process.cwd(), 'prompts');

for (const [filename, text] of Object.entries(prompts)) {
  fs.writeFileSync(path.join(promptsDir, filename + '.md'), contextString + text);
}

console.log('Successfully updated 25 prompt files to focus on user setup and experience.');
