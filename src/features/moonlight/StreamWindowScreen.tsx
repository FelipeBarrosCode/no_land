import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  moonlightActivateNativeMouseCapture,
  moonlightDeactivateNativeMouseCapture,
  moonlightGetActiveInputMode,
  moonlightSendAbsoluteMouse,
  moonlightSendKeyboard,
  moonlightSendMouseButton,
  moonlightSendRelativeMouse,
} from "../../lib/backend";

const MOUSE_BUTTON_MAP: Record<number, number> = {
  0: 0x01,
  1: 0x02,
  2: 0x03,
  3: 0x04,
  4: 0x05,
};

const KEYBOARD_MODIFIER_SHIFT = 0x01;
const KEYBOARD_MODIFIER_CTRL = 0x02;
const KEYBOARD_MODIFIER_ALT = 0x04;
const KEYBOARD_MODIFIER_META = 0x08;
const UNGRAB_SHORTCUT_CODES = new Set(["KeyZ", "KeyQ"]);

const KEY_CODE_TO_VK: Record<string, number> = {
  KeyA: 0x41,
  KeyB: 0x42,
  KeyC: 0x43,
  KeyD: 0x44,
  KeyE: 0x45,
  KeyF: 0x46,
  KeyG: 0x47,
  KeyH: 0x48,
  KeyI: 0x49,
  KeyJ: 0x4a,
  KeyK: 0x4b,
  KeyL: 0x4c,
  KeyM: 0x4d,
  KeyN: 0x4e,
  KeyO: 0x4f,
  KeyP: 0x50,
  KeyQ: 0x51,
  KeyR: 0x52,
  KeyS: 0x53,
  KeyT: 0x54,
  KeyU: 0x55,
  KeyV: 0x56,
  KeyW: 0x57,
  KeyX: 0x58,
  KeyY: 0x59,
  KeyZ: 0x5a,
  Digit0: 0x30,
  Digit1: 0x31,
  Digit2: 0x32,
  Digit3: 0x33,
  Digit4: 0x34,
  Digit5: 0x35,
  Digit6: 0x36,
  Digit7: 0x37,
  Digit8: 0x38,
  Digit9: 0x39,
  Enter: 0x0d,
  NumpadEnter: 0x0d,
  Numpad0: 0x60,
  Numpad1: 0x61,
  Numpad2: 0x62,
  Numpad3: 0x63,
  Numpad4: 0x64,
  Numpad5: 0x65,
  Numpad6: 0x66,
  Numpad7: 0x67,
  Numpad8: 0x68,
  Numpad9: 0x69,
  NumpadAdd: 0x6b,
  NumpadSubtract: 0x6d,
  NumpadMultiply: 0x6a,
  NumpadDivide: 0x6f,
  NumpadDecimal: 0x6e,
  Tab: 0x09,
  Escape: 0x1b,
  Space: 0x20,
  Backspace: 0x08,
  CapsLock: 0x14,
  Delete: 0x2e,
  Insert: 0x2d,
  Home: 0x24,
  End: 0x23,
  PageUp: 0x21,
  PageDown: 0x22,
  Pause: 0x13,
  PrintScreen: 0x2c,
  ScrollLock: 0x91,
  NumLock: 0x90,
  ArrowLeft: 0x25,
  ArrowUp: 0x26,
  ArrowRight: 0x27,
  ArrowDown: 0x28,
  ShiftLeft: 0x10,
  ShiftRight: 0x10,
  ControlLeft: 0x11,
  ControlRight: 0x11,
  AltLeft: 0x12,
  AltRight: 0x12,
  MetaLeft: 0x5b,
  MetaRight: 0x5c,
  F1: 0x70,
  F2: 0x71,
  F3: 0x72,
  F4: 0x73,
  F5: 0x74,
  F6: 0x75,
  F7: 0x76,
  F8: 0x77,
  F9: 0x78,
  F10: 0x79,
  F11: 0x7a,
  F12: 0x7b,
  Minus: 0xbd,
  Equal: 0xbb,
  BracketLeft: 0xdb,
  BracketRight: 0xdd,
  Backslash: 0xdc,
  Semicolon: 0xba,
  Quote: 0xde,
  Backquote: 0xc0,
  Comma: 0xbc,
  Period: 0xbe,
  Slash: 0xbf,
};

