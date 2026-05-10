import { useEffect, useState } from "react";
import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import { InputField } from "../../components/ui/InputField";
import type {
  SharedStorageSettingsResponse,
  SharedStorageSettingsUpdate
} from "../../lib/types";

interface Props {
  settings: SharedStorageSettingsResponse | null;
  busy: boolean;
  onSave: (payload: SharedStorageSettingsUpdate) => Promise<void>;
  onTest: () => Promise<string | null>;
}

export function SharedStorageSettings({
  settings,
  busy,
  onSave,
  onTest
}: Props) {
  const [enabled, setEnabled] = useState(false);
  const [keyId, setKeyId] = useState("");
  const [appKey, setAppKey] = useState("");
  const [bucketName, setBucketName] = useState("noland");
  const [remoteName, setRemoteName] = useState("b2");
  const [destinationPrefix, setDestinationPrefix] = useState("vm-backup");
  const [cryptPassword, setCryptPassword] = useState("");
  const [testResult, setTestResult] = useState<string | null>(null);
  const [testError, setTestError] = useState<string | null>(null);

  useEffect(() => {
    if (settings) {
      setEnabled(settings.enabled);
      setKeyId(settings.backblazeKeyId);
      setBucketName(settings.bucketName);
      setRemoteName(settings.remoteName);
      setDestinationPrefix(settings.destinationPrefix);
    }
  }, [settings]);

  const handleSave = async () => {
    setTestResult(null);
    setTestError(null);
    await onSave({
      enabled,
      backblazeKeyId: keyId.trim(),
      backblazeApplicationKey: appKey.trim(),
      bucketName: bucketName.trim() || "noland",
      remoteName: remoteName.trim() || "b2",
      destinationPrefix: destinationPrefix.trim() || "vm-backup",
      cryptPassword: cryptPassword.trim() || undefined
    });
    setAppKey("");
    setCryptPassword("");
  };

  const handleTest = async () => {
    setTestResult(null);
    setTestError(null);
    const result = await onTest();
    if (result) {
      setTestResult(result);
    } else {
      setTestError("Configuration test failed. Check your credentials and try again.");
    }
  };

  return (
    <div className="space-y-6">
      <Card className="p-6">
        <h3 className="text-lg font-display text-neon-cyan mb-4">
          Backblaze B2 Cloud Backup
        </h3>
        <p className="text-sm text-gray-400 mb-6">
          Configure automatic backups of your VM user files to Backblaze B2 cloud storage.
          Only changed files are uploaded on each run.
        </p>

        <div className="space-y-4">
          <div className="flex items-center gap-3">
            <input
              type="checkbox"
              id="backup-enabled"
              checked={enabled}
              onChange={(e) => setEnabled(e.target.checked)}
              className="w-4 h-4 accent-neon-cyan"
            />
            <label htmlFor="backup-enabled" className="text-sm text-gray-200">
              Enable automatic cloud backups
            </label>
          </div>

          <InputField
            label="Backblaze Key ID"
            value={keyId}
            onChange={(event) => setKeyId(event.target.value)}
            placeholder="Your Backblaze key ID"
            disabled={busy}
          />

          <InputField
            label="Backblaze Application Key"
            value={appKey}
            onChange={(event) => setAppKey(event.target.value)}
            placeholder="Your Backblaze application key (secret)"
            type="password"
            disabled={busy}
          />

          <InputField
            label="Bucket Name"
            value={bucketName}
            onChange={(event) => setBucketName(event.target.value)}
            placeholder="noland"
            disabled={busy}
          />

          <InputField
            label="rclone Remote Name"
            value={remoteName}
            onChange={(event) => setRemoteName(event.target.value)}
            placeholder="b2"
            disabled={busy}
          />

          <InputField
            label="Destination Prefix"
            value={destinationPrefix}
            onChange={(event) => setDestinationPrefix(event.target.value)}
            placeholder="vm-backup"
            disabled={busy}
          />

          <InputField
            label="Encryption Password (optional)"
            value={cryptPassword}
            onChange={(event) => setCryptPassword(event.target.value)}
            placeholder="Leave empty for no encryption"
            type="password"
            disabled={busy}
          />
          {settings?.cryptPasswordSet && !cryptPassword && (
            <p className="text-xs text-neon-cyan">
              Encryption is already configured. Enter a new password to change it, or leave empty to keep existing.
            </p>
          )}
        </div>

        {testResult && (
          <div className="mt-4 p-3 bg-green-900/30 border border-green-500/50 rounded text-green-300 text-sm">
            {testResult}
          </div>
        )}

        {testError && (
          <div className="mt-4 p-3 bg-red-900/30 border border-red-500/50 rounded text-red-300 text-sm">
            {testError}
          </div>
        )}

        <div className="mt-6 flex gap-3">
          <Button
            variant="primary"
            onClick={handleSave}
            disabled={busy}
            loading={busy}
            loadingText="Saving..."
          >
            Save Settings
          </Button>
          <Button
            variant="secondary"
            onClick={handleTest}
            disabled={busy || !keyId.trim()}
            loading={busy}
            loadingText="Testing..."
          >
            Test Connection
          </Button>
        </div>
      </Card>
    </div>
  );
}
