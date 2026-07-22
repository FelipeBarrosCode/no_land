#import <AVFoundation/AVFoundation.h>

#include "noland_audio_renderer.h"
#include "AudioMasterClock.h"
#include "AudioPlayoutController.h"

#include <float.h>
#include <math.h>
#include <unistd.h>

@interface NolandAudioPlaybackContext : NSObject
- (instancetype)initWithOpusConfig:(const POPUS_MULTISTREAM_CONFIGURATION)opusConfig
                    targetBufferMs:(uint32_t)targetBufferMs
                   maximumBufferMs:(uint32_t)maximumBufferMs;
- (BOOL)startPlayback;
- (void)stopPlayback;
- (void)cleanupPlayback;
- (void)decodeAndPlaySample:(char*)sampleData
                       length:(int)sampleLength
               sequenceNumber:(uint16_t)sequenceNumber
                 rtpTimestamp:(uint32_t)rtpTimestamp
         extendedRtpTimestamp:(uint64_t)extendedRtpTimestamp
                arrivalTimeUs:(uint64_t)arrivalTimeUs;
- (void)fillStats:(nl_audio_renderer_stats_t*)stats;
@end

static float nl_peak_for_planar_samples(float* const* channelData, AVAudioFrameCount frameCount, AVAudioChannelCount channelCount) {
  if (channelData == NULL || frameCount == 0 || channelCount == 0) {
    return 0.0f;
  }

  float peak = 0.0f;
  for (AVAudioChannelCount channel = 0; channel < channelCount; ++channel) {
    float* samples = channelData[channel];
    if (samples == NULL) {
      continue;
    }
    for (AVAudioFrameCount frame = 0; frame < frameCount; ++frame) {
      float magnitude = fabsf(samples[frame]);
      if (magnitude > peak) {
        peak = magnitude;
      }
    }
  }

  return peak;
}

static float nl_peak_for_interleaved_samples(const float* samples, uint32_t frameCount, int channelCount) {
  if (samples == NULL || frameCount == 0 || channelCount <= 0) {
    return 0.0f;
  }

  float peak = 0.0f;
  uint32_t sampleCount = frameCount * (uint32_t)channelCount;
  for (uint32_t index = 0; index < sampleCount; ++index) {
    float magnitude = fabsf(samples[index]);
    if (magnitude > peak) {
      peak = magnitude;
    }
  }

  return peak;
}

static float nl_peak_for_buffer(AVAudioPCMBuffer* buffer, AVAudioFrameCount frameCount, AVAudioChannelCount channelCount) {
  if (buffer == nil) {
    return 0.0f;
  }
  return nl_peak_for_planar_samples(buffer.floatChannelData, frameCount, channelCount);
}

@implementation NolandAudioPlaybackContext {
  AVAudioEngine* _engine;
  AVAudioPlayerNode* _player;
  AVAudioFormat* _sourceFormat;
  dispatch_queue_t _queue;
  nl_audio_playout_controller_t _playoutController;
  float* _stagingInterleaved;
  uint64_t _incomingPacketCount;
  uint64_t _decodeCallCount;
  uint64_t _scheduledBufferCount;
  uint64_t _droppedForMaximumDurationCount;
  uint64_t _totalScheduledFrames;
  uint32_t _scheduleChunkFrames;
  uint32_t _stagedFrames;
  int _sampleRate;
  int _channelCount;
  int _samplesPerFrame;
  BOOL _started;
  BOOL _playing;
  NSInteger _pendingBufferCount;
  uint32_t _targetBufferMs;
  uint32_t _maximumBufferMs;
}

