import fs from 'fs';
import path from 'path';

const contextString = "Context: The user is currently using 'Noland' (or Noland Connect), a platform that automates the process of renting cloud GPUs (via Vast.ai) and seamlessly connecting them for high-end remote game streaming. ";

const prompts = {
  "moonlight-card": `Act as a helpful cloud gaming assistant. Explain to the user how to properly download and set up Moonlight for use with Noland Connect. Describe how using Moonlight impacts their gaming experience by providing a low-latency, high-quality stream, and give them tips on the best settings to use.`,
  
  "wireguard-card": `Act as a helpful cloud gaming assistant. Explain to the user exactly how to set up WireGuard as their connection type in Noland Connect. Walk them through what they need to install, and explain how choosing WireGuard impacts their user experience in terms of connection stability and manual setup steps.`,
  
  "tailscale-card": `Act as a helpful cloud gaming assistant. Explain to the user how to easily set up Tailscale as their connection type in Noland Connect. Walk them through where to get their Tailscale API key, and explain how using Tailscale provides a smoother, automated user experience compared to manual VPNs.`,
  
  "set-server-card": `Act as a helpful cloud gaming assistant. Explain to the user how to properly search for and set their preferred server for cloud gaming. Walk them through what server specs to prioritize (like location and GPU) and how choosing the right server directly impacts their streaming latency and graphical quality.`,
  
  "rented-servers-section": `Act as a helpful cloud gaming assistant. Explain to the user how to manage their rented servers in Noland Connect. Describe what actions they should take when a server is listed as 'Ready' or 'Starting', and how they can use this section to quickly jump into or stop their gaming sessions.`,
  
  "selected-server-section": `Act as a helpful cloud gaming assistant. Explain to the user how to review their selected server's specifications. Walk them through which details to verify (such as price, location, and GPU) before they hit Play, and how this final check ensures they get the gaming experience they expect.`,
  
  "play-button-section": `Act as a helpful cloud gaming assistant. Explain to the user what to expect and what actions they need to take after clicking the Play button. Walk them through the connection prompts (like pairing Moonlight or activating their VPN) and how following these steps correctly bridges them into their game.`,
  
  "wireguard-modal-info": `Act as a helpful cloud gaming assistant. Explain to the user exactly how to handle the WireGuard connection step. Provide a clear step-by-step guide on how to import the downloaded .conf file into the WireGuard app and activate the tunnel so their game stream can connect successfully.`,
  
  "tailscale-modal-info": `Act as a helpful cloud gaming assistant. Explain to the user how to provide their Tailscale API key to automatically establish the VPN connection. Describe how this setup simplifies their experience by eliminating the need to manually import connection profiles.`,
  
  "server-picker-modal-header": `Act as a helpful cloud gaming assistant. Explain to the user how to navigate the Select Server Market. Walk them through how to browse the available GPU offers and how their selections here will impact both their hourly costs and their gaming performance.`,
  
  "server-search-preferences": `Act as a helpful cloud gaming assistant. Explain to the user how to properly set their server search filters. Give them advice on what minimum requirements to set (like storage or price) to find a good gaming rig, and how these filters affect the quality of servers they will see.`,
  
  "server-instance-card": `Act as a helpful cloud gaming assistant. Explain to the user how to evaluate an individual server offer card. Tell them which metrics matter most (like internet speed, reliability, and VRAM) and how those numbers will impact their actual gameplay experience.`,
  
  "help-step-1": `Act as a helpful cloud gaming assistant. Explain Step 1 (Open the app) of the Noland Connect onboarding guide. Tell the user what the Noland app does, how to use it, and how it serves as the control center for their game streaming experience.`,
  
  "help-step-2": `Act as a helpful cloud gaming assistant. Explain Step 2 (Open Vast.ai) of the onboarding guide. Walk the user through what Vast.ai is, why they need to visit it, and how it provides the raw server power for their gaming sessions.`,
  
  "help-step-3": `Act as a helpful cloud gaming assistant. Explain Step 3 (Create your Vast.ai account) of the onboarding guide. Provide clear instructions on how to register and explain why having this account is necessary to start renting servers.`,
  
  "help-step-4": `Act as a helpful cloud gaming assistant. Explain Step 4 (Add billing) of the onboarding guide. Walk the user through how to add billing details on Vast.ai, and explain how the pay-as-you-go hourly pricing impacts their costs.`,
  
  "help-step-5": `Act as a helpful cloud gaming assistant. Explain Step 5 (Get your API key) of the onboarding guide. Tell the user exactly where to find their Vast.ai API key, how to paste it into Noland, and how this automates the entire server renting process.`,
  
  "help-step-6": `Act as a helpful cloud gaming assistant. Explain Step 6 (Download Moonlight) of the onboarding guide. Walk the user through downloading Moonlight, and explain how it acts as the high-quality video player for their games.`,
  
  "help-step-7": `Act as a helpful cloud gaming assistant. Explain Step 7 (Install your connection app) of the onboarding guide. Help the user choose between installing WireGuard or Tailscale, and explain how their choice impacts the simplicity of their future connections.`,
  
  "help-step-8": `Act as a helpful cloud gaming assistant. Explain Step 8 (Get a Tailscale API key) of the onboarding guide. If the user chose Tailscale, walk them through how to generate and add their Tailscale API key, and how it provides a seamless, automated connection.`,
  
  "help-step-9": `Act as a helpful cloud gaming assistant. Explain Step 9 (Select a server) of the onboarding guide. Give the user tips on how to pick a reliable, low-latency server and how their choice will impact the smoothness of their game stream.`,
  
  "help-step-10": `Act as a helpful cloud gaming assistant. Explain Step 10 (Connect when asked) of the onboarding guide. Prepare the user for the connection prompts they will see after clicking Play, and explain how to properly activate their VPN when instructed.`,
  
  "help-step-11": `Act as a helpful cloud gaming assistant. Explain Step 11 (Follow the instructions) of the onboarding guide. Walk the user through the final Moonlight PIN pairing process, and explain how this securely links their local computer to the gaming server.`,
  
  "help-step-12": `Act as a helpful cloud gaming assistant. Explain the Final Step (Computer password) of the onboarding guide. Tell the user what the default Windows password is ('password'), when they need to enter it, and how it gets them to their remote desktop.`,
  
  "settings-page": `Act as a helpful cloud gaming assistant. Explain to the user how to configure their Noland Connect settings properly. Walk them through the API keys, connection providers, and Moonlight preferences, and explain how adjusting these settings optimizes their automated workflows and streaming quality.`
};

const promptsDir = path.join(process.cwd(), 'prompts');

for (const [filename, text] of Object.entries(prompts)) {
  fs.writeFileSync(path.join(promptsDir, filename + '.md'), contextString + text);
}

console.log('Successfully updated 25 prompt files to focus on user setup and experience.');
