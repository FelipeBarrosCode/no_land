import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import { AIPromptHelper } from "../../components/ui/AIPromptHelper";
import { APP_PROMPTS } from "../../prompts/appPrompts";
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
  onDisconnect: (profileId: string) => Promise<void>;
  onLoadProviders: () => Promise<void>;
  onLoadProfiles: () => Promise<void>;
  onBeginOauthFlow: (provider: string, displayName: string, clientId?: string, clientSecret?: string | null) => Promise<string | null>;
  onCompleteOauthFlow: (sessionId: string) => Promise<void>;
  activeInstanceId: number | null;
  onSyncActiveInstance: () => Promise<void>;
}

const CATEGORY_LABELS: Record<string, string> = {
  "object-storage": "Object Storage",
  "cloud-drives": "Cloud Drives",
  "enterprise-and-self-hosted": "Enterprise and Self-hosted",
};

export function SharedStorageSettingsV2({
  busy,
  providers,
  profiles,
  testResult,
  oauthSessionId,
  onConnectProvider,
  onTestConnection,
  onDisconnect,
  onLoadProviders,
  onLoadProfiles,
  onBeginOauthFlow,
  onCompleteOauthFlow,
  activeInstanceId,
  onSyncActiveInstance,
}: Props) {
  const [selectedProvider, setSelectedProvider] = useState<ProviderDefinition | null>(null);
  const [showProviderPicker, setShowProviderPicker] = useState(false);
  const [formValues, setFormValues] = useState<Record<string, string>>({});
  const [displayName, setDisplayName] = useState("");
  const [bucket, setBucket] = useState<string | null>(null);
  const [prefix, setPrefix] = useState<string | null>(null);

  useEffect(() => {
    void onLoadProviders();
    void onLoadProfiles();
  }, [onLoadProviders, onLoadProfiles]);

  const connectedProfile = profiles.length > 0 ? profiles[0] : null;

  const categorizedProviders = providers.reduce<Record<string, ProviderDefinition[]>>(
    (acc, p) => {
      const cat = p.category || "object-storage";
      if (!acc[cat]) acc[cat] = [];
      acc[cat].push(p);
      return acc;
    },
    {},
  );

  // Map provider slug to its tutorial prompt
  const PROVIDER_PROMPT_MAP: Record<string, string> = {
    backblaze_b2: APP_PROMPTS.storageBackblazeB2,
    amazon_s3: APP_PROMPTS.storageAmazonS3,
    cloudflare_r2: APP_PROMPTS.storageCloudflareR2,
    wasabi: APP_PROMPTS.storageWasabi,
    digital_ocean_spaces: APP_PROMPTS.storageDigitaloceanSpaces,
    generic_s3: APP_PROMPTS.storageGenericS3,
    google_drive: APP_PROMPTS.storageGoogleDrive,
    google_cloud_storage: APP_PROMPTS.storageGoogleCloud,
    microsoft_one_drive: APP_PROMPTS.storageOnedrive,
    dropbox: APP_PROMPTS.storageDropbox,
    box: APP_PROMPTS.storageBox,
    azure_blob: APP_PROMPTS.storageAzure,
    sftp: APP_PROMPTS.storageSftp,
    webdav: APP_PROMPTS.storageWebdav,
  };

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
    await onConnectProvider(
      selectedProvider.provider,
      formValues,
      bucket,
      prefix,
      displayName,
    );
    setSelectedProvider(null);
    setFormValues({});
  }

  return (
    <div className="space-y-6">
      {/* Empty state */}
      {!connectedProfile && !selectedProvider && (
        <Card className="p-6">
          <div className="flex items-center gap-2 mb-4">
            <h3 className="text-lg font-display text-neon-cyan">
              Shared Storage
            </h3>
            <AIPromptHelper
              topic="Shared Storage Overview"
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
            Keep your games, applications, saves, settings, and mods
            available across your Noland instances.
          </p>
          <p className="text-sm text-gray-500 mb-4">
            No storage provider connected.
          </p>
          <Button variant="primary" onClick={() => setShowProviderPicker(true)} disabled={busy}>
            Connect Storage Provider
          </Button>
        </Card>
      )}

      {/* Connected state */}
      {connectedProfile && !selectedProvider && (
        <Card className="p-6">
          <h3 className="text-lg font-display text-neon-cyan mb-4">
            Shared Storage
          </h3>
          <div className="space-y-3 mb-6">
            <div className="flex items-center justify-between">
              <span className="text-sm text-gray-400">Provider</span>
              <span className="text-sm text-neon-lime">{connectedProfile.providerLabel}</span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-sm text-gray-400">Status</span>
              <span className="text-sm text-green-400">Connected</span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-sm text-gray-400">Repository</span>
              <span className="text-sm text-gray-200 font-mono">{connectedProfile.id.substring(0, 12)}...</span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-sm text-gray-400">Display Name</span>
              <span className="text-sm text-gray-200">{connectedProfile.displayName}</span>
            </div>
          </div>

          {testResult && (
            <div className={`mb-4 p-3 rounded border text-sm ${
              testResult.error
                ? "bg-red-900/30 border-red-500/50 text-red-300"
                : "bg-green-900/30 border-green-500/50 text-green-300"
            }`}>
              {testResult.error || "Connection test passed successfully"}
              {testResult.latencyMs != null && (
                <span className="ml-2 text-gray-400">
                  ({testResult.latencyMs}ms latency)
                </span>
              )}
            </div>
          )}

          <div className="mb-4 rounded border border-[#3f476c] bg-[#0b0f23]/60 p-3">
            <p className="text-sm text-gray-200">Remote instance sync test</p>
            <p className="mt-1 text-xs text-gray-500">
              Run a real save-to-cloud flow from the currently selected remote instance to confirm Shared Storage is actually writing data.
              {activeInstanceId ? ` Active instance: #${activeInstanceId}.` : " Select or start a server first."}
            </p>
            <div className="mt-3">
              <Button
                variant="secondary"
                onClick={() => onSyncActiveInstance()}
                disabled={busy || !activeInstanceId}
                loading={busy}
                loadingText="Syncing..."
              >
                Sync Active Instance Now
              </Button>
            </div>
          </div>

          <div className="flex gap-2 flex-wrap">
            <Button
              variant="secondary"
              onClick={() => onTestConnection(connectedProfile.id)}
              disabled={busy}
              loading={busy}
              loadingText="Testing..."
            >
              Test Connection
            </Button>
            <Button
              variant="ghost"
              onClick={() => setShowProviderPicker(true)}
              disabled={busy}
            >
              Change Provider
            </Button>
            <Button
              variant="ghost"
              className="text-red-400"
              onClick={() => onDisconnect(connectedProfile.id)}
              disabled={busy}
            >
              Disconnect
            </Button>
          </div>
        </Card>
      )}

      {/* Provider picker */}
      {showProviderPicker && (
        <Card className="p-6">
          <div className="flex items-center justify-between mb-4">
            <h3 className="text-lg font-display text-neon-cyan">Select Storage Provider</h3>
            <Button variant="ghost" onClick={() => setShowProviderPicker(false)}>
              Back
            </Button>
          </div>
          <div className="space-y-6">
            {Object.entries(categorizedProviders).map(([category, catProviders]) => (
              <div key={category}>
                <h4 className="text-xs font-display uppercase tracking-wider text-gray-500 mb-2">
                  {CATEGORY_LABELS[category] || category}
                </h4>
                <div className="grid gap-2 sm:grid-cols-2">
                  {catProviders.map((provider) => (
                    <button
                      key={provider.provider}
                      className="text-left p-3 border border-[#3f476c] rounded bg-[#0b0f23] hover:border-neon-cyan hover:bg-[#121731] transition text-sm group"
                      onClick={() => handleProviderSelect(provider)}
                    >
                      <div className="flex items-center justify-between gap-2">
                        <p className="text-gray-200 font-medium">{provider.label}</p>
                        {PROVIDER_PROMPT_MAP[provider.provider] && (
                          <AIPromptHelper
                            topic={`${provider.label} Setup Guide`}
                            promptText={PROVIDER_PROMPT_MAP[provider.provider]}
                            variant="icon"
                          />
                        )}
                      </div>
                      <p className="text-xs text-gray-500 mt-1">{provider.description}</p>
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </div>
        </Card>
      )}

      {/* Provider form - static credentials */}
      {selectedProvider && !connectedProfile && !selectedProvider.isOauth && (
        <Card className="p-6">
          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-2">
              <h3 className="text-lg font-display text-neon-cyan">
                Configure {selectedProvider.label}
              </h3>
              {PROVIDER_PROMPT_MAP[selectedProvider.provider] && (
                <AIPromptHelper
                  topic={`${selectedProvider.label} Setup Guide`}
                  promptText={PROVIDER_PROMPT_MAP[selectedProvider.provider]}
                  variant="icon"
                />
              )}
            </div>
            <Button variant="ghost" onClick={() => setSelectedProvider(null)}>
              Back to Providers
            </Button>
          </div>

          <div className="space-y-4">
            <InputField
              label="Display Name"
              value={displayName}
              onChange={(e) => setDisplayName(e.currentTarget.value)}
              placeholder="My Backup Storage"
            />

            {selectedProvider.fields.map((field) => (
              <InputField
                key={field.key}
                label={field.label}
                value={formValues[field.key] || ""}
                onChange={(e) => handleFieldChange(field.key, e.currentTarget.value)}
                placeholder={field.placeholder || ""}
                type={typeof field.fieldType === "string" && field.fieldType === "password" ? "password" : "text"}
                disabled={busy}
              />
            ))}

            <InputField
              label="Bucket (optional)"
              value={bucket || ""}
              onChange={(e) => setBucket(e.currentTarget.value || null)}
              placeholder="Bucket name"
            />
            <InputField
              label="Prefix (optional)"
              value={prefix || ""}
              onChange={(e) => setPrefix(e.currentTarget.value || null)}
              placeholder="repositories/"
            />

            <Button
              variant="primary"
              onClick={handleConnect}
              disabled={busy || displayName.trim().length < 2}
              loading={busy}
              loadingText="Connecting..."
            >
              Connect {selectedProvider.label}
            </Button>
          </div>
        </Card>
      )}

      {/* Provider form - OAuth */}
      {selectedProvider && !connectedProfile && selectedProvider.isOauth && (
        <Card className="p-6">
          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-2">
              <h3 className="text-lg font-display text-neon-cyan">
                Authorize {selectedProvider.label}
              </h3>
              {PROVIDER_PROMPT_MAP[selectedProvider.provider] && (
                <AIPromptHelper
                  topic={`${selectedProvider.label} Setup Guide`}
                  promptText={PROVIDER_PROMPT_MAP[selectedProvider.provider]}
                  variant="icon"
                />
              )}
            </div>
            <Button variant="ghost" onClick={() => setSelectedProvider(null)}>
              Back to Providers
            </Button>
          </div>

          <p className="text-sm text-gray-400 mb-4">
            {selectedProvider.label} requires you to create your own OAuth application.
            Click the robot icon for step-by-step instructions, then enter your credentials below.
          </p>

          {!oauthSessionId && (
            <div className="space-y-4 mb-4">
              <InputField
                label="Client ID"
                value={formValues["client_id"] || ""}
                onChange={(e) => handleFieldChange("client_id", e.currentTarget.value)}
                placeholder="Your OAuth Client ID from the developer console"
              />
              <InputField
                label="Client Secret"
                value={formValues["client_secret"] || ""}
                onChange={(e) => handleFieldChange("client_secret", e.currentTarget.value)}
                placeholder="Your OAuth Client Secret"
                type="password"
              />
            </div>
          )}

          {oauthSessionId ? (
            <div className="space-y-4">
              <div className="p-3 bg-yellow-900/30 border border-yellow-500/50 rounded text-yellow-300 text-sm">
                Authorization in progress. Complete the sign-in in your browser, then click below.
              </div>
              <div className="flex gap-2">
                <Button
                  variant="primary"
                  onClick={() => onCompleteOauthFlow(oauthSessionId)}
                  disabled={busy}
                  loading={busy}
                  loadingText="Completing..."
                >
                  Complete Authorization
                </Button>
                <Button
                  variant="ghost"
                  onClick={() => {
                    setSelectedProvider(null);
                  }}
                >
                  Cancel
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
                );
              }}
              disabled={busy || !(formValues["client_id"] || "").trim()}
              loading={busy}
              loadingText="Opening browser..."
            >
              Continue with {selectedProvider.label}
            </Button>
          )}
        </Card>
      )}
    </div>
  );
}