- (instancetype)initWithOpusConfig:(const POPUS_MULTISTREAM_CONFIGURATION)opusConfig
                    targetBufferMs:(uint32_t)targetBufferMs
                   maximumBufferMs:(uint32_t)maximumBufferMs {
  self = [super init];
  if (self == nil || opusConfig == NULL) {
    return nil;
  }

  _sampleRate = opusConfig->sampleRate;
  _channelCount = opusConfig->channelCount;
  _samplesPerFrame = opusConfig->samplesPerFrame;
  _queue = dispatch_queue_create("io.noland.moonlight.audio", DISPATCH_QUEUE_SERIAL);
  _targetBufferMs = targetBufferMs > 0 ? targetBufferMs : 20U;
  _maximumBufferMs = maximumBufferMs > 0 ? maximumBufferMs : 80U;
  if (_maximumBufferMs < _targetBufferMs) {
    _maximumBufferMs = _targetBufferMs;
  }
  _sourceFormat = [[AVAudioFormat alloc] initStandardFormatWithSampleRate:(double)_sampleRate
                                                                  channels:(AVAudioChannelCount)_channelCount];
  if (_sourceFormat == nil) {
    return nil;
  }

  uint32_t chunkMs = _targetBufferMs / 2U;
  if (chunkMs < 5U) {
    chunkMs = 5U;
  }
  if (chunkMs > 10U) {
    chunkMs = 10U;
  }
  uint32_t chunkFrames = (uint32_t)(((uint64_t)_sampleRate * (uint64_t)chunkMs) / 1000ULL);
  if (chunkFrames < (uint32_t)_samplesPerFrame) {
    chunkFrames = (uint32_t)_samplesPerFrame;
  }
  _scheduleChunkFrames = chunkFrames;

  _stagingInterleaved = calloc((size_t)_scheduleChunkFrames * (size_t)_channelCount, sizeof(float));
  if (_stagingInterleaved == NULL) {
    return nil;
  }

  if (!nl_audio_playout_controller_init(&_playoutController,
                                        opusConfig,
                                        _targetBufferMs,
                                        _maximumBufferMs)) {
    free(_stagingInterleaved);
    _stagingInterleaved = NULL;
    return nil;
  }

  _engine = [[AVAudioEngine alloc] init];
  _player = [[AVAudioPlayerNode alloc] init];
  [_engine attachNode:_player];

  // Connect player → main mixer with nil format so AVAudioEngine
  // inserts any needed sample-rate converter automatically.
  [_engine connect:_player to:_engine.mainMixerNode format:nil];
  _player.volume = 1.0f;
  _engine.mainMixerNode.outputVolume = 1.0f;

  AVAudioFormat* mixerOutputFormat = [_engine.mainMixerNode outputFormatForBus:0];
  AVAudioFormat* outputNodeInputFormat = [_engine.outputNode inputFormatForBus:0];
  NSLog(@"[noland-audio] init sampleRate=%d channels=%d samplesPerFrame=%d chunkFrames=%u target=%u max=%u sourceFormat=%@ mixerOutputFormat=%@ outputFormat=%@ playerVolume=%.3f mixerVolume=%.3f",
        _sampleRate,
        _channelCount,
        _samplesPerFrame,
        (unsigned int)_scheduleChunkFrames,
        (unsigned int)_targetBufferMs,
        (unsigned int)_maximumBufferMs,
        _sourceFormat,
        mixerOutputFormat,
        outputNodeInputFormat,
        _player.volume,
        _engine.mainMixerNode.outputVolume);
  return self;
}

- (NSInteger)pendingBufferCount {
  @synchronized(self) {
    return _pendingBufferCount;
  }
}

- (void)incrementPendingBufferCount {
  @synchronized(self) {
    _pendingBufferCount += 1;
  }
}

- (void)decrementPendingBufferCount {
  @synchronized(self) {
    if (_pendingBufferCount > 0) {
      _pendingBufferCount -= 1;
    }
  }
}

- (BOOL)startPlaybackOnQueue {
  if (_started) {
    return YES;
  }

  NSError* error = nil;
  [_engine prepare];
  if (![_engine isRunning] && ![_engine startAndReturnError:&error]) {
    NSLog(@"[noland-audio] failed to start audio engine: %@", error);
    return NO;
  }

  _started = YES;
  NSLog(@"[noland-audio] output activated engineRunning=%d playerPlaying=%d",
        [_engine isRunning] ? 1 : 0,
        [_player isPlaying] ? 1 : 0);
  return YES;
}

- (BOOL)startPlayback {
  __block BOOL started = YES;
  dispatch_sync(_queue, ^{
    started = [self startPlaybackOnQueue];
  });
  return started;
}

- (void)ensurePlayerPlayingOnQueue {
  if (_player != nil && !_playing) {
    [_player play];
    _playing = YES;
  }
}

- (void)stopPlayback {
  dispatch_sync(_queue, ^{
    if (_player != nil && [_player isPlaying]) {
      [_player stop];
    }
    if (_engine != nil && [_engine isRunning]) {
      [_engine pause];
    }
    _started = NO;
    _playing = NO;
    _stagedFrames = 0;
    _totalScheduledFrames = 0;
    nl_audio_playout_controller_reset(&_playoutController);
    @synchronized(self) {
      _pendingBufferCount = 0;
    }
    AudioClockUpdatePendingSamples(0);
  });
}

