#include "AudioMasterClock.h"

#include <string.h>

static AUDIO_MASTER_CLOCK_STATS g_audioClock;
static uint32_t g_lastRawRtpTimestamp;
static bool g_hasLastRawRtpTimestamp;
static bool g_hasAnchor;

static uint32_t lowPassPendingSamples(uint32_t previousPending, uint32_t measuredPending) {
    uint64_t weighted = ((uint64_t)previousPending * 9ULL) + (uint64_t)measuredPending;
    return (uint32_t)(weighted / 10ULL);
}

void AudioClockReset(uint32_t sampleRate) {
    memset(&g_audioClock, 0, sizeof(g_audioClock));
    g_audioClock.sampleRate = sampleRate;
    g_lastRawRtpTimestamp = 0;
    g_hasLastRawRtpTimestamp = false;
    g_hasAnchor = false;
}

uint64_t AudioClockExtendRtpTimestamp32(uint32_t current) {
    uint64_t extended;

    if (!g_hasLastRawRtpTimestamp) {
        g_lastRawRtpTimestamp = current;
        g_hasLastRawRtpTimestamp = true;
        return (uint64_t)current;
    }

    extended = (g_audioClock.lastExtendedRtpTimestamp & 0xFFFFFFFF00000000ULL) | (uint64_t)current;
    if (current < g_lastRawRtpTimestamp && (g_lastRawRtpTimestamp - current) > 0x80000000U) {
        extended += 0x100000000ULL;
    }
    else if (current > g_lastRawRtpTimestamp && (current - g_lastRawRtpTimestamp) > 0x80000000U) {
        extended -= 0x100000000ULL;
    }

    g_lastRawRtpTimestamp = current;
    return extended;
}

uint64_t AudioClockPredictNextExtendedTimestamp(uint32_t increment) {
    if (!g_audioClock.valid) {
        return 0;
    }

    return g_audioClock.lastExtendedRtpTimestamp + (uint64_t)increment;
}

uint64_t AudioClockMediaTimeUsForExtendedTimestamp(uint64_t extendedTimestamp) {
    if (!g_audioClock.valid || g_audioClock.sampleRate == 0) {
        return 0;
    }

    return g_audioClock.anchorMediaTimeUs +
           (((extendedTimestamp - g_audioClock.anchorRtpTimestamp) * 1000000ULL) / (uint64_t)g_audioClock.sampleRate);
}

void AudioClockObservePacket(uint16_t sequenceNumber, uint32_t rtpTimestamp, uint64_t extendedTimestamp, uint64_t arrivalTimeUs) {
    if (!g_hasAnchor) {
        g_audioClock.anchorRtpTimestamp = extendedTimestamp;
        g_audioClock.anchorMediaTimeUs = 0;
        g_hasAnchor = true;
    }

    g_audioClock.lastSequenceNumber = sequenceNumber;
    g_audioClock.lastRtpTimestamp = rtpTimestamp;
    g_audioClock.lastExtendedRtpTimestamp = extendedTimestamp;
    g_audioClock.lastPacketArrivalTimeUs = arrivalTimeUs;
    g_audioClock.lastPacketMediaTimeUs = AudioClockMediaTimeUsForExtendedTimestamp(extendedTimestamp);
}

void AudioClockRecordSubmittedSamples(uint32_t submittedSamples) {
    g_audioClock.submittedSamples += (uint64_t)submittedSamples;
    if (g_hasAnchor && g_audioClock.sampleRate != 0 && g_audioClock.submittedSamples != 0) {
        g_audioClock.valid = 1;
    }
}

void AudioClockUpdatePendingSamples(uint32_t pendingSamples) {
    g_audioClock.filteredPendingSamples = lowPassPendingSamples(g_audioClock.filteredPendingSamples, pendingSamples);
    if (g_audioClock.submittedSamples > g_audioClock.filteredPendingSamples) {
        g_audioClock.playedSamples = g_audioClock.submittedSamples - (uint64_t)g_audioClock.filteredPendingSamples;
    }
    else {
        g_audioClock.playedSamples = 0;
    }
}

bool AudioClockIsValid(void) {
    return g_audioClock.valid;
}

uint64_t AudioClockGetMediaTimeUs(void) {
    if (!g_audioClock.valid || g_audioClock.sampleRate == 0) {
        return 0;
    }

    return g_audioClock.anchorMediaTimeUs +
           ((g_audioClock.playedSamples * 1000000ULL) / (uint64_t)g_audioClock.sampleRate);
}

const AUDIO_MASTER_CLOCK_STATS* AudioClockGetStats(void) {
    return &g_audioClock;
}
