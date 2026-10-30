// Native owns persistence and credentials; these are read-only frontend mirrors.
export interface FeedbackControl {
  kind: "consent" | "delete";
  requestId: string;
  expectedEpoch: number;
  enabled: boolean;
  optionalContextEnabled: boolean;
}

export interface FeedbackProgramState {
  schemaVersion: 1;
  enabled: boolean;
  enrollmentStatus: string;
  installationId: string | null;
  userId: string | null;
  credentialRef: string | null;
  deletionCodeRef: string | null;
  consentEpoch: number;
  privacyVersion: string | null;
  termsVersion: string | null;
  consentedAt: string | null;
  optionalContextEnabled: boolean;
  pendingControlAction: FeedbackControl | null;
  enrollmentRequestId: string | null;
  enrollmentProofRef: string | null;
  enrollmentStartedAt: string | null;
  receiptId: string | null;
  receiptStatus: string | null;
  receiptTokenRef: string | null;
  collectionPaused: boolean;
}

export interface FeedbackProgramStatus extends FeedbackProgramState {
  configured: boolean;
  supported: boolean;
  statusMessage: string | null;
  pendingSessions: number;
}

export interface FeedbackProgramDocument {
  schemaVersion: 1;
  available: boolean;
  collectionPaused: boolean;
  privacyVersion: string;
  termsVersion: string;
  privacyUrl: string;
  termsUrl: string;
  deletionUrl: string;
  contact: string;
  disclosure: string;
  metrics: unknown[];
}

export interface FeedbackOptionalProfile {
  country?: string;
  region?: string;
  connectionCategory?: string;
  bandwidthTier?: string;
}

export interface FeedbackEnrollmentRequest {
  privacyVersion: string;
  termsVersion: string;
  participationAccepted: boolean;
  optionalContextEnabled: boolean;
  optionalProfile: FeedbackOptionalProfile;
}

export interface FeedbackMachineSummary {
  providerMachineId: string;
  reliability: {
    source: "no_land";
    ratio: number | null;
    eligibleSessions: number;
    successfulSessions: number;
    failedSessions: number;
    basis: string;
    scope: string;
    firstObservedAt: string | null;
    lastObservedAt: string | null;
  };
  jitter: {
    meanMs: number;
    minMs: number;
    maxMs: number;
    sampleCount: number;
    contributingSessions: number;
    highSampleRatio: number;
    severeSampleRatio: number;
    metricVersion: string;
    scope: string;
  } | null;
  reviews: {
    commentCount: number;
    ratingCount: number;
    averageScore: number | null;
  };
  asOf: string | null;
}

export interface FeedbackReview {
  reviewId: string;
  score: number | null;
  comment: string | null;
  publishedDate: string;
  hardwareSnapshot: {
    gpuName: string | null;
    rentedGpuCount: number | null;
    country: string | null;
    region: string | null;
  } | null;
}

export interface FeedbackReviewsPage {
  reviews: FeedbackReview[];
  nextCursor: string | null;
}

export interface FeedbackRead<T> {
  value: T;
  expiresAt: number;
}