- (void)cleanupPlayback {
  dispatch_sync(_queue, ^{
    if (_player != nil && [_player isPlaying]) {
      [_player stop];
    }
    if (_engine != nil) {
      [_engine stop];
    }
    if (_stagingInterleaved != NULL) {
      free(_stagingInterleaved);
      _stagingInterleaved = NULL;
    }
    nl_audio_playout_controller_cleanup(&_playoutController);
    _stagedFrames = 0;
    _started = NO;
    _playing = NO;
    _totalScheduledFrames = 0;
    @synchronized(self) {
      _pendingBufferCount = 0;
    }
    AudioClockUpdatePendingSamples(0);
  });
}

- (uint32_t)estimatedPendingFrames {
  if (_sampleRate <= 0) {
    return 0;
  }

  uint64_t renderedFrames = 0;
  AVAudioTime* nodeTime = [_player lastRenderTime];
  if (nodeTime != nil) {
    AVAudioTime* playerTime = [_player playerTimeForNodeTime:nodeTime];
    if (playerTime != nil && playerTime.sampleTime >= 0) {
      renderedFrames = (uint64_t)playerTime.sampleTime;
    }
  }

  uint64_t pendingFrames = _totalScheduledFrames +
                           (uint64_t)nl_audio_playout_controller_available_frames(&_playoutController) +
                           (uint64_t)_stagedFrames;
  if (pendingFrames > renderedFrames) {
    pendingFrames -= renderedFrames;
  }
  else {
    pendingFrames = 0;
  }

  if (pendingFrames > UINT32_MAX) {
    pendingFrames = UINT32_MAX;
  }
  return (uint32_t)pendingFrames;
}

- (void)updateAudioClockPendingFrames {
  AudioClockUpdatePendingSamples([self estimatedPendingFrames]);
}

- (void)scheduleInterleavedFrames:(uint32_t)frameCount {
  if (_stagingInterleaved == NULL || _sourceFormat == nil || _player == nil || frameCount == 0) {
    return;
  }

  AVAudioPCMBuffer* sourceBuffer = [[AVAudioPCMBuffer alloc] initWithPCMFormat:_sourceFormat
                                                                  frameCapacity:(AVAudioFrameCount)frameCount];
  if (sourceBuffer == nil || sourceBuffer.floatChannelData == NULL) {
    return;
  }

  for (int channel = 0; channel < _channelCount; ++channel) {
    float* dst = sourceBuffer.floatChannelData[channel];
    if (dst == NULL) {
      return;
    }
    for (uint32_t frame = 0; frame < frameCount; ++frame) {
      dst[frame] = _stagingInterleaved[(frame * (uint32_t)_channelCount) + (uint32_t)channel];
    }
  }
  sourceBuffer.frameLength = (AVAudioFrameCount)frameCount;

  [self incrementPendingBufferCount];
  _scheduledBufferCount += 1;
  _totalScheduledFrames += (uint64_t)frameCount;
  if (_scheduledBufferCount <= 10 || (_scheduledBufferCount % 200) == 0) {
    float sourcePeak = nl_peak_for_buffer(sourceBuffer, sourceBuffer.frameLength, sourceBuffer.format.channelCount);
    NSLog(@"[noland-audio] scheduled buffer=%llu srcFrameLength=%u pendingLocal=%ld sourcePeak=%.6f playerVolume=%.3f mixerVolume=%.3f",
          _scheduledBufferCount,
          (unsigned int)sourceBuffer.frameLength,
          (long)[self pendingBufferCount],
          sourcePeak,
          _player.volume,
          _engine.mainMixerNode.outputVolume);
  }

  __block NolandAudioPlaybackContext* context = self;
  [_player scheduleBuffer:sourceBuffer
        completionHandler:^{
          [context decrementPendingBufferCount];
          [context updateAudioClockPendingFrames];
        }];
  AudioClockRecordSubmittedSamples(frameCount);
  [self updateAudioClockPendingFrames];
}

