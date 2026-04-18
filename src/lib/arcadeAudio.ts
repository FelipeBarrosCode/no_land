const STORAGE_KEY = "noland.arcadeSound";

let context: AudioContext | null = null;

function getContext(): AudioContext | null {
  if (typeof window === "undefined") {
    return null;
  }

  const AudioContextCtor = window.AudioContext || (window as typeof window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!AudioContextCtor) {
    return null;
  }

  if (!context) {
    context = new AudioContextCtor();
  }

  return context;
}

export function getArcadeSoundEnabled(): boolean {
  if (typeof window === "undefined") {
    return false;
  }

  const saved = window.localStorage.getItem(STORAGE_KEY);
  if (!saved) {
    return false;
  }

  return saved === "1";
}

export function setArcadeSoundEnabled(enabled: boolean): void {
  if (typeof window === "undefined") {
    return;
  }

  window.localStorage.setItem(STORAGE_KEY, enabled ? "1" : "0");
}

export function playArcadeClick(): void {
  if (!getArcadeSoundEnabled()) {
    return;
  }

  const audio = getContext();
  if (!audio) {
    return;
  }

  if (audio.state === "suspended") {
    void audio.resume();
  }

  const oscillator = audio.createOscillator();
  const gain = audio.createGain();

  oscillator.type = "square";
  oscillator.frequency.setValueAtTime(760, audio.currentTime);
  oscillator.frequency.exponentialRampToValueAtTime(420, audio.currentTime + 0.06);

  gain.gain.setValueAtTime(0.0001, audio.currentTime);
  gain.gain.exponentialRampToValueAtTime(0.045, audio.currentTime + 0.008);
  gain.gain.exponentialRampToValueAtTime(0.0001, audio.currentTime + 0.09);

  oscillator.connect(gain);
  gain.connect(audio.destination);
  oscillator.start();
  oscillator.stop(audio.currentTime + 0.1);
}
