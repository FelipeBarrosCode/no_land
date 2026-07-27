import { useMemo, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import {
  VAST_API_KEY_URL,
  VAST_BILLING_URL,
  VAST_LOGIN_URL,
} from "../../lib/constants";
import type {
  OnboardingPayload,
  VastBrowserAutomationStatus,
  VastBrowserGeneratedApiKeyResult,
  VastBrowserBillingAction,
} from "../../lib/types";
import { TutorialModal } from "./TutorialModal";
import { tutorialSteps } from "./tutorialSteps";

interface Props {
  busy: boolean;
  onSubmit: (payload: OnboardingPayload) => Promise<void>;
  vastAutomationStatus: VastBrowserAutomationStatus | null;
  onConnectVastBrowser: () => Promise<unknown>;
  onRefreshVastAutomationStatus: () => Promise<VastBrowserAutomationStatus | null>;
  onGenerateVastApiKey: (
    apiKeyName?: string,
  ) => Promise<VastBrowserGeneratedApiKeyResult | null>;
  onOpenVastBillingBrowser: (
    action?: VastBrowserBillingAction,
  ) => Promise<unknown>;
}

interface FormState {
  appUsername: string;
  appPassword: string;
  vastApiKey: string;
}

export function OnboardingScreen({
  busy,
  onSubmit,
  vastAutomationStatus,
  onConnectVastBrowser,
  onRefreshVastAutomationStatus,
  onGenerateVastApiKey,
  onOpenVastBillingBrowser,
}: Props) {
  const [tutorialOpen, setTutorialOpen] = useState(true);
  const [tutorialCompleted, setTutorialCompleted] = useState(false);
  const [tutorialStep, setTutorialStep] = useState(0);
  const [form, setForm] = useState<FormState>({
    appUsername: "",
    appPassword: "",
    vastApiKey: "",
  });
  const [touched, setTouched] = useState<Record<keyof FormState, boolean>>({
    appUsername: false,
    appPassword: false,
    vastApiKey: false,
  });
  const [automationNote] = useState<string | null>(null);

  void vastAutomationStatus;
  void onConnectVastBrowser;
  void onRefreshVastAutomationStatus;
  void onGenerateVastApiKey;
  void onOpenVastBillingBrowser;

  const errors = useMemo(() => {
    return {
      appUsername:
        form.appUsername.trim().length < 3 ? "Use at least 3 characters" : "",
      appPassword:
        form.appPassword.length < 6 ? "Use at least 6 characters" : "",
      vastApiKey:
        form.vastApiKey.trim().length < 16 ? "API key seems too short" : "",
    };
  }, [form]);

  const hasErrors = Object.values(errors).some(Boolean);

  async function submitForm() {
    setTouched({
      appUsername: true,
      appPassword: true,
      vastApiKey: true,
    });
    if (hasErrors) {
      return;
    }

    await onSubmit({
      appUsername: form.appUsername.trim(),
      appPassword: form.appPassword,
      vastApiKey: form.vastApiKey.trim(),
      tailscaleApiKey: "",
    });
  }

  async function openExternalUrl(url: string) {
    try {
      await openUrl(url);
    } catch {
      window.open(url, "_blank", "noopener,noreferrer");
    }
  }

  async function openApiKeyPage() {
    await openExternalUrl(VAST_API_KEY_URL);
  }

  async function openBillingPage() {
    await openExternalUrl(VAST_BILLING_URL);
  }

  async function openLoginPage() {
    await openExternalUrl(VAST_LOGIN_URL);
  }

  function openTutorial() {
    setTutorialStep(0);
    setTutorialOpen(true);
  }

  function goToPreviousTutorialStep() {
    setTutorialStep((current) => Math.max(0, current - 1));
  }

  function goToNextTutorialStep() {
    if (tutorialStep === tutorialSteps.length - 1) {
      setTutorialCompleted(true);
      setTutorialOpen(false);
      return;
    }

    setTutorialStep((current) => current + 1);
  }

  return (
    <main className="crt-surface min-h-screen bg-hero-glow px-6 py-8">
      <div className="mx-auto flex min-h-[calc(100vh-4rem)] w-full max-w-6xl items-center justify-center">
        <Card className="pixel-frame w-full max-w-xl animate-fade-in p-8">
          <div className="flex items-start justify-between gap-4">
            <div className="space-y-2">
              <p className="font-display text-[10px] uppercase tracking-[0.2em] text-neon-cyan">
                01. Boot Sequence
              </p>
              <h1
                className="pixel-heading glitch-title font-display text-xl text-white md:text-2xl"
                data-text="Noland Connect Terminal"
              >
                Noland Connect Terminal
              </h1>
              <p className="text-[1.4rem] leading-[1.15] text-[#b4c8de]">
                Add your local credentials and{" "}
                <a
                  className="text-neon-cyan underline decoration-[#61f7ff] underline-offset-2 hover:text-white"
                  href={VAST_API_KEY_URL}
                  target="_blank"
                  rel="noreferrer"
                >
                  Vast.ai API key
                </a>
                . We will generate an SSH key pair, prepare the remote machine,
                and handle the connection flow during provisioning.
              </p>
            </div>

            <Button variant="ghost" onClick={openTutorial}>
              Help
            </Button>
          </div>

          <div className="mt-6 rounded-md border border-[#35506e] bg-[#0d1630]/80 p-4 text-[1.1rem] text-[#b4d7f4]">
            <p className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-lime">
              Vast.ai Account Setup
            </p>
            <p className="mt-2 leading-snug">
              Use your normal browser to log in to Vast.ai, add billing, and create an API key. Then paste that API key into Noland below.
            </p>
            <div className="mt-3 flex flex-wrap gap-3">
              <Button variant="secondary" disabled={busy} onClick={() => void openLoginPage()}>
                Open Vast.ai Login
              </Button>
              <Button variant="ghost" disabled={busy} onClick={() => void openBillingPage()}>
                Open Vast.ai Billing
              </Button>
              <Button variant="ghost" disabled={busy} onClick={() => void openApiKeyPage()}>
                Open API Key Page
              </Button>
            </div>
            <div className="mt-3 space-y-1 text-[1rem] text-[#8fb4d4]">
              <p>Sign in in your normal browser, then come back here and paste the API key.</p>
              {automationNote ? <p className="text-neon-cyan">{automationNote}</p> : null}
            </div>
          </div>

          <div className="mt-8 grid gap-4">
            <InputField
              label="Setup Username"
              placeholder="noland-user"
              value={form.appUsername}
              onChange={(event) =>
                setForm((prev) => ({
                  ...prev,
                  appUsername: event.target.value,
                }))
              }
              onBlur={() =>
                setTouched((prev) => ({ ...prev, appUsername: true }))
              }
              error={touched.appUsername ? errors.appUsername : undefined}
            />
            <InputField
              label="Setup Password"
              type="password"
              value={form.appPassword}
              onChange={(event) =>
                setForm((prev) => ({
                  ...prev,
                  appPassword: event.target.value,
                }))
              }
              onBlur={() =>
                setTouched((prev) => ({ ...prev, appPassword: true }))
              }
              error={touched.appPassword ? errors.appPassword : undefined}
            />
            <p className="-mt-2 text-[1.1rem] text-[#8fb4d4]">
              New here? The tutorial explains the full setup flow, and the
              remote computer password is{" "}
              <span className="text-neon-lime">password</span>.
            </p>
            <InputField
              label={
                <span>
                  <a
                    className="text-neon-cyan underline decoration-[#61f7ff] underline-offset-2 hover:text-white"
                    href={VAST_API_KEY_URL}
                    target="_blank"
                    rel="noreferrer"
                  >
                    Vast.ai
                  </a>{" "}
                  API Key
                </span>
              }
              type="password"
              placeholder="vast_xxxxx"
              value={form.vastApiKey}
              onChange={(event) =>
                setForm((prev) => ({ ...prev, vastApiKey: event.target.value }))
              }
              onBlur={() =>
                setTouched((prev) => ({ ...prev, vastApiKey: true }))
              }
              error={touched.vastApiKey ? errors.vastApiKey : undefined}
            />
          </div>

          <div className="mt-5 flex items-center justify-between gap-3">
            <button
              className="font-display text-[10px] uppercase tracking-[0.12em] text-neon-cyan hover:text-[#99f8ff]"
              type="button"
              onClick={openApiKeyPage}
            >
              Get your Vast.ai API key
            </button>
            <Button
              disabled={busy}
              loading={busy}
              loadingText="Configuring..."
              onClick={submitForm}
              className="px-8"
            >
              Continue
            </Button>
          </div>
        </Card>
      </div>

      <TutorialModal
        open={tutorialOpen}
        stepIndex={tutorialStep}
        steps={tutorialSteps}
        closable={tutorialCompleted}
        onBack={goToPreviousTutorialStep}
        onNext={goToNextTutorialStep}
        onClose={() => setTutorialOpen(false)}
      />


    </main>
  );
}