- (void)pumpPlaybackFromControllerOnQueue {
  uint32_t availableFrames = nl_audio_playout_controller_available_frames(&_playoutController);
  uint32_t queuedPacketFrames = nl_audio_playout_controller_queued_packet_frames(&_playoutController);
  uint32_t startupPrefillFrames = nl_audio_playout_controller_startup_prefill_frames(&_playoutController);
  uint32_t targetFrames = nl_audio_playout_controller_current_target_frames(&_playoutController);
  uint32_t pendingFrames = [self estimatedPendingFrames];
  BOOL allowConcealment = _playing ? YES : NO;

  if (!_started) {
    return;
  }

  availableFrames = nl_audio_playout_controller_fill_frames(&_playoutController,
                                                            !_playing ? startupPrefillFrames : targetFrames,
                                                            allowConcealment);
  queuedPacketFrames = nl_audio_playout_controller_queued_packet_frames(&_playoutController);

  if (!_playing) {
    if (availableFrames + queuedPacketFrames < startupPrefillFrames) {
      [self updateAudioClockPendingFrames];
      return;
    }
    [self ensurePlayerPlayingOnQueue];
    availableFrames = nl_audio_playout_controller_fill_frames(&_playoutController,
                                                              targetFrames,
                                                              YES);
    queuedPacketFrames = nl_audio_playout_controller_queued_packet_frames(&_playoutController);
  }

  if (_playing && availableFrames == 0 && pendingFrames == 0 && queuedPacketFrames == 0) {
    nl_audio_playout_controller_note_starvation(&_playoutController);
    availableFrames = nl_audio_playout_controller_fill_frames(&_playoutController,
                                                              _samplesPerFrame,
                                                              YES);
  }

  while (availableFrames > 0 && (pendingFrames < targetFrames || pendingFrames == 0)) {
    uint32_t framesToSchedule = availableFrames;
    if (framesToSchedule > _scheduleChunkFrames) {
      framesToSchedule = _scheduleChunkFrames;
    }
    if (framesToSchedule < (uint32_t)_samplesPerFrame && pendingFrames > 0) {
      break;
    }

    if (nl_audio_playout_controller_pop_frames(&_playoutController,
                                               _stagingInterleaved,
                                               framesToSchedule) != framesToSchedule) {
      break;
    }

    [self scheduleInterleavedFrames:framesToSchedule];
    pendingFrames += framesToSchedule;
    availableFrames = nl_audio_playout_controller_available_frames(&_playoutController);
    if (availableFrames < _scheduleChunkFrames) {
      availableFrames = nl_audio_playout_controller_fill_frames(&_playoutController,
                                                                _scheduleChunkFrames,
                                                                _playing ? YES : NO);
    }
    queuedPacketFrames = nl_audio_playout_controller_queued_packet_frames(&_playoutController);
  }

  nl_audio_playout_controller_note_buffer_level(&_playoutController,
                                                pendingFrames + availableFrames + queuedPacketFrames);
  [self updateAudioClockPendingFrames];
}

- (void)decodeAndPlaySample:(char*)sampleData
                       length:(int)sampleLength
               sequenceNumber:(uint16_t)sequenceNumber
                 rtpTimestamp:(uint32_t)rtpTimestamp
         extendedRtpTimestamp:(uint64_t)extendedRtpTimestamp
                arrivalTimeUs:(uint64_t)arrivalTimeUs {
  dispatch_sync(_queue, ^{
    if (_sourceFormat == nil || _player == nil || _stagingInterleaved == NULL) {
      NSLog(@"[noland-audio] dropping packet before enqueue sourceFormat=%@ player=%@ staging=%p",
            _sourceFormat,
            _player,
            _stagingInterleaved);
      return;
    }
    if (![self startPlaybackOnQueue]) {
      return;
    }

    _incomingPacketCount += 1;
    if (_incomingPacketCount <= 10 || (_incomingPacketCount % 200) == 0) {
      NSLog(@"[noland-audio] enqueue packet=%llu seq=%u sampleBytes=%d pendingLocal=%ld pendingDeviceFrames=%u pcmFrames=%u queuedPacketFrames=%u",
            _incomingPacketCount,
            (unsigned int)sequenceNumber,
            sampleLength,
            (long)[self pendingBufferCount],
            (unsigned int)[self estimatedPendingFrames],
            (unsigned int)nl_audio_playout_controller_available_frames(&_playoutController),
            (unsigned int)nl_audio_playout_controller_queued_packet_frames(&_playoutController));
      }

    int queuedFrames = nl_audio_playout_controller_submit_packet(&_playoutController,
                                                                 sampleData,
                                                                 sampleLength,
                                                                 sequenceNumber,
                                                                 rtpTimestamp,
                                                                 extendedRtpTimestamp,
                                                                 arrivalTimeUs);
    if (queuedFrames <= 0 && !(sampleData == NULL || sampleLength == 0)) {
      _droppedForMaximumDurationCount += 1;
      if (_droppedForMaximumDurationCount <= 10 || (_droppedForMaximumDurationCount % 200) == 0) {
        NSLog(@"[noland-audio] dropping compressed packet seq=%u sampleBytes=%d drops=%llu",
              (unsigned int)sequenceNumber,
              sampleLength,
              _droppedForMaximumDurationCount);
      }
      return;
    }

    [self pumpPlaybackFromControllerOnQueue];
  });
}

