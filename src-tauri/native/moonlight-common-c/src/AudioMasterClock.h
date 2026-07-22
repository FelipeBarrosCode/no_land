#pragma once

#include <stdint.h>
#include <stdbool.h>

#include "Limelight.h"

void AudioClockReset(uint32_t sampleRate);
uint64_t AudioClockExtendRtpTimestamp32(uint32_t current);
uint64_t AudioClockPredictNextExtendedTimestamp(uint32_t increment);
uint64_t AudioClockMediaTimeUsForExtendedTimestamp(uint64_t extendedTimestamp);
void AudioClockObservePacket(uint16_t sequenceNumber, uint32_t rtpTimestamp, uint64_t extendedTimestamp, uint64_t arrivalTimeUs);
void AudioClockRecordSubmittedSamples(uint32_t submittedSamples);
void AudioClockUpdatePendingSamples(uint32_t pendingSamples);
bool AudioClockIsValid(void);
uint64_t AudioClockGetMediaTimeUs(void);
const AUDIO_MASTER_CLOCK_STATS* AudioClockGetStats(void);
