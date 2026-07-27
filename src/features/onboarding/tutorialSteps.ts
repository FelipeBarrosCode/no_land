import {
  VAST_API_KEY_URL,
  VAST_BILLING_URL,
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
      "You are in the right place. Noland Connect walks you from account setup to launching your first cloud gaming machine.",
  },
  {
    eyebrow: "Step 2",
    title: "Create your Vast.ai account",
    description:
      "Head to Vast.ai next. That is where you rent the GPU machine. Create your account if you are new — it only takes a minute.",
    linkLabel: "Open Vast.ai",
    linkUrl: VAST_API_KEY_URL,
  },
  {
    eyebrow: "Step 3",
    title: "Add billing to Vast.ai",
    description:
      "Add a payment method in Vast.ai billing so you can rent servers. You only pay for the hours your machine is running.",
    linkLabel: "Open Vast.ai Billing",
    linkUrl: VAST_BILLING_URL,
  },
  {
    eyebrow: "Step 4",
    title: "Get your API key",
    description:
      "Copy your Vast.ai API key from the account page. Noland uses it to find servers, start them, and set everything up automatically.",
    linkLabel: "Manage Vast.ai keys",
    linkUrl: VAST_API_KEY_URL,
  },
  {
    eyebrow: "Step 5",
    title: "Paste your API key in onboarding",
    description:
      "Return to Noland Connect, enter your credentials in the onboarding screen, and click Submit. The app validates everything before you continue.",
  },
  {
    eyebrow: "Step 6",
    title: "Select a server",
    description:
      "Click 'Select Server' on the dashboard toolbar. The server picker shows available GPU machines sorted by distance, reliability, and price. Use the search filters to narrow by country, price range, GPU RAM, and internet speed.",
  },
  {
    eyebrow: "Step 7",
    title: "Adjust server filters",
    description:
      "In the server picker, you can filter by country (US, Canada, Europe, Asia), set a max hourly price, require verified hosts, minimum GPU RAM, and toggle offer types (on-demand, interruptible, reserved). Click 'Find Offers' after setting your filters.",
  },
  {
    eyebrow: "Step 8",
    title: "Choose your server and storage",
    description:
      "Pick a server that fits your budget and performance needs. Set the storage size (recommended: 100 GB minimum). The app ranks servers by a combined score of distance, price, and GPU power.",
  },
  {
    eyebrow: "Step 9",
    title: "Launch and follow provisioning",
    description:
      "Click Provisioning on your chosen server. The app automatically handles connection setup, streaming configuration, and driver installation. Watch the progress timeline — no manual steps needed.",
  },
  {
    eyebrow: "Step 10",
    title: "Explore Settings",
    description:
      "Open Settings to customize your experience. Available sections: Profile (credentials), Server Preferences (template, reliability, pricing defaults), Client (streaming quality, codec, bitrate, resolution), Shared Storage (cloud backup provider), and Connection (connection type).",
  },
  {
    eyebrow: "Step 11",
    title: "Configure Shared Storage",
    description:
      "In Settings → Shared Storage, connect a cloud provider to back up your applications, saves, settings, and mods. Choose from Backblaze B2, S3, Cloudflare R2, Google Drive, OneDrive, Dropbox, and more. Click the robot icon next to any provider for a step-by-step setup guide.",
  },
  {
    eyebrow: "Step 12",
    title: "Manage your applications",
    description:
      "Use the Applications page to view all your backed-up applications. Select bundles to restore onto new instances. Choose Complete Restore (everything), Personal Data Only (saves, settings, mods), or Custom Selection. Cloud backups remain available even after you delete the instance.",
  },
];