function codeToModifier(code: string): number {
  switch (code) {
    case "ShiftLeft":
    case "ShiftRight":
      return KEYBOARD_MODIFIER_SHIFT;
    case "ControlLeft":
    case "ControlRight":
      return KEYBOARD_MODIFIER_CTRL;
    case "AltLeft":
    case "AltRight":
      return KEYBOARD_MODIFIER_ALT;
    case "MetaLeft":
    case "MetaRight":
      return KEYBOARD_MODIFIER_META;
    default:
      return 0;
  }
}

function pressedSetToModifiers(pressedCodes: Iterable<string>): number {
  let modifiers = 0;
  for (const code of pressedCodes) {
    modifiers |= codeToModifier(code);
  }
  return modifiers;
}

type CaptureMode =
  | "none"
  | "native-relative"
  | "pointer-lock"
  | "window-absolute";

export function StreamWindowScreen() {
  const overlayRef = useRef<HTMLDivElement | null>(null);
  const appWindowRef = useRef(getCurrentWindow());
  const pressedKeysRef = useRef<Set<string>>(new Set());
  const pressedMouseButtonsRef = useRef<Set<number>>(new Set());
  const relativeMouseDeltaRef = useRef({ deltaX: 0, deltaY: 0 });
  const relativeMouseFrameRef = useRef<number | null>(null);
  const mouseSendInFlightRef = useRef(false);
  const captureModeRef = useRef<CaptureMode>("none");
  const preferredMouseModeRef = useRef<"relative" | "absolute">("relative");
  const [captured, setCaptured] = useState(false);
  const [captureRequested, setCaptureRequested] = useState(false);
  const [captureMode, setCaptureMode] = useState<CaptureMode>("none");
  const [lastError, setLastError] = useState<string | null>(null);

  const captureHint = useMemo(() => {
    if (captured) {
      if (captureMode === "native-relative") {
        return "Input captured — native mouse mode active · Ctrl+Alt+Shift+Z to release";
      }
      if (captureMode === "pointer-lock") {
        return "Input captured — pointer lock active · Ctrl+Alt+Shift+Z to release";
      }
      if (captureMode === "window-absolute") {
        return "Input captured — stream window absolute mouse mode active · Ctrl+Alt+Shift+Z to release";
      }
      return "Input captured — Ctrl+Alt+Shift+Z to release";
    }
    if (captureRequested) {
      return "Waiting for pointer lock…";
    }
    return "Click to capture input";
  }, [captureMode, captureRequested, captured]);

  const flushRelativeMouse = useCallback(() => {
    relativeMouseFrameRef.current = null;
    if (mouseSendInFlightRef.current) {
      return;
    }

    const { deltaX, deltaY } = relativeMouseDeltaRef.current;
    if (deltaX === 0 && deltaY === 0) {
      return;
    }

    relativeMouseDeltaRef.current = { deltaX: 0, deltaY: 0 };
    mouseSendInFlightRef.current = true;

    void moonlightSendRelativeMouse({ deltaX, deltaY })
      .catch((error: unknown) => {
        const message = error instanceof Error ? error.message : String(error);
        setLastError(message);
      })
      .finally(() => {
        mouseSendInFlightRef.current = false;
        const pending = relativeMouseDeltaRef.current;
        if (pending.deltaX !== 0 || pending.deltaY !== 0) {
          relativeMouseFrameRef.current = window.requestAnimationFrame(
            flushRelativeMouse,
          );
        }
      });
  }, []);

  const scheduleRelativeMouseFlush = useCallback(() => {
    if (relativeMouseFrameRef.current !== null) {
      return;
    }
    relativeMouseFrameRef.current = window.requestAnimationFrame(flushRelativeMouse);
  }, [flushRelativeMouse]);

  const setWindowCapture = useCallback(async (active: boolean) => {
    try {
      await appWindowRef.current.setCursorGrab(active);
    } catch {
      // pointer lock remains the primary path when native grab is unavailable
    }
    try {
      await appWindowRef.current.setCursorVisible(!active);
    } catch {
      // ignore cursor visibility failures
    }
  }, []);

  const activateWindowAbsoluteFallback = useCallback(async (reason?: string | null) => {
    setCaptureRequested(false);
    captureModeRef.current = "window-absolute";
    setCaptureMode("window-absolute");
    setCaptured(true);
    overlayRef.current?.focus();
    if (reason) {
      setLastError(reason);
    }
    await setWindowCapture(true);
  }, [setWindowCapture]);

  const releasePressedInputs = useCallback(() => {
    const pressedCodes = Array.from(pressedKeysRef.current);
    const pressedMouseButtons = Array.from(pressedMouseButtonsRef.current);

    pressedKeysRef.current.clear();
    pressedMouseButtonsRef.current.clear();
    relativeMouseDeltaRef.current = { deltaX: 0, deltaY: 0 };

    for (const button of pressedMouseButtons) {
      void moonlightSendMouseButton({ button, pressed: false }).catch(
        (error: unknown) => {
          const message = error instanceof Error ? error.message : String(error);
          setLastError(message);
        },
      );
    }

    for (const code of pressedCodes) {
      const virtualKey = KEY_CODE_TO_VK[code];
      if (virtualKey === undefined) {
        continue;
      }
      const remainingPressed = new Set(pressedCodes.filter((value) => value !== code));
      void moonlightSendKeyboard({
        virtualKey,
        pressed: false,
        modifiers: pressedSetToModifiers(remainingPressed),
      }).catch((error: unknown) => {
        const message = error instanceof Error ? error.message : String(error);
        setLastError(message);
      });
    }
  }, []);

  const releaseCapture = useCallback(() => {
    const previousMode = captureModeRef.current;
    setCaptureRequested(false);
    captureModeRef.current = "none";
    setCaptureMode("none");
    setCaptured(false);
    if (document.pointerLockElement === overlayRef.current) {
      document.exitPointerLock();
    }
    if (previousMode === "native-relative") {
      void moonlightDeactivateNativeMouseCapture().catch((error: unknown) => {
        const message = error instanceof Error ? error.message : String(error);
        setLastError(message);
      });
    }
    void setWindowCapture(false);
    releasePressedInputs();
  }, [releasePressedInputs, setWindowCapture]);

  useEffect(() => {
    document.documentElement.classList.add("stream-window");
    document.body.classList.add("stream-window");
    void moonlightGetActiveInputMode()
      .then((mouseMode) => {
        if (mouseMode) {
          preferredMouseModeRef.current = mouseMode;
        }
      })
      .catch(() => {
        preferredMouseModeRef.current = "relative";
      });
    return () => {
      document.documentElement.classList.remove("stream-window");
      document.body.classList.remove("stream-window");
      if (relativeMouseFrameRef.current !== null) {
        window.cancelAnimationFrame(relativeMouseFrameRef.current);
      }
      releaseCapture();
    };
  }, [releaseCapture]);

  useEffect(() => {
    const handlePointerLockChange = () => {
      const isPointerLocked = document.pointerLockElement === overlayRef.current;
      if (isPointerLocked) {
        captureModeRef.current = "pointer-lock";
        setCaptureMode("pointer-lock");
        setCaptured(true);
        setCaptureRequested(false);
        overlayRef.current?.focus();
        setLastError(null);
        return;
      }

      if (captureModeRef.current === "pointer-lock") {
        captureModeRef.current = "none";
        setCaptureMode("none");
        setCaptured(false);
        void setWindowCapture(false);
        releasePressedInputs();
      }
    };

    const handlePointerLockError = () => {
      if (captureModeRef.current !== "none" || !captureRequested) {
        return;
      }
      void activateWindowAbsoluteFallback(
        "Pointer lock denied — using stream window absolute mouse fallback",
      );
    };

    const handleWindowBlur = () => {
      releaseCapture();
    };

    const handleVisibilityChange = () => {
      if (document.visibilityState !== "visible") {
        releaseCapture();
      }
    };

    document.addEventListener("pointerlockchange", handlePointerLockChange);
    document.addEventListener("pointerlockerror", handlePointerLockError);
    document.addEventListener("visibilitychange", handleVisibilityChange);
    window.addEventListener("blur", handleWindowBlur);

    return () => {
      document.removeEventListener("pointerlockchange", handlePointerLockChange);
      document.removeEventListener("pointerlockerror", handlePointerLockError);
      document.removeEventListener("visibilitychange", handleVisibilityChange);
      window.removeEventListener("blur", handleWindowBlur);
    };
  }, [activateWindowAbsoluteFallback, captureRequested, releaseCapture, releasePressedInputs, setWindowCapture]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (!captured) {
        return;
      }

      const releaseComboPressed =
        event.ctrlKey &&
        event.altKey &&
        event.shiftKey &&
        UNGRAB_SHORTCUT_CODES.has(event.code);

      if (releaseComboPressed) {
        event.preventDefault();
        releaseCapture();
        return;
      }

      const virtualKey = KEY_CODE_TO_VK[event.code];
      if (virtualKey === undefined) {
        return;
      }

      event.preventDefault();
      if (event.repeat || pressedKeysRef.current.has(event.code)) {
        return;
      }

      const nextPressed = new Set(pressedKeysRef.current);
      nextPressed.add(event.code);
      pressedKeysRef.current = nextPressed;

      void moonlightSendKeyboard({
        virtualKey,
        pressed: true,
        modifiers: pressedSetToModifiers(nextPressed),
      }).catch((error: unknown) => {
        const message = error instanceof Error ? error.message : String(error);
        setLastError(message);
      });
    };

    const handleKeyUp = (event: KeyboardEvent) => {
      if (!captured) {
        return;
      }

      const virtualKey = KEY_CODE_TO_VK[event.code];
      if (virtualKey === undefined) {
        return;
      }

      event.preventDefault();
      const nextPressed = new Set(pressedKeysRef.current);
      nextPressed.delete(event.code);
      pressedKeysRef.current = nextPressed;

      void moonlightSendKeyboard({
        virtualKey,
        pressed: false,
        modifiers: pressedSetToModifiers(nextPressed),
      }).catch((error: unknown) => {
        const message = error instanceof Error ? error.message : String(error);
        setLastError(message);
      });
    };

    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("keyup", handleKeyUp);

    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("keyup", handleKeyUp);
    };
  }, [captured, releaseCapture]);

  const requestCapture = useCallback(async () => {
    const overlay = overlayRef.current;
    if (!overlay) {
      return;
    }

    if (
      document.pointerLockElement === overlay ||
      captureModeRef.current === "native-relative" ||
      captureModeRef.current === "window-absolute"
    ) {
      overlay.focus();
      return;
    }

    setCaptureRequested(true);
    setLastError(null);

    try {
      await appWindowRef.current.setFocus();
    } catch {
      // best effort only
    }

    overlay.focus();

    if (preferredMouseModeRef.current === "absolute") {
      await activateWindowAbsoluteFallback(null);
      return;
    }

    try {
      const nativeCaptureActivated = await moonlightActivateNativeMouseCapture();
      if (nativeCaptureActivated) {
        setCaptureRequested(false);
        captureModeRef.current = "native-relative";
        setCaptureMode("native-relative");
        setCaptured(true);
        setLastError(null);
        return;
      }
    } catch {
      // fall through to pointer lock and absolute fallback
    }

    try {
      const pointerLockResult = overlay.requestPointerLock();
      if (pointerLockResult && typeof pointerLockResult.then === "function") {
        await pointerLockResult;
      }
    } catch (error: unknown) {
      const message = error instanceof Error ? error.message : String(error);
      await activateWindowAbsoluteFallback(
        `Pointer lock unavailable — using stream window absolute mouse fallback (${message})`,
      );
    }
  }, [activateWindowAbsoluteFallback]);

  const handleMouseMove = useCallback(
    (event: React.MouseEvent<HTMLDivElement>) => {
      if (!captured) {
        return;
      }

      if (captureModeRef.current === "native-relative") {
        return;
      }

      if (captureModeRef.current === "window-absolute") {
        const overlay = overlayRef.current;
        if (!overlay) {
          return;
        }

        const rect = overlay.getBoundingClientRect();
        const x = Math.max(0, Math.min(rect.width, event.clientX - rect.left));
        const y = Math.max(0, Math.min(rect.height, event.clientY - rect.top));
        const referenceWidth = Math.max(1, Math.round(rect.width));
        const referenceHeight = Math.max(1, Math.round(rect.height));

        void moonlightSendAbsoluteMouse({
          x: Math.round(x),
          y: Math.round(y),
          referenceWidth,
          referenceHeight,
        }).catch((error: unknown) => {
          const message = error instanceof Error ? error.message : String(error);
          setLastError(message);
        });
        return;
      }

      relativeMouseDeltaRef.current.deltaX += event.movementX;
      relativeMouseDeltaRef.current.deltaY += event.movementY;
      scheduleRelativeMouseFlush();
    },
    [captured, scheduleRelativeMouseFlush],
  );

  const handleMouseButton = useCallback(
    (event: React.MouseEvent<HTMLDivElement>, pressed: boolean) => {
      const mappedButton = MOUSE_BUTTON_MAP[event.button];
      if (captureModeRef.current === "native-relative") {
        return;
      }
      if (!captured || mappedButton === undefined) {
        if (!captured && pressed) {
          void requestCapture();
        }
        return;
      }

      event.preventDefault();
      const nextPressed = new Set(pressedMouseButtonsRef.current);
      if (pressed) {
        nextPressed.add(mappedButton);
      } else {
        nextPressed.delete(mappedButton);
      }
      pressedMouseButtonsRef.current = nextPressed;

      void moonlightSendMouseButton({ button: mappedButton, pressed }).catch(
        (error: unknown) => {
          const message = error instanceof Error ? error.message : String(error);
          setLastError(message);
        },
      );
    },
    [captured, requestCapture],
  );

  return (
    <main className="relative h-screen w-screen overflow-hidden bg-transparent text-white">
      <div
        ref={overlayRef}
        className="absolute inset-0 flex select-none outline-none"
        onClick={() => {
          if (!captured) {
            void requestCapture();
          }
        }}
        onAuxClick={(event) => event.preventDefault()}
        onContextMenu={(event) => event.preventDefault()}
        onMouseDown={(event) => handleMouseButton(event, true)}
        onMouseMove={handleMouseMove}
        onMouseUp={(event) => handleMouseButton(event, false)}
        tabIndex={0}
        style={{ cursor: captured ? "none" : "default" }}
      >
        <div className="pointer-events-none absolute inset-x-0 top-0 flex justify-center p-4">
          <div className="rounded border border-cyan-300/70 bg-slate-950/70 px-4 py-2 font-mono text-sm shadow-[0_0_18px_rgba(34,211,238,0.25)] backdrop-blur-sm">
            {captureHint}
          </div>
        </div>

        <div className="pointer-events-none absolute bottom-4 right-4 max-w-md rounded border border-slate-700/80 bg-slate-950/65 px-3 py-2 font-mono text-xs text-slate-100 shadow-[0_0_18px_rgba(15,23,42,0.35)] backdrop-blur-sm">
          <div>
            {captured
              ? `Mouse + keyboard forwarding active (${captureMode === "native-relative" ? "native relative mouse" : captureMode === "pointer-lock" ? "pointer lock" : captureMode === "window-absolute" ? "stream window absolute mouse" : "captured"})`
              : "Native video stays behind this transparent overlay"}
          </div>
          <div className="mt-1 text-slate-300">
            Click to capture · Ctrl+Alt+Shift+Z to release
          </div>
          <div className="mt-1 text-slate-400">
            Ctrl+Alt+Shift+Q remains accepted as a compatibility alias
          </div>
          {lastError ? (
            <div className="mt-2 text-rose-300">Input bridge error: {lastError}</div>
          ) : null}
        </div>
      </div>
    </main>
  );
}
