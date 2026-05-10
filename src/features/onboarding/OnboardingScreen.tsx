import { useMemo, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import { VAST_API_KEY_URL } from "../../lib/constants";
import type { OnboardingPayload } from "../../lib/types";

interface Props {
  busy: boolean;
  onSubmit: (payload: OnboardingPayload) => Promise<void>;
}

interface FormState {
  appUsername: string;
  appPassword: string;
  vastApiKey: string;
}

export function OnboardingScreen({ busy, onSubmit }: Props) {
  const [form, setForm] = useState<FormState>({
    appUsername: "",
    appPassword: "",
    vastApiKey: ""
  });
  const [touched, setTouched] = useState<Record<keyof FormState, boolean>>({
    appUsername: false,
    appPassword: false,
    vastApiKey: false
  });

  const errors = useMemo(() => {
    return {
      appUsername: form.appUsername.trim().length < 3 ? "Use at least 3 characters" : "",
      appPassword: form.appPassword.length < 6 ? "Use at least 6 characters" : "",
      vastApiKey: form.vastApiKey.trim().length < 16 ? "API key seems too short" : ""
    };
  }, [form]);

  const hasErrors = Object.values(errors).some(Boolean);

  async function submitForm() {
    setTouched({ appUsername: true, appPassword: true, vastApiKey: true });
    if (hasErrors) {
      return;
    }

    await onSubmit({
      appUsername: form.appUsername.trim(),
      appPassword: form.appPassword,
      vastApiKey: form.vastApiKey.trim()
    });
  }

  async function openApiKeyPage() {
    try {
      await openUrl(VAST_API_KEY_URL);
    } catch {
      window.open(VAST_API_KEY_URL, "_blank", "noopener,noreferrer");
    }
  }

  return (
    <main className="crt-surface min-h-screen bg-hero-glow px-6 py-8">
      <div className="mx-auto flex min-h-[calc(100vh-4rem)] w-full max-w-6xl items-center justify-center">
        <Card className="pixel-frame w-full max-w-xl animate-fade-in p-8">
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
              Add your local credentials and Vast.ai API key. We will generate an SSH key pair and
              connect your account automatically.
            </p>
          </div>

          <div className="mt-8 grid gap-4">
            <InputField
              label="Setup Username"
              placeholder="noland-user"
              value={form.appUsername}
              onChange={(event) => setForm((prev) => ({ ...prev, appUsername: event.target.value }))}
              onBlur={() => setTouched((prev) => ({ ...prev, appUsername: true }))}
              error={touched.appUsername ? errors.appUsername : undefined}
            />
            <InputField
              label="Setup Password"
              type="password"
              value={form.appPassword}
              onChange={(event) => setForm((prev) => ({ ...prev, appPassword: event.target.value }))}
              onBlur={() => setTouched((prev) => ({ ...prev, appPassword: true }))}
              error={touched.appPassword ? errors.appPassword : undefined}
            />
            <InputField
              label="Vast.ai API Key"
              type="password"
              placeholder="vast_xxxxx"
              value={form.vastApiKey}
              onChange={(event) => setForm((prev) => ({ ...prev, vastApiKey: event.target.value }))}
              onBlur={() => setTouched((prev) => ({ ...prev, vastApiKey: true }))}
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
            <Button disabled={busy} loading={busy} loadingText="Configuring..." onClick={submitForm} className="px-8">
              Continue
            </Button>
          </div>
        </Card>
      </div>
    </main>
  );
}
