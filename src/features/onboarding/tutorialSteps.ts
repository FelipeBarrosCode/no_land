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
      "Add payment in Vast.ai billing so you can rent a server when you are ready to launch.",
    linkLabel: "Open Vast.ai Billing",
    linkUrl: VAST_BILLING_URL,
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
    title: "Return to Noland",
    description:
      "After billing and API key setup, return to Noland Connect and paste your Vast.ai API key into onboarding.",
  },
  {
    eyebrow: "Step 7",
    title: "Select a server",
    description:
      "After setup, pick a server inside Noland Connect. Choose the one that fits your location, budget, and performance needs.",
  },
  {
    eyebrow: "Step 8",
    title: "Follow the instructions",
    description:
      "Keep following the on-screen instructions during setup. The app will guide you through the remaining pairing steps for Sunshine and Moonlight.",
  },
  {
    eyebrow: "Step 9",
    title: "Sign in to your computer",
    description:
      "Use the credentials configured for your session when the remote computer asks you to sign in.",
  },
];
