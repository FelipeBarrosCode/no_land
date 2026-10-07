import { translate } from "../../lib/i18n";
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
          {translate("generated.2bcf3907d3fc476e")}
        </h3>
        <p className="text-sm text-gray-400 mb-6">
          {translate("generated.2c799e0af94550e9")}
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
              {translate("generated.a6a6fef22fc00560")}
            </label>
          </div>

          <InputField
            label={translate("generated.1c88cedec1241047")}
            value={keyId}
            onChange={(event) => setKeyId(event.target.value)}
            placeholder={translate("generated.b9a66e510468f6bb")}
            disabled={busy}
          />

          <InputField
            label={translate("generated.18492bf90c8e067e")}
            value={appKey}
            onChange={(event) => setAppKey(event.target.value)}
            placeholder={translate("generated.b830ed73c1106680")}
            type="password"
            disabled={busy}
          />

          <InputField
            label={translate("generated.f104ff0ab0b1fb9f")}
            value={bucketName}
            onChange={(event) => setBucketName(event.target.value)}
            placeholder={translate("generated.9c1a1a0b3fbde6f2")}
            disabled={busy}
          />

          <InputField
            label={translate("generated.6be23a2a79556eb1")}
            value={remoteName}
            onChange={(event) => setRemoteName(event.target.value)}
            placeholder={translate("generated.4814d92093ac8a0f")}
            disabled={busy}
          />

          <InputField
            label={translate("generated.26cacfd4800d06b5")}
            value={destinationPrefix}
            onChange={(event) => setDestinationPrefix(event.target.value)}
            placeholder={translate("generated.582ee20d1881e627")}
            disabled={busy}
          />

          <InputField
            label={translate("generated.c717199ca8c27c62")}
            value={cryptPassword}
            onChange={(event) => setCryptPassword(event.target.value)}
            placeholder={translate("generated.4971759ad95db6ea")}
            type="password"
            disabled={busy}
          />
          {settings?.cryptPasswordSet && !cryptPassword && (
            <p className="text-xs text-neon-cyan">
              {translate("generated.cb741282e0ec321c")}
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
            loadingText={translate("generated.dc85af8f2b1d0d67")}
          >
            {translate("generated.ec92e1dc9bb3bf7b")}
          </Button>
          <Button
            variant="secondary"
            onClick={handleTest}
            disabled={busy || !keyId.trim()}
            loading={busy}
            loadingText={translate("generated.6c02a28421f8ad91")}
          >
            {translate("generated.c02977b07ec93816")}
          </Button>
        </div>
      </Card>
    </div>
  );
}
