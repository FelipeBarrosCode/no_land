import {
  MOONLIGHT_DOWNLOAD_URL,
  TAILSCALE_API_KEY_URL,
  TAILSCALE_DOWNLOAD_URL,
  VAST_API_KEY_URL,
  WIREGUARD_DOWNLOAD_URL,
} from "../../lib/constants";

export interface TutorialStep {
  eyebrow: string;
  title: string;
  description: string;
  linkLabel?: string;
  linkUrl?: string;
  links?: { label: string; url: string }[];
}

export const tutorialSteps: TutorialStep[] = [
  {
    eyebrow: "Step 1",
    title: "Open the app",
    description:
      "You are in the right place. Noland Connect will walk you from account setup to launching your first cloud computer.",
  },
  {
    eyebrow: "Step 2",
    title: "Open Vast.ai",
    description:
      "Head to Vast.ai next. That is where you rent the GPU machine that Noland Connect will prepare for you.",
    linkLabel: "Open Vast.ai",
    linkUrl: VAST_API_KEY_URL,
  },
  {
    eyebrow: "Step 3",
    title: "Create your Vast.ai account",
    description:
      "Make your Vast.ai account if you are new. It only takes a minute and unlocks server access.",
    linkLabel: "Open Vast.ai",
    linkUrl: VAST_API_KEY_URL,
  },
  {
    eyebrow: "Step 4",
    title: "Add billing",
    description:
      "Add a payment card in Vast.ai billing so you can rent a server when you are ready to launch.",
    linkLabel: "Open Vast.ai",
    linkUrl: VAST_API_KEY_URL,
  },
  {
    eyebrow: "Step 5",
    title: "Get your API key",
    description:
      "Copy your Vast.ai API key. Noland Connect uses it to find servers, start them, and manage the setup for you.",
    linkLabel: "Manage Vast.ai keys",
    linkUrl: VAST_API_KEY_URL,
  },
  {
    eyebrow: "Step 6",
    title: "Download Moonlight",
    description:
      "Install Moonlight on your computer. That is the app you will use to stream into your cloud gaming machine.",
    linkLabel: "Download Moonlight",
    linkUrl: MOONLIGHT_DOWNLOAD_URL,
  },
  {
    eyebrow: "Step 7",
    title: "Install your connection app",
    description:
      "Install WireGuard or Tailscale. WireGuard creates a direct VPN tunnel. Tailscale uses your Tailscale mesh network for an even simpler setup.",
    links: [
      { label: "Download WireGuard", url: WIREGUARD_DOWNLOAD_URL },
      { label: "Download Tailscale", url: TAILSCALE_DOWNLOAD_URL },
    ],
  },
  {
    eyebrow: "Step 8 (Optional)",
    title: "Get a Tailscale auth key",
    description:
      "If you want to use Tailscale instead of WireGuard, grab your Tailscale auth key. Paste it in the optional field on the setup screen or in Settings > Connection later.",
    linkLabel: "Tailscale Admin Keys",
    linkUrl: TAILSCALE_API_KEY_URL,
  },
  {
    eyebrow: "Step 9",
    title: "Select a server",
    description:
      "After setup, pick a server inside Noland Connect. Choose the one that fits your location, budget, and performance needs.",
  },
  {
    eyebrow: "Step 10",
    title: "Connect when asked",
    description:
      "When the app prompts you to choose a connection method, pick WireGuard or Tailscale. WireGuard requires importing a config file. Tailscale connects your devices through your mesh network automatically.",
  },
  {
    eyebrow: "Step 11",
    title: "Follow the instructions",
    description:
      "Keep following the on-screen instructions during setup. The app will guide you through the remaining pairing steps for Sunshine and Moonlight.",
  },
  {
    eyebrow: "Final Step",
    title: "Computer password",
    description:
      'The password to get into the computer is "password". Use that exact password when the remote computer asks for it.',
  },
];
