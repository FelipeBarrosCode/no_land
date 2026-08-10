import fs from 'fs';
import path from 'path';

const baseContext = `Context: The user is currently using 'Noland' (or Noland Connect), a platform that automates the process of renting cloud GPUs (via Vast.ai) and seamlessly connecting them for high-end remote game streaming.

Role: Act as a helpful cloud gaming assistant.

Goal: Explain to the user how to properly use or set up this specific feature and how it impacts their gaming experience (e.g., latency, convenience, graphics quality). Do not give overly technical "under the hood" networking or systems engineering explanations. Instead, use the Noland facts provided below to guide the user practically and clearly.

Noland Facts for this feature:
`;

const facts = {
  "moonlight-card": `Noland Connect includes its own embedded Moonlight-compatible streaming client. The user does not need to download or install the external Moonlight desktop app. Noland installs and configures Sunshine on the remote server, completes pairing in the app, and starts the stream inside Noland.`,

  "wireguard-card": `Noland uses its embedded GotaTun engine to create and manage a WireGuard-compatible tunnel on Linux, macOS, and Windows. No external WireGuard or GotaTun installation, command-line tool, configuration download, or manual profile import is required. The operating system may ask the user to approve elevation so Noland can create the network adapter.`,

  "tailscale-card": `Tailscale is not part of Noland's current desktop connection flow. Current builds use Noland's embedded GotaTun engine and do not require a Tailscale account, API key, or locally installed VPN app.`,

  "set-server-card": `This panel opens the server marketplace (powered by Vast.ai) to find GPU rental offers. Users must pick a server geographically close to them to reduce stream latency (lag), and pick a GPU capable of running the games they want to play smoothly.`,

  "rented-servers-section": `This section lists the user's currently active server leases from Vast.ai. It shows real-time states like 'Starting' or 'Ready'. Users can manage their hourly costs by stopping or destroying unused servers here. Once a server is 'Ready', they can select it to start gaming.`,

  "selected-server-section": `This overview card shows the specific server the user is about to connect to. It displays critical specs like geographic location (affecting latency), price per hour, GPU model, and reliability score. The user should verify these before hitting Play.`,

  "play-button-section": `Clicking Play triggers Noland's automation immediately: it rents and prepares the server, installs Sunshine, starts the embedded secure tunnel, verifies connectivity, pairs the embedded streaming client, and prepares the stream. The user may only need to approve an operating-system permission prompt; no external app activation or profile import is required.`,

  "wireguard-modal-info": `Noland automatically creates and activates the local WireGuard-compatible connection with its embedded GotaTun engine. The user does not download or import a configuration file and does not open a separate WireGuard app. If an operating-system elevation prompt appears, approving it lets Noland create the network adapter and continue provisioning.`,

  "tailscale-modal-info": `This prompt belongs to a legacy connection flow. Current Noland builds use the embedded GotaTun tunnel and do not ask the user for a Tailscale API key or require a local Tailscale installation.`,

  "server-picker-modal-header": `This modal pulls live GPU rental offers from Vast.ai. Renting a machine directly from this panel provisions the user's remote streaming server. The choices here balance hourly cost against gaming performance.`,

  "server-search-preferences": `The server picker has only two search controls: country selects which Vast.ai market results to fetch, and full-text search filters every field in the returned offers, including state or region, city, GPU, CPU, host, price, reliability, network speed, and offer type. Full-text search applies to the currently returned market page.`,

  "server-instance-card": `Each card represents a machine for rent. It shows download/upload speeds, reliability scores, and VRAM. High internet speeds and reliability are crucial to prevent the game stream from stuttering or dropping.`,

  "help-step-1": `Step 1 of onboarding: The user must open Noland Connect. Noland acts as the central control deck that bridges their local computer with the remote cloud gaming hardware.`,

  "help-step-2": `Step 2 of onboarding: The user must go to Vast.ai. Vast.ai is the marketplace that provides the raw server power and cloud GPUs that Noland will rent on the user's behalf for their gaming sessions.`,

  "help-step-3": `Step 3 of onboarding: The user must create an account on Vast.ai so they have access to rent the GPU servers.`,

  "help-step-4": `Step 4 of onboarding: The user must add billing details to Vast.ai. Servers are rented on a pay-as-you-go hourly basis, so a payment method is required to start renting compute hours.`,

  "help-step-5": `Step 5 of onboarding: The user needs to find their Vast.ai API key in their Vast.ai account settings and paste it into Noland. This key is what gives Noland permission to automatically search for and rent servers for the user.`,

  "help-step-6": `Step 6 of onboarding: Noland includes an embedded Moonlight-compatible streaming client, so the user does not need to install Moonlight separately. Explain that Noland prepares Sunshine remotely and handles pairing and streaming inside the app.`,

  "help-step-7": `Step 7 of onboarding: The secure connection engine is included with Noland. The user does not install WireGuard, GotaTun, Tailscale, or networking command-line tools. They may need to approve an operating-system elevation prompt when Noland creates its managed adapter.`,

  "help-step-8": `Step 8 of onboarding: No separate VPN account or API key is required. Noland generates the connection details, starts its embedded GotaTun tunnel, and verifies the server automatically.`,

  "help-step-9": `Step 9 of onboarding: The user must select a server from the marketplace. They need to balance geographic proximity (for low lag), GPU performance, and the hourly price.`,

  "help-step-10": `Step 10 of onboarding: After clicking Play, provisioning and connection start automatically. The user should keep Noland open and approve an operating-system permission prompt if one appears; Noland starts and verifies the embedded tunnel without manual VPN steps.`,

  "help-step-11": `Step 11 of onboarding: Noland handles the Sunshine pairing handoff with its embedded Moonlight-compatible client inside the app. The user should follow the in-app pairing instructions; no external Moonlight app is required.`,

  "help-step-12": `Step 12 of onboarding: Once connected, the user will see a Windows login screen. The default password for the remote streaming desktop is simply 'password'.`,

  "settings-page": `The settings panel configures Noland's automation. The Vast.ai API key allows server search and rental, server filters control marketplace defaults, and streaming preferences control resolution, bitrate, and frame rate. The secure local connection uses Noland's embedded GotaTun engine and does not require VPN-provider settings, external apps, or networking command-line tools.`
};

const promptsDir = path.join(process.cwd(), 'prompts');

for (const [filename, fact] of Object.entries(facts)) {
  fs.writeFileSync(path.join(promptsDir, filename + '.md'), baseContext + fact);
}

console.log('Successfully updated 25 prompt files with structured Noland facts and clear AI goals.');
