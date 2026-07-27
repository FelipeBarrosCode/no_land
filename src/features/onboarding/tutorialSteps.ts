import {
  MOONLIGHT_DOWNLOAD_URL,
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
    title: "Install the managed tunnel dependency",
    description:
      "Install GotaTun on your computer. Noland uses it to bring up the local managed WireGuard-compatible userspace tunnel during provisioning.",
    links: [{ label: "Download GotaTun", url: WIREGUARD_DOWNLOAD_URL }],
  },
  {
    eyebrow: "Step 8",
    title: "Select a server",
    description:
      "After setup, pick a server inside Noland Connect. Choose the one that fits your location, budget, and performance needs.",
  },
  {
    eyebrow: "Step 9",
    title: "Start the managed tunnel when asked",
    description:
      "When the app reaches tunnel setup, let Noland start the managed GotaTun tunnel. You may need to approve elevation so routing and the tunnel interface can be configured locally.",
  },
  {
    eyebrow: "Step 10",
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
