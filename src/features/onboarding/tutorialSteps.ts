export interface TutorialStep {
  eyebrow: string;
  title: string;
  description: string;
}

export const tutorialSteps: TutorialStep[] = [
  {
    eyebrow: "Step 1",
    title: "Open the app",
    description: "You are in the right place. Noland Connect will walk you from account setup to launching your first cloud computer."
  },
  {
    eyebrow: "Step 2",
    title: "Open Vast.ai",
    description: "Head to Vast.ai next. That is where you rent the GPU machine that Noland Connect will prepare for you."
  },
  {
    eyebrow: "Step 3",
    title: "Create your Vast.ai account",
    description: "Make your Vast.ai account if you are new. It only takes a minute and unlocks server access."
  },
  {
    eyebrow: "Step 4",
    title: "Add billing",
    description: "Add a payment card in Vast.ai billing so you can rent a server when you are ready to launch."
  },
  {
    eyebrow: "Step 5",
    title: "Get your API key",
    description: "Copy your Vast.ai API key. Noland Connect uses it to find servers, start them, and manage the setup for you."
  },
  {
    eyebrow: "Step 6",
    title: "Download Moonlight",
    description: "Install Moonlight on your computer. That is the app you will use to stream into your cloud gaming machine."
  },
  {
    eyebrow: "Step 7",
    title: "Download WireGuard",
    description: "Install the WireGuard app too. Noland Connect uses it to create the secure connection to your server."
  },
  {
    eyebrow: "Step 8",
    title: "Select a server",
    description: "After setup, pick a server inside Noland Connect. Choose the one that fits your location, budget, and performance needs."
  },
  {
    eyebrow: "Step 9",
    title: "Connect when asked",
    description: "When the app prompts you, connect through WireGuard and continue. This links your machine to the rented server."
  },
  {
    eyebrow: "Step 10",
    title: "Follow the instructions",
    description: "Keep following the on-screen instructions during setup. The app will guide you through the remaining pairing steps."
  },
  {
    eyebrow: "Final Step",
    title: "Computer password",
    description: "The password to get into the computer is \"password\". Use that exact password when the remote computer asks for it."
  }
];