- (void)fillStats:(nl_audio_renderer_stats_t*)stats {
  if (stats == NULL) {
    return;
  }

  dispatch_sync(_queue, ^{
    stats->buffered_frames = nl_audio_playout_controller_available_frames(&_playoutController);
    stats->startup_prefill_frames = nl_audio_playout_controller_startup_prefill_frames(&_playoutController);
    stats->target_frames = nl_audio_playout_controller_target_frames(&_playoutController);
    stats->current_target_frames = nl_audio_playout_controller_current_target_frames(&_playoutController);
    stats->maximum_frames = nl_audio_playout_controller_maximum_frames(&_playoutController);
    stats->concealment_count = nl_audio_playout_controller_concealment_count(&_playoutController);
    stats->dropped_frames_count = nl_audio_playout_controller_dropped_frames_count(&_playoutController);
    stats->starvation_event_count = nl_audio_playout_controller_starvation_event_count(&_playoutController);
    stats->target_increase_count = nl_audio_playout_controller_target_increase_count(&_playoutController);
    stats->target_decrease_count = nl_audio_playout_controller_target_decrease_count(&_playoutController);
  });
}

@end

int nl_audio_renderer_init(nl_audio_renderer_t* renderer,
                           int audio_configuration,
                           const POPUS_MULTISTREAM_CONFIGURATION opus_config,
                           int ar_flags) {
  (void)audio_configuration;
  (void)ar_flags;
  if (renderer == NULL || opus_config == NULL) {
    return -1;
  }

  nl_audio_renderer_cleanup(renderer);
  NolandAudioPlaybackContext* context = [[NolandAudioPlaybackContext alloc]
      initWithOpusConfig:opus_config
          targetBufferMs:renderer->target_buffer_ms
         maximumBufferMs:renderer->maximum_buffer_ms];
  if (context == nil) {
    NSLog(@"[noland-audio] failed to initialize audio playback context");
    return -1;
  }

  renderer->platform_context = (__bridge_retained void*)context;
  return 0;
}

void nl_audio_renderer_start(nl_audio_renderer_t* renderer) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    return;
  }
  NolandAudioPlaybackContext* context = (__bridge NolandAudioPlaybackContext*)renderer->platform_context;
  (void)[context startPlayback];
}

void nl_audio_renderer_stop(nl_audio_renderer_t* renderer) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    return;
  }
  NolandAudioPlaybackContext* context = (__bridge NolandAudioPlaybackContext*)renderer->platform_context;
  [context stopPlayback];
}

void nl_audio_renderer_cleanup(nl_audio_renderer_t* renderer) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    return;
  }
  NolandAudioPlaybackContext* context = (__bridge_transfer NolandAudioPlaybackContext*)renderer->platform_context;
  renderer->platform_context = NULL;
  [context cleanupPlayback];
}

void nl_audio_renderer_decode_and_play_sample(nl_audio_renderer_t* renderer,
                                              char* sample_data,
                                              int sample_length,
                                              uint16_t sequence_number,
                                              uint32_t rtp_timestamp,
                                              uint64_t extended_rtp_timestamp,
                                              uint64_t arrival_time_us) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    return;
  }
  NolandAudioPlaybackContext* context = (__bridge NolandAudioPlaybackContext*)renderer->platform_context;
  [context decodeAndPlaySample:sample_data
                        length:sample_length
                sequenceNumber:sequence_number
                  rtpTimestamp:rtp_timestamp
          extendedRtpTimestamp:extended_rtp_timestamp
                 arrivalTimeUs:arrival_time_us];
}

void nl_audio_renderer_get_stats(nl_audio_renderer_t* renderer,
                                 nl_audio_renderer_stats_t* stats) {
  if (stats == NULL) {
    return;
  }
  memset(stats, 0, sizeof(*stats));
  if (renderer == NULL || renderer->platform_context == NULL) {
    return;
  }

  NolandAudioPlaybackContext* context = (__bridge NolandAudioPlaybackContext*)renderer->platform_context;
  [context fillStats:stats];
}
