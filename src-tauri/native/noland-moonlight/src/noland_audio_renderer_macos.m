#import <AVFoundation/AVFoundation.h>

#include "noland_audio_renderer.h"

#include <float.h>
#include <math.h>
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
  AVAudioFormat* _playbackFormat;
  AVAudioConverter* _converter;
  dispatch_queue_t _queue;
  OpusMSDecoder* _decoder;
  float* _decodeScratch;
  float* _stagingInterleaved;
  uint64_t _incomingPacketCount;
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

  AVAudioFormat* mixerOutputFormat = [_engine.mainMixerNode outputFormatForBus:0];
  AVAudioFormat* outputNodeInputFormat = [_engine.outputNode inputFormatForBus:0];
  double playbackSampleRate = mixerOutputFormat != nil && mixerOutputFormat.sampleRate > 0.0
      ? mixerOutputFormat.sampleRate
      : (outputNodeInputFormat != nil && outputNodeInputFormat.sampleRate > 0.0
            ? outputNodeInputFormat.sampleRate
            : (double)_sampleRate);
  _playbackFormat = [[AVAudioFormat alloc] initStandardFormatWithSampleRate:playbackSampleRate
                                                                   channels:(AVAudioChannelCount)_channelCount];
  if (_playbackFormat == nil) {
    return nil;
  }

  if (fabs(_playbackFormat.sampleRate - _sourceFormat.sampleRate) > 0.5 ||
      _playbackFormat.channelCount != _sourceFormat.channelCount) {
    _converter = [[AVAudioConverter alloc] initFromFormat:_sourceFormat toFormat:_playbackFormat];
    if (_converter == nil) {
      NSLog(@"[noland-audio] failed to create converter from %@ to %@", _sourceFormat, _playbackFormat);
      return nil;
    }
  }

  [_engine connect:_player to:_engine.mainMixerNode format:_playbackFormat];
  _player.volume = 1.0f;
  _engine.mainMixerNode.outputVolume = 1.0f;

  NSLog(@"[noland-audio] init sampleRate=%d channels=%d samplesPerFrame=%d chunkFrames=%u target=%u max=%u sourceFormat=%@ playbackFormat=%@ mixerOutputFormat=%@ outputFormat=%@ converter=%@ playerVolume=%.3f mixerVolume=%.3f",
        _sampleRate,
        _channelCount,
        _samplesPerFrame,
        (unsigned int)_scheduleChunkFrames,
        (unsigned int)_targetBufferMs,
        (unsigned int)_maximumBufferMs,
        _sourceFormat,
        _playbackFormat,
        [_engine.mainMixerNode outputFormatForBus:0],
        [_engine.outputNode inputFormatForBus:0],
        _converter,
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
  if (_stagingInterleaved == NULL || _sourceFormat == nil || _playbackFormat == nil || _player == nil || frameCount == 0) {
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

  AVAudioPCMBuffer* playbackBuffer = sourceBuffer;
  if (_converter != nil) {
    double ratio = _playbackFormat.sampleRate / _sourceFormat.sampleRate;
    AVAudioFrameCount convertedCapacity = (AVAudioFrameCount)MAX(1.0, ceil((double)frameCount * ratio) + 16.0);
    AVAudioPCMBuffer* convertedBuffer = [[AVAudioPCMBuffer alloc] initWithPCMFormat:_playbackFormat
                                                                       frameCapacity:convertedCapacity];
    if (convertedBuffer == nil) {
      return;
    }

    __block BOOL providedInput = NO;
    NSError* conversionError = nil;
    AVAudioConverterOutputStatus status = [_converter convertToBuffer:convertedBuffer
                                                                error:&conversionError
                                                   withInputFromBlock:^AVAudioBuffer* _Nullable(AVAudioPacketCount inNumPackets, AVAudioConverterInputStatus* outStatus) {
      (void)inNumPackets;
      if (providedInput) {
        *outStatus = AVAudioConverterInputStatus_EndOfStream;
        return nil;
      }
      providedInput = YES;
      *outStatus = AVAudioConverterInputStatus_HaveData;
      return sourceBuffer;
    }];

    if (status == AVAudioConverterOutputStatus_Error || conversionError != nil) {
      NSLog(@"[noland-audio] conversion failed status=%ld error=%@", (long)status, conversionError);
      return;
    }
    if (convertedBuffer.frameLength == 0) {
      NSLog(@"[noland-audio] conversion produced no frames status=%ld", (long)status);
      return;
    }
    if (status != AVAudioConverterOutputStatus_HaveData && status != AVAudioConverterOutputStatus_InputRanDry) {
      NSLog(@"[noland-audio] conversion returned unusual status=%ld frameLength=%u",
            (long)status,
            (unsigned int)convertedBuffer.frameLength);
    }
    playbackBuffer = convertedBuffer;
  }

  [self incrementPendingBufferCount];
  _scheduledBufferCount += 1;
  if (_scheduledBufferCount <= 10 || (_scheduledBufferCount % 200) == 0) {
    float sourcePeak = nl_peak_for_buffer(sourceBuffer, sourceBuffer.frameLength, sourceBuffer.format.channelCount);
    float playbackPeak = nl_peak_for_buffer(playbackBuffer, playbackBuffer.frameLength, playbackBuffer.format.channelCount);
    NSLog(@"[noland-audio] scheduled buffer=%llu srcFrameLength=%u playbackFrameLength=%u pendingLocal=%ld sourcePeak=%.6f playbackPeak=%.6f playerVolume=%.3f mixerVolume=%.3f",
          _scheduledBufferCount,
          (unsigned int)sourceBuffer.frameLength,
          (unsigned int)playbackBuffer.frameLength,
          (long)[self pendingBufferCount],
          sourcePeak,
          playbackPeak,
          _player.volume,
          _engine.mainMixerNode.outputVolume);
  }

  __block NolandAudioPlaybackContext* context = self;
  [_player scheduleBuffer:playbackBuffer
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
    if (_decoder == NULL || _sourceFormat == nil || _playbackFormat == nil || _player == nil) {
      NSLog(@"[noland-audio] dropping packet before decode decoder=%p sourceFormat=%@ playbackFormat=%@ player=%@",
            _decoder,
            _sourceFormat,
            _playbackFormat,
            _player);
      return;
    }
    if (![self startPlaybackOnQueue]) {
      return;
    }

    if (_decodeScratch == NULL || _stagingInterleaved == NULL) {
      NSLog(@"[noland-audio] decode buffers unavailable scratch=%p staging=%p",
            _decodeScratch,
            _stagingInterleaved);
      return;
    }

    _incomingPacketCount += 1;
    if (_incomingPacketCount <= 10 || (_incomingPacketCount % 200) == 0) {
      NSLog(@"[noland-audio] incoming packet=%llu sampleBytes=%d pendingMoonlight=%d pendingLocal=%ld stagedFrames=%u",
            _incomingPacketCount,
            sampleLength,
            pendingAudioDuration,
            (long)[self pendingBufferCount],
            (unsigned int)_stagedFrames);
    }

    int decodedSamples = opus_multistream_decode_float(_decoder,
                                                       (const unsigned char*)sampleData,
                                                       sampleLength,
                                                       _decodeScratch,
                                                       _samplesPerFrame,
                                                       0);
    if (decodedSamples <= 0) {
      NSLog(@"[noland-audio] opus decode failed sampleBytes=%d code=%d reason=%s",
            sampleLength,
            decodedSamples,
            opus_strerror(decodedSamples));
      return;
    }

    _decodeCallCount += 1;
    float decodedPeak = nl_peak_for_interleaved_samples(_decodeScratch,
                                                        (uint32_t)decodedSamples,
                                                        _channelCount);
    if (_decodeCallCount <= 10 || (_decodeCallCount % 200) == 0) {
      NSLog(@"[noland-audio] decoded packet=%llu sampleBytes=%d decodedSamples=%d pendingMoonlight=%d pendingLocal=%ld stagedFrames=%u decodedPeak=%.6f",
            _decodeCallCount,
            sampleLength,
            decodedSamples,
            pendingAudioDuration,
            (long)[self pendingBufferCount],
            (unsigned int)_stagedFrames,
            decodedPeak);
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
