import { translate, translateSource } from "../../lib/i18n";
import { useEffect, useRef, useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import { AIPromptHelper } from "../../components/ui/AIPromptHelper";
import { useAppStore } from "../../store/appStore";
import type {
  ProviderDefinition,
  SharedStorageTestResult,
  ProfileReference,
} from "../../lib/types";

interface Props {
  busy: boolean;
  providers: ProviderDefinition[];
  profiles: ProfileReference[];
  testResult: SharedStorageTestResult | null;
  oauthSessionId: string | null;
  onConnectProvider: (
    provider: string,
    credentials: Record<string, string>,
    bucket: string | null,
    prefix: string | null,
    displayName: string,
  ) => Promise<void>;
  onTestConnection: (profileId: string) => Promise<void>;
  onSetActiveProfile: (profileId: string) => Promise<void>;
  onDisconnect: (profileId: string) => Promise<void>;
  onLoadProviders: () => Promise<void>;
  onLoadProfiles: () => Promise<void>;
  onBeginOauthFlow: (provider: string, displayName: string, clientId?: string, clientSecret?: string | null, providerFields?: Record<string, string>) => Promise<string | null>;
  onCompleteOauthFlow: (sessionId: string) => Promise<void>;
  onCancelOauthFlow: (sessionId: string) => Promise<void>;
}

const CATEGORY_LABELS: Record<string, string> = {
  "object-storage": "storage.category.object",
  "cloud-drives": "storage.category.drives",
  "enterprise-and-self-hosted": "storage.category.enterprise",
};

export function SharedStorageSettingsV2({
  busy,
  providers,
  profiles,
  testResult,
  oauthSessionId,
  onConnectProvider,
  onTestConnection,
  onSetActiveProfile,
  onDisconnect,
  onLoadProviders,
  onLoadProfiles,
  onBeginOauthFlow,
  onCompleteOauthFlow,
  onCancelOauthFlow,
}: Props) {
  const [selectedProvider, setSelectedProvider] = useState<ProviderDefinition | null>(null);
  const [showProviderPicker, setShowProviderPicker] = useState(false);
  const [formValues, setFormValues] = useState<Record<string, string>>({});
  const [displayName, setDisplayName] = useState("");
  const [bucket, setBucket] = useState<string | null>(null);
  const [prefix, setPrefix] = useState<string | null>(null);

  // Track the end of an OAuth session so completing authorization can land on
  // the connected-provider card instead of bouncing back to the initial form.
  const prevOauthSessionIdRef = useRef<string | null>(oauthSessionId);
  const [oauthJustCompleted, setOauthJustCompleted] = useState(false);

  // Read the global store error so we can surface it contextually inside
  // the OAuth panel.  The global error banner is easy to miss; showing the
  // message right next to the "Complete Authorization" button makes it
  // obvious why the flow reset.
  const storeError = useAppStore((state) => state.error);
  const clearStoreError = useAppStore((state) => state.clearError);

  useEffect(() => {
    void onLoadProviders();
    void onLoadProfiles();
  }, [onLoadProviders, onLoadProfiles]);

  useEffect(() => {
    const hadActiveSession = prevOauthSessionIdRef.current !== null;
    prevOauthSessionIdRef.current = oauthSessionId;

    if (hadActiveSession && oauthSessionId === null && !storeError) {
      // The OAuth token exchange finished without an error. Wait for the
      // refreshed profile list before switching to the connected card.
      setOauthJustCompleted(true);
    }
  }, [oauthSessionId, storeError]);

  useEffect(() => {
    if (!oauthJustCompleted || profiles.length === 0) {
      return;
    }

    setOauthJustCompleted(false);
    setSelectedProvider(null);
    setFormValues({});
    setBucket(null);
    setPrefix(null);
    setDisplayName("");
  }, [oauthJustCompleted, profiles]);

  const connectedProfile = profiles.find((profile) => profile.active) ?? profiles[0] ?? null;
  const selectedProviderFields = selectedProvider?.fields ?? [];
  const hasDedicatedBucketField = selectedProviderFields.some((field) => field.key === "bucket");
  const hasDedicatedPrefixField = selectedProviderFields.some((field) => field.key === "prefix");
  const staticCredentialFields = selectedProviderFields.filter(
    (field) => !["bucket", "prefix"].includes(field.key),
  );
  const oauthProviderFields = selectedProviderFields.filter(
    (field) => !["client_id", "client_secret"].includes(field.key),
  );

  const categorizedProviders = providers.reduce<Record<string, ProviderDefinition[]>>(
    (acc, p) => {
      const cat = p.category || "object-storage";
      if (!acc[cat]) acc[cat] = [];
      acc[cat].push(p);
      return acc;
    },
    {},
  );

  const PROVIDER_PROMPT_MAP: Record<string, string> = {};

  function handleProviderSelect(provider: ProviderDefinition) {
    setSelectedProvider(provider);
    setFormValues({});
    setDisplayName(provider.label);
    setShowProviderPicker(false);
  }

  function handleFieldChange(key: string, value: string) {
    setFormValues((prev) => ({ ...prev, [key]: value }));
  }

  async function handleConnect() {
    if (!selectedProvider) return;
    const effectiveBucket = (bucket || formValues["bucket"] || "").trim() || null;
    const effectivePrefix = (prefix || formValues["prefix"] || "").trim() || null;
    await onConnectProvider(
      selectedProvider.provider,
      formValues,
      effectiveBucket,
      effectivePrefix,
      displayName,
    );
    setSelectedProvider(null);
    setFormValues({});
    setBucket(null);
    setPrefix(null);
  }

  return (
    <div className="space-y-6">
      {/* Empty state */}
      {!connectedProfile && !selectedProvider && (
        <Card className="p-6">
          <div className="flex items-center gap-2 mb-4">
            <h3 className="text-lg font-display text-neon-cyan">
              {translate("generated.0e9614b59ef4c78b")}
            </h3>
            <AIPromptHelper
              topic={translate("generated.db4dc7d44e7e2a36")}
              promptText={`# Shared Storage

Noland Shared Storage keeps your games, applications, saves, settings, and mods available across your Noland instances.

## Choosing a provider

- **Object Storage** (B2, S3, R2): Best for frequent backups. Pay per GB stored.
- **Cloud Drives** (Google Drive, OneDrive, Dropbox): Convenient if you already have an account.
- **Enterprise / Self-hosted** (Azure, GCS, SFTP, WebDAV): For advanced setups.

All data is encrypted before upload and can only be decrypted with your repository key.`}
              variant="icon"
            />
          </div>
          <p className="text-sm text-gray-400 mb-6">
            {translate("generated.1d70042e1ff9b60f")}
          </p>
          <p className="text-sm text-gray-500 mb-4">
            {translate("generated.567127c5b616cc02")}
          </p>
          <Button variant="primary" onClick={() => setShowProviderPicker(true)} disabled={busy}>
            {translate("generated.320c8881eac2ca36")}
          </Button>
        </Card>
      )}

      {/* Connected state */}
      {connectedProfile && !selectedProvider && (
        <Card className="p-6">
          <h3 className="text-lg font-display text-neon-cyan mb-4">
            {translate("generated.0e9614b59ef4c78b")}
          </h3>
          <div className="space-y-3 mb-6">
            <div className="flex items-center justify-between">
              <span className="text-sm text-gray-400">{translate("generated.472590ae974d4c1f")}</span>
              <span className="text-sm text-neon-lime">{connectedProfile.providerLabel}</span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-sm text-gray-400">{translate("generated.920e413c7d411b61")}</span>
              <span className="text-sm text-green-400">{translate("generated.22965568d22a14ee")}</span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-sm text-gray-400">{translate("generated.13d6ff07b8a5d792")}</span>
              <span className="text-sm text-gray-200 font-mono">{connectedProfile.id.substring(0, 12)}...</span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-sm text-gray-400">{translate("generated.18d67c992b71ce69")}</span>
              <span className="text-sm text-gray-200">{connectedProfile.displayName}</span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-sm text-gray-400">{translate("generated.5608b89a81125b78")}</span>
              <span className="text-sm text-gray-200">{connectedProfile.active ? translate("generated.85a39ab345d672ff") : translate("generated.1ea442a134b2a184")}</span>
            </div>
          </div>

          {profiles.length > 1 && (
            <div className="mb-6 rounded border border-[#3f476c] bg-[#0b0f23]/60 p-3">
              <p className="text-sm text-gray-200">{translate("generated.17ee1bb8b8821502")}</p>
              <div className="mt-3 space-y-2">
                {profiles.map((profile) => (
                  <div
                    key={profile.id}
                    className="flex items-center justify-between gap-3 rounded border border-[#3f476c] px-3 py-2"
                  >
                    <div>
                      <p className="text-sm text-gray-100">{profile.displayName}</p>
                      <p className="text-xs text-gray-500">{profile.providerLabel}</p>
                    </div>
                    <div className="flex items-center gap-2">
                      {profile.active ? (
                        <span className="text-xs uppercase tracking-wide text-neon-lime">{translate("generated.92340695899bd2d8")}</span>
                      ) : (
                        <Button
                          variant="secondary"
                          onClick={() => onSetActiveProfile(profile.id)}
                          disabled={busy}
                        >
                          {translate("generated.edec3be72b3ed02b")}
                        </Button>
                      )}
                      <Button
                        variant="ghost"
                        className="text-red-400"
                        onClick={() => onDisconnect(profile.id)}
                        disabled={busy}
                      >
                        {translate("generated.acfc5be785a9bb3d")}
                      </Button>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}

          {testResult && (
            <div className={`mb-4 p-3 rounded border text-sm ${
              testResult.error
                ? "bg-red-900/30 border-red-500/50 text-red-300"
                : "bg-green-900/30 border-green-500/50 text-green-300"
            }`}>
              {testResult.error || "Connection test passed successfully"}
              {testResult.latencyMs != null && (
                <span className="ml-2 text-gray-400">
                  ({testResult.latencyMs}{translate("generated.abe01774f0229206")}
                </span>
              )}
            </div>
          )}

          <div className="mb-4 rounded border border-[#3f476c] bg-[#0b0f23]/60 p-3">
            <p className="text-sm text-gray-200">{translate("generated.31cc6e316ec6b651")}</p>
            <p className="mt-1 text-xs text-gray-500">
              {translate("generated.eba933bf191e8079")}
            </p>
          </div>

          <div className="flex gap-2 flex-wrap">
            <Button
              variant="secondary"
              onClick={() => onTestConnection(connectedProfile.id)}
              disabled={busy}
              loading={busy}
              loadingText={translate("generated.6c02a28421f8ad91")}
            >
              {translate("generated.c02977b07ec93816")}
            </Button>
            <Button
              variant="ghost"
              onClick={() => setShowProviderPicker(true)}
              disabled={busy}
            >
              {translate("generated.70fed393a3cc3aef")}
            </Button>
            <Button
              variant="ghost"
              className="text-red-400"
              onClick={() => onDisconnect(connectedProfile.id)}
              disabled={busy}
            >
              {translate("generated.acfc5be785a9bb3d")}
            </Button>
          </div>
        </Card>
      )}

      {/* Provider picker */}
      {showProviderPicker && (
        <Card className="p-6">
          <div className="flex items-center justify-between mb-4">
            <h3 className="text-lg font-display text-neon-cyan">{translate("generated.c2f590058ef5d597")}</h3>
            <Button variant="ghost" onClick={() => setShowProviderPicker(false)}>
              {translate("generated.76900f1bfd16c8d4")}
            </Button>
          </div>
          <div className="space-y-6">
            {Object.entries(categorizedProviders).map(([category, catProviders]) => (
              <div key={category}>
                <h4 className="text-xs font-display uppercase tracking-wider text-gray-500 mb-2">
                  {CATEGORY_LABELS[category] ? translate(CATEGORY_LABELS[category]) : category}
                </h4>
                <div className="grid gap-2 sm:grid-cols-2">
                  {catProviders.map((provider) => (
                    <button
                      key={provider.provider}
                      className="group relative border border-[#3f476c] bg-[#0b0f23] p-3 pr-10 text-left text-sm transition hover:border-neon-cyan hover:bg-[#121731]"
                      onClick={() => handleProviderSelect(provider)}
                    >
                      {provider.provider === "google_drive" && (
                        <span
                          className="absolute right-2 top-2 text-lg leading-none text-amber-300"
                          title={translate("generated.53aa318cddeb0e19")}
                        >
                          <span aria-hidden="true">★</span>
                          <span className="sr-only">{translate("generated.53aa318cddeb0e19")}</span>
                        </span>
                      )}
                      <div className="flex items-center justify-between gap-2">
                        <p className="text-gray-200 font-medium">{translateSource(provider.label)}</p>
                        {PROVIDER_PROMPT_MAP[provider.provider] && (
                          <AIPromptHelper
                            topic={translate("storage.provider.setup.topic", { provider: provider.label })}
                            promptText={PROVIDER_PROMPT_MAP[provider.provider]}
                            variant="icon"
                          />
                        )}
                      </div>
                      <p className="text-xs text-gray-500 mt-1">{translateSource(provider.description)}</p>
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </div>
        </Card>
      )}

      {/* Provider form - static credentials */}
      {selectedProvider && !selectedProvider.isOauth && (
        <Card className="p-6">
          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-2">
              <h3 className="text-lg font-display text-neon-cyan">
                {translate("generated.6defafa2caa65304")} {selectedProvider.label}
              </h3>
              {PROVIDER_PROMPT_MAP[selectedProvider.provider] && (
                <AIPromptHelper
                  topic={translate("storage.provider.setup.topic", { provider: selectedProvider.label })}
                  promptText={PROVIDER_PROMPT_MAP[selectedProvider.provider]}
                  variant="icon"
                />
              )}
            </div>
            <Button variant="ghost" onClick={() => setSelectedProvider(null)}>
              {translate("generated.9f73d5ebe9024c6b")}
            </Button>
          </div>

          <div className="space-y-4">
            <InputField
              label={translate("generated.18d67c992b71ce69")}
              value={displayName}
              onChange={(e) => setDisplayName(e.currentTarget.value)}
              placeholder={translate("generated.7bad6ea57de19d52")}
            />

            {staticCredentialFields.map((field) => {
              if (typeof field.fieldType === "object" && field.fieldType !== null && "options" in field.fieldType) {
                return (
                  <label key={field.key} className="flex flex-col gap-2 text-base">
                    <span className="font-display text-[10px] uppercase tracking-[0.14em] text-[#9ad9ff]">{translateSource(field.label)}</span>
                    <select
                      className="border border-[#3f476c] bg-[#0b0f23] px-3 py-2 text-[1.1rem] text-[#dff8ff] outline-none shadow-[inset_0_0_0_2px_#121731] focus:border-neon-cyan"
                      value={formValues[field.key] || field.fieldType.options[0]?.value || ""}
                      onChange={(e) => handleFieldChange(field.key, e.currentTarget.value)}
                      disabled={busy}
                    >
                      {field.fieldType.options.map((option) => (
                        <option key={option.value} value={option.value}>
                          {translateSource(option.label)}
                        </option>
                      ))}
                    </select>
                  </label>
                );
              }
              if (field.fieldType === "toggle") {
                return (
                  <label key={field.key} className="flex items-center justify-between rounded border border-[#3f476c] bg-[#0b0f23] px-3 py-2 text-sm text-[#dff8ff]">
                    <span>{translateSource(field.label)}</span>
                    <input
                      type="checkbox"
                      checked={(formValues[field.key] || "false") === "true"}
                      onChange={(e) => handleFieldChange(field.key, e.currentTarget.checked ? "true" : "false")}
                      disabled={busy}
                    />
                  </label>
                );
              }
              return (
                <InputField
                  key={field.key}
                  label={translateSource(field.label)}
                  value={formValues[field.key] || ""}
                  onChange={(e) => handleFieldChange(field.key, e.currentTarget.value)}
                  placeholder={field.placeholder ? translateSource(field.placeholder) : ""}
                  type={typeof field.fieldType === "string" && field.fieldType === "password" ? "password" : "text"}
                  disabled={busy}
                />
              );
            })}

            {!hasDedicatedBucketField && (
              <InputField
                label={translate("generated.ebb53106f5eb44b5")}
                value={bucket || ""}
                onChange={(e) => setBucket(e.currentTarget.value || null)}
                placeholder={translate("generated.800f2e137fa1c022")}
              />
            )}
            {!hasDedicatedPrefixField && (
              <InputField
                label={translate("generated.9eb94e768d3f763e")}
                value={prefix || ""}
                onChange={(e) => setPrefix(e.currentTarget.value || null)}
                placeholder={translate("generated.d00182a96d461ba3")}
              />
            )}

            <Button
              variant="primary"
              onClick={handleConnect}
              disabled={busy || displayName.trim().length < 2}
              loading={busy}
              loadingText={translate("generated.5f04ae9ed6a865bb")}
            >
              {translate("generated.1a2303ede07493ac")} {selectedProvider.label}
            </Button>
          </div>
        </Card>
      )}

      {/* Provider form - OAuth */}
      {selectedProvider && selectedProvider.isOauth && (
        <Card className="p-6">
          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-2">
              <h3 className="text-lg font-display text-neon-cyan">
                {translate("generated.b6741b4ccf6d675a")} {selectedProvider.label}
              </h3>
              {PROVIDER_PROMPT_MAP[selectedProvider.provider] && (
                <AIPromptHelper
                  topic={translate("storage.provider.setup.topic", { provider: selectedProvider.label })}
                  promptText={PROVIDER_PROMPT_MAP[selectedProvider.provider]}
                  variant="icon"
                />
              )}
            </div>
            <Button variant="ghost" onClick={() => setSelectedProvider(null)}>
              {translate("generated.9f73d5ebe9024c6b")}
            </Button>
          </div>

          <p className="text-sm text-gray-400 mb-4">
            {selectedProvider.label} {translate("generated.1bb25cdb464ff46a")}
          </p>

          {!oauthSessionId && (
            <div className="space-y-4 mb-4">
              <InputField
                label={translate("generated.18d67c992b71ce69")}
                value={displayName}
                onChange={(e) => setDisplayName(e.currentTarget.value)}
                placeholder={translate("generated.a6689cd30e6831c7")}
              />
              <InputField
                label={translate("generated.8726db013948f070")}
                value={formValues["client_id"] || ""}
                onChange={(e) => handleFieldChange("client_id", e.currentTarget.value)}
                placeholder={translate("generated.c85f2a53bfb9575b")}
              />
              <InputField
                label={translate("generated.ae21cf6d24b8ca46")}
                value={formValues["client_secret"] || ""}
                onChange={(e) => handleFieldChange("client_secret", e.currentTarget.value)}
                placeholder={translate("generated.245c6b7190118758")}
                type="password"
              />
              {oauthProviderFields.map((field) => {
                if (typeof field.fieldType === "object" && field.fieldType !== null && "options" in field.fieldType) {
                  return (
                    <label key={field.key} className="flex flex-col gap-2 text-base">
                      <span className="font-display text-[10px] uppercase tracking-[0.14em] text-[#9ad9ff]">{translateSource(field.label)}</span>
                      <select
                        className="border border-[#3f476c] bg-[#0b0f23] px-3 py-2 text-[1.1rem] text-[#dff8ff] outline-none shadow-[inset_0_0_0_2px_#121731] focus:border-neon-cyan"
                        value={formValues[field.key] || field.fieldType.options[0]?.value || ""}
                        onChange={(e) => handleFieldChange(field.key, e.currentTarget.value)}
                        disabled={busy}
                      >
                        {field.fieldType.options.map((option) => (
                          <option key={option.value} value={option.value}>
                            {translateSource(option.label)}
                          </option>
                        ))}
                      </select>
                      {field.helpText && <span className="text-xs text-gray-400">{translateSource(field.helpText)}</span>}
                    </label>
                  );
                }
                return (
                  <InputField
                    key={field.key}
                    label={translateSource(field.label)}
                    value={formValues[field.key] || ""}
                    onChange={(e) => handleFieldChange(field.key, e.currentTarget.value)}
                    placeholder={field.placeholder ? translateSource(field.placeholder) : ""}
                    type={typeof field.fieldType === "string" && field.fieldType === "password" ? "password" : "text"}
                    disabled={busy}
                  />
                );
              })}
            </div>
          )}

          {oauthSessionId ? (
            <div className="space-y-4">
              <div className="p-3 bg-yellow-900/30 border border-yellow-500/50 rounded text-yellow-300 text-sm">
                {translate("generated.e16e0b5717baf8a9")}
              </div>
              {storeError && (
                <div className="p-3 bg-red-900/30 border border-red-500/50 rounded text-red-300 text-sm space-y-2">
                  <p>{storeError}</p>
                  {storeError.toLowerCase().includes("still in progress") ? (
                    <p className="text-red-200">
                      {translate("generated.ed2d84eef1dc37cc")}
                    </p>
                  ) : (
                    <p className="text-red-200">
                      {translate("generated.00fb9e51e0f2476b")}
                    </p>
                  )}
                  <button
                    type="button"
                    className="text-xs underline text-red-400 hover:text-red-200"
                    onClick={clearStoreError}
                  >
                    {translate("generated.48845bff334a50a5")}
                  </button>
                </div>
              )}
              <div className="flex gap-2">
                <Button
                  variant="primary"
                  onClick={() => onCompleteOauthFlow(oauthSessionId)}
                  disabled={busy}
                  loading={busy}
                  loadingText={translate("generated.6196d07de6390a38")}
                >
                  {translate("generated.dcd37480d04ed6ce")}
                </Button>
                <Button
                  variant="ghost"
                  onClick={async () => {
                    clearStoreError();
                    await onCancelOauthFlow(oauthSessionId);
                  }}
                  disabled={busy}
                >
                  {translate("generated.130cba879fa80380")}
                </Button>
              </div>
            </div>
          ) : (
            <Button
              variant="primary"
              onClick={async () => {
                const clientId = formValues["client_id"] || "";
                const clientSecret = formValues["client_secret"] || null;
                if (!clientId.trim()) return;
                await onBeginOauthFlow(
                  selectedProvider.provider,
                  displayName || selectedProvider.label,
                  clientId.trim(),
                  clientSecret?.trim() || null,
                  Object.fromEntries(
                    Object.entries(formValues).filter(([key]) => !["client_id", "client_secret"].includes(key)),
                  ),
                );
              }}
              disabled={busy || !(formValues["client_id"] || "").trim()}
              loading={busy}
              loadingText={translate("generated.814c72d767dc6efb")}
            >
              {translate("generated.d2b79489c9e61b64")} {selectedProvider.label}
            </Button>
          )}
        </Card>
      )}
    </div>
  );
}
