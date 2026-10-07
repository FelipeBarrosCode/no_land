import { translate } from "../../lib/i18n";
import { useState } from "react";
import { getArcadeSoundEnabled, setArcadeSoundEnabled } from "../../lib/arcadeAudio";
import { Button } from "./Button";

export function ArcadeSoundToggle() {
  const [enabled, setEnabled] = useState(getArcadeSoundEnabled());

  function toggle() {
    const next = !enabled;
    setEnabled(next);
    setArcadeSoundEnabled(next);
  }

  return (
    <Button variant="ghost" onClick={toggle} className="min-w-[132px] justify-center">
      {enabled ? translate("generated.6b79f0dbcc3b6936") : translate("generated.05f70001b4e02396")}
    </Button>
  );
}
