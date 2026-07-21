#import <AVFoundation/AVFoundation.h>

#include "noland_audio_renderer.h"

#include <opus/opus_multistream.h>
#include <unistd.h>

@interface NolandAudioPlaybackContext : NSObject
- (instancetype)initWithOpusConfig:(const POPUS_MULTISTREAM_CONFIGURATION)opusConfig
                    targetBufferMs:(uint32_t)targetBufferMs
                   maximumBufferMs:(uint32_t)maximumBufferMs;
- (BOOL)startPlayback;
- (void)stopPlayback;
- (void)cleanupPlayback;
- (void)decodeAndPlaySample:(char*)sampleData length:(int)sampleLength;
@end

@implementation NolandAudioPlaybackContext {
  AVAudioEngine* _engine;
  AVAudioPlayerNode* _player;
  AVAudioFormat* _format;
  dispatch_queue_t _queue;
  OpusMSDecoder* _decoder;
  float* _decodeScratch;
  float* _stagingInterleaved;
  uint64_t _decodeCallCount;
  uint64_t _scheduledBufferCount;
  uint64_t _droppedForMaximumDurationCount;
  uint32_t _scheduleChunkFrames;
  uint32_t _stagedFrames;
  int _sampleRate;
  int _channelCount;
  int _samplesPerFrame;
  BOOL _started;
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
  _format = [[AVAudioFormat alloc] initStandardFormatWithSampleRate:(double)_sampleRate
                                                            channels:(AVAudioChannelCount)_channelCount];
  if (_format == nil) {
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

  _decodeScratch = calloc((size_t)_samplesPerFrame * (size_t)_channelCount, sizeof(float));
  if (_decodeScratch == NULL) {
    return nil;
  }
  _stagingInterleaved = calloc((size_t)_scheduleChunkFrames * (size_t)_channelCount, sizeof(float));
  if (_stagingInterleaved == NULL) {
    free(_decodeScratch);
    _decodeScratch = NULL;
    return nil;
  }

  int error = 0;
  _decoder = opus_multistream_decoder_create(opusConfig->sampleRate,
                                             opusConfig->channelCount,
                                             opusConfig->streams,
                                             opusConfig->coupledStreams,
                                             opusConfig->mapping,
                                             &error);
  if (_decoder == NULL || error != OPUS_OK) {
    return nil;
  }

  _engine = [[AVAudioEngine alloc] init];
  _player = [[AVAudioPlayerNode alloc] init];
  [_engine attachNode:_player];
  [_engine connect:_player to:_engine.mainMixerNode format:_format];

  NSLog(@"[noland-audio] init sampleRate=%d channels=%d samplesPerFrame=%d chunkFrames=%u target=%u max=%u streamFormat=%@ mixerOutputFormat=%@ outputFormat=%@",
        _sampleRate,
        _channelCount,
        _samplesPerFrame,
        (unsigned int)_scheduleChunkFrames,
        (unsigned int)_targetBufferMs,
        (unsigned int)_maximumBufferMs,
        _format,
        [_engine.mainMixerNode outputFormatForBus:0],
        [_engine.outputNode inputFormatForBus:0]);
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

  [_player play];
  _started = YES;
  NSLog(@"[noland-audio] playback started engineRunning=%d playerPlaying=%d",
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

- (void)stopPlayback {
  dispatch_sync(_queue, ^{
    if (_player != nil && [_player isPlaying]) {
      [_player stop];
    }
    if (_engine != nil && [_engine isRunning]) {
      [_engine pause];
    }
    _started = NO;
    @synchronized(self) {
      _pendingBufferCount = 0;
    }
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
    if (_decoder != NULL) {
      opus_multistream_decoder_destroy(_decoder);
      _decoder = NULL;
    }
    if (_decodeScratch != NULL) {
      free(_decodeScratch);
      _decodeScratch = NULL;
    }
    if (_stagingInterleaved != NULL) {
      free(_stagingInterleaved);
      _stagingInterleaved = NULL;
    }
    _stagedFrames = 0;
    _started = NO;
    @synchronized(self) {
      _pendingBufferCount = 0;
    }
  });
}

- (void)scheduleInterleavedFrames:(uint32_t)frameCount {
  if (_stagingInterleaved == NULL || _format == nil || _player == nil || frameCount == 0) {
    return;
  }

  AVAudioPCMBuffer* buffer = [[AVAudioPCMBuffer alloc] initWithPCMFormat:_format
                                                            frameCapacity:(AVAudioFrameCount)frameCount];
  if (buffer == nil || buffer.floatChannelData == NULL) {
    return;
  }

  for (int channel = 0; channel < _channelCount; ++channel) {
    float* dst = buffer.floatChannelData[channel];
    if (dst == NULL) {
      return;
    }
    for (uint32_t frame = 0; frame < frameCount; ++frame) {
      dst[frame] = _stagingInterleaved[(frame * (uint32_t)_channelCount) + (uint32_t)channel];
    }
  }

  buffer.frameLength = (AVAudioFrameCount)frameCount;

  [self incrementPendingBufferCount];
  _scheduledBufferCount += 1;
  if (_scheduledBufferCount <= 10 || (_scheduledBufferCount % 200) == 0) {
    NSLog(@"[noland-audio] scheduled buffer=%llu frameLength=%u pendingLocal=%ld",
          _scheduledBufferCount,
          (unsigned int)buffer.frameLength,
          (long)[self pendingBufferCount]);
  }

  __block NolandAudioPlaybackContext* context = self;
  [_player scheduleBuffer:buffer
        completionHandler:^{
          [context decrementPendingBufferCount];
        }];
}

- (void)decodeAndPlaySample:(char*)sampleData length:(int)sampleLength {
  if (sampleData == NULL || sampleLength <= 0 || _decoder == NULL) {
    return;
  }

  int pendingAudioDuration = LiGetPendingAudioDuration();
  if (pendingAudioDuration > (int)_maximumBufferMs) {
    _droppedForMaximumDurationCount += 1;
    if (_droppedForMaximumDurationCount <= 10 || (_droppedForMaximumDurationCount % 200) == 0) {
      NSLog(@"[noland-audio] dropping sample due to pending duration pending=%d max=%u drops=%llu",
            pendingAudioDuration,
            (unsigned int)_maximumBufferMs,
            _droppedForMaximumDurationCount);
    }
    return;
  }

  int frameDurationMs = _sampleRate > 0 ? (_samplesPerFrame * 1000) / _sampleRate : 5;
  if (frameDurationMs <= 0) {
    frameDurationMs = 5;
  }
  int maxPendingBuffers = (int)((_targetBufferMs + (uint32_t)frameDurationMs - 1U) / (uint32_t)frameDurationMs);
  if (maxPendingBuffers < 1) {
    maxPendingBuffers = 1;
  }

  for (int i = 0; i < 100; ++i) {
    if ([self pendingBufferCount] <= maxPendingBuffers) {
      break;
    }
    usleep(1000);
  }

  dispatch_sync(_queue, ^{
    if (_decoder == NULL || _format == nil || _player == nil) {
      return;
    }
    if (![self startPlaybackOnQueue]) {
      return;
    }

    if (_decodeScratch == NULL || _stagingInterleaved == NULL) {
      return;
    }

    int decodedSamples = opus_multistream_decode_float(_decoder,
                                                       (const unsigned char*)sampleData,
                                                       sampleLength,
                                                       _decodeScratch,
                                                       _samplesPerFrame,
                                                       0);
    if (decodedSamples <= 0) {
      return;
    }

    _decodeCallCount += 1;
    if (_decodeCallCount <= 10 || (_decodeCallCount % 200) == 0) {
      NSLog(@"[noland-audio] decoded packet=%llu sampleBytes=%d decodedSamples=%d pendingMoonlight=%d pendingLocal=%ld stagedFrames=%u",
            _decodeCallCount,
            sampleLength,
            decodedSamples,
            pendingAudioDuration,
            (long)[self pendingBufferCount],
            (unsigned int)_stagedFrames);
    }

    if (_stagedFrames > 0 && (_stagedFrames + (uint32_t)decodedSamples) > _scheduleChunkFrames) {
      [self scheduleInterleavedFrames:_stagedFrames];
      _stagedFrames = 0;
    }

    if ((uint32_t)decodedSamples > _scheduleChunkFrames) {
      memcpy(_stagingInterleaved,
             _decodeScratch,
             (size_t)decodedSamples * (size_t)_channelCount * sizeof(float));
      [self scheduleInterleavedFrames:(uint32_t)decodedSamples];
      _stagedFrames = 0;
      return;
    }

    memcpy(_stagingInterleaved + ((size_t)_stagedFrames * (size_t)_channelCount),
           _decodeScratch,
           (size_t)decodedSamples * (size_t)_channelCount * sizeof(float));
    _stagedFrames += (uint32_t)decodedSamples;

    if (_stagedFrames >= _scheduleChunkFrames || ([self pendingBufferCount] == 0 && _stagedFrames >= (uint32_t)_samplesPerFrame)) {
      [self scheduleInterleavedFrames:_stagedFrames];
      _stagedFrames = 0;
    }
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
                                              int sample_length) {
  if (renderer == NULL || renderer->platform_context == NULL) {
    return;
  }
  NolandAudioPlaybackContext* context = (__bridge NolandAudioPlaybackContext*)renderer->platform_context;
  [context decodeAndPlaySample:sample_data length:sample_length];
}
