import {
  VAST_API_KEY_URL,
  VAST_BILLING_URL,
  VAST_HOME_URL,
} from "../../lib/constants";

export interface TutorialStep {
  eyebrow: string;
  title: string;
  description: string;
  linkLabel?: string;
  linkUrl?: string;
  links?: { label: string; url: string }[];
}

export const tutorialSteps: TutorialStep[] = [
  {
    eyebrow: "tutorial.step1.eyebrow",
    title: "tutorial.step1.title",
    description:
      "tutorial.step1.description",
  },
  {
    eyebrow: "tutorial.step2.eyebrow",
    title: "tutorial.step2.title",
    description:
      "tutorial.step2.description",
    linkLabel: "tutorial.link.vast",
    linkUrl: VAST_HOME_URL,
  },
  {
    eyebrow: "tutorial.step3.eyebrow",
    title: "tutorial.step3.title",
    description:
      "tutorial.step3.description",
    linkLabel: "tutorial.link.vast",
    linkUrl: VAST_HOME_URL,
  },
  {
    eyebrow: "tutorial.step4.eyebrow",
    title: "tutorial.step4.title",
    description:
      "tutorial.step4.description",
    linkLabel: "tutorial.link.billing",
    linkUrl: VAST_BILLING_URL,
  },
  {
    eyebrow: "tutorial.step5.eyebrow",
    title: "tutorial.step5.title",
    description:
      "tutorial.step5.description",
    linkLabel: "tutorial.link.keys",
    linkUrl: VAST_API_KEY_URL,
  },
  {
    eyebrow: "tutorial.step6.eyebrow",
    title: "tutorial.step6.title",
    description:
      "tutorial.step6.description",
  },
  {
    eyebrow: "tutorial.step7.eyebrow",
    title: "tutorial.step7.title",
    description:
      "tutorial.step7.description",
  },
  {
    eyebrow: "tutorial.step8.eyebrow",
    title: "tutorial.step8.title",
    description:
      "tutorial.step8.description",
  },
  {
    eyebrow: "tutorial.step9.eyebrow",
    title: "tutorial.step9.title",
    description:
      "tutorial.step9.description",
  },
];
