import { useState, useRef } from "react";

interface Props {
  topic: string;
  promptText: string;
  variant?: "icon" | "text" | "both";
  className?: string;
}

const RobotIcon = () => (
  <svg
    xmlns="http://www.w3.org/2000/svg"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
    className="h-4 w-4 text-neon-cyan"
  >
    <rect x="3" y="11" width="18" height="10" rx="2" />
    <circle cx="12" cy="5" r="2" />
    <path d="M12 7v4" />
    <line x1="8" y1="16" x2="8" y2="16" />
    <line x1="16" y1="16" x2="16" y2="16" />
    <path d="M2 14h1" />
    <path d="M21 14h1" />
  </svg>
);

export function AIPromptHelper({ topic, promptText, variant = "both", className }: Props) {
  const [showNotification, setShowNotification] = useState(false);
  const [isCopied, setIsCopied] = useState(false);
  const timeoutRef = useRef<any>(null);

  const handleAction = async (e: React.MouseEvent) => {
    e.stopPropagation(); // Avoid triggering any parent click events
    try {
      await navigator.clipboard.writeText(promptText);
      setIsCopied(true);
      setShowNotification(true);
      if (timeoutRef.current) {
        clearTimeout(timeoutRef.current);
      }
      timeoutRef.current = setTimeout(() => {
        setIsCopied(false);
        setShowNotification(false);
      }, 5000);
    } catch (e) {
      console.error("Failed to copy prompt to clipboard", e);
    }
  };

  const handleMouseEnter = () => {
    setShowNotification(true);
  };

  const handleMouseLeave = () => {
    if (!isCopied) {
      setShowNotification(false);
    }
  };

  return (
    <div
      className={`relative inline-block ${className || ""}`}
      onMouseEnter={handleMouseEnter}
      onMouseLeave={handleMouseLeave}
    >
      <div className="flex items-center gap-1.5">
        {(variant === "icon" || variant === "both") && (
          <button
            onClick={handleAction}
            className="flex h-7 w-7 items-center justify-center border border-[#3e4270] bg-[#10152f] hover:bg-[#202754] transition-colors duration-100 shadow-[0_0_10px_rgba(97,247,255,0.1)] rounded"
            title={`Copy AI explanation prompt about ${topic}`}
            type="button"
          >
            <RobotIcon />
          </button>
        )}

        {(variant === "text" || variant === "both") && (
          <span
            onClick={handleAction}
            className="cursor-pointer text-[1.1rem] font-bold text-neon-cyan underline decoration-[#61f7ff]/40 underline-offset-2 hover:text-white hover:decoration-white transition-all duration-100"
          >
            Copy Explanation Prompt
          </span>
        )}
      </div>

      {showNotification && (
        <div className="absolute left-0 top-full z-50 mt-2 w-72 border border-[#44d6ff]/50 bg-[#090b16] p-3 text-[1.05rem] leading-snug text-[#cfe7ff] shadow-[0_0_15px_rgba(68,214,255,0.3)] animate-fade-in text-left">
          <p className="font-semibold text-neon-cyan mb-1">
            {isCopied ? "Prompt Copied! 🤖" : "AI Explanation Prompt"}
          </p>
          <p>
            {isCopied ? (
              <>
                You just copied a prompt about <strong className="text-white">{topic}</strong>. Go to your AI provider and paste the prompt; it will be explained.
              </>
            ) : (
              <>
                Click the robot icon to copy an AI explanation prompt about <strong className="text-white">{topic}</strong> to your clipboard.
              </>
            )}
          </p>
        </div>
      )}
    </div>
  );
}
