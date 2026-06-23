import { Button } from "../../components/ui/Button";
import { Card } from "../../components/ui/Card";
import type { TutorialStep } from "./tutorialSteps";

interface Props {
  open: boolean;
  stepIndex: number;
  steps: TutorialStep[];
  closable?: boolean;
  onBack: () => void;
  onNext: () => void;
  onClose?: () => void;
}

export function TutorialModal({
  open,
  stepIndex,
  steps,
  closable = false,
  onBack,
  onNext,
  onClose
}: Props) {
  if (!open) {
    return null;
  }

  const step = steps[stepIndex];
  const isLastStep = stepIndex === steps.length - 1;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-[#02040bdd] p-4">
      <Card className="pixel-frame w-full max-w-lg animate-fade-in p-6 md:p-8">
        <div className="flex items-start justify-between gap-4">
          <div>
            <p className="font-display text-[10px] uppercase tracking-[0.2em] text-neon-cyan">
              {step.eyebrow}
            </p>
            <h2 className="pixel-heading mt-2 font-display text-lg text-white md:text-xl">
              {step.title}
            </h2>
          </div>

          {closable && onClose ? (
            <Button variant="ghost" onClick={onClose}>
              Close
            </Button>
          ) : null}
        </div>

        <p className="mt-4 text-[1.35rem] leading-[1.15] text-[#c5d8ec]">{step.description}</p>

        {step.links ? (
          <div className="mt-4 flex flex-wrap gap-2">
            {step.links.map((link) => (
              <a
                key={link.label}
                className="inline-flex items-center justify-center border border-[#61f7ff] bg-[#1b2f4d] px-4 py-2 font-display text-[11px] uppercase tracking-[0.12em] text-[#7cf8ff] shadow-[0_0_0_2px_#090a17,inset_0_0_0_2px_#2f5f86,0_0_20px_rgba(68,214,255,0.25)] transition duration-100 hover:bg-[#22466e] hover:text-white"
                href={link.url}
                target="_blank"
                rel="noreferrer"
              >
                {link.label}
              </a>
            ))}
          </div>
        ) : step.linkLabel && step.linkUrl ? (
          <a
            className="mt-4 inline-flex items-center justify-center border border-[#61f7ff] bg-[#1b2f4d] px-4 py-2 font-display text-[11px] uppercase tracking-[0.12em] text-[#7cf8ff] shadow-[0_0_0_2px_#090a17,inset_0_0_0_2px_#2f5f86,0_0_20px_rgba(68,214,255,0.25)] transition duration-100 hover:bg-[#22466e] hover:text-white"
            href={step.linkUrl}
            target="_blank"
            rel="noreferrer"
          >
            {step.linkLabel}
          </a>
        ) : null}

        <div className="mt-6 flex items-center justify-between gap-3">
          <p className="font-display text-[10px] uppercase tracking-[0.12em] text-[#8fb4d4]">
            {stepIndex + 1} / {steps.length}
          </p>
          <div className="flex items-center gap-2">
            <Button variant="ghost" onClick={onBack} disabled={stepIndex === 0}>
              Back
            </Button>
            <Button onClick={onNext}>{isLastStep ? "Start Setup" : "Next"}</Button>
          </div>
        </div>
      </Card>
    </div>
  );
}
