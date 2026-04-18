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
      {enabled ? "8-Bit Sound: ON" : "8-Bit Sound: OFF"}
    </Button>
  );
}
