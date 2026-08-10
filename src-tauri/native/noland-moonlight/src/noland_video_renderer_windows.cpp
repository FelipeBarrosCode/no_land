#include "noland_video_renderer.h"
#include "Limelight.h"

#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <new>
#include <vector>

struct nl_windows_video_context {
  HWND hwnd;
  IMFTransform* decoder;
  GUID output_subtype;
  MFT_OUTPUT_STREAM_INFO output_info;
  int video_format;
  int width;
  int height;
  int redraw_rate;
  std::vector<uint8_t> bgra;
  HANDLE frame_thread;
  CRITICAL_SECTION mutex;
  volatile LONG running;
  bool mf_started;
  bool com_initialized;
};

static nl_windows_video_context* nl_windows_context(nl_video_renderer_t* renderer) {
  return renderer == nullptr
             ? nullptr
             : static_cast<nl_windows_video_context*>(renderer->platform_context);
}

static nl_windows_video_context* nl_windows_ensure_context(nl_video_renderer_t* renderer) {
  nl_windows_video_context* context = nl_windows_context(renderer);
  if (context != nullptr) return context;
  context = new (std::nothrow) nl_windows_video_context();
  if (context == nullptr) return nullptr;
  std::memset(&context->output_subtype, 0, sizeof(context->output_subtype));
  std::memset(&context->output_info, 0, sizeof(context->output_info));
  context->hwnd = nullptr;
  context->decoder = nullptr;
  context->video_format = 0;
  context->width = 0;
  context->height = 0;
  context->redraw_rate = 0;
  context->frame_thread = nullptr;
  context->running = FALSE;
  context->mf_started = false;
  context->com_initialized = false;
  InitializeCriticalSection(&context->mutex);
  renderer->platform_context = context;
  return context;
}

static void nl_release_decoder(nl_windows_video_context* context) {
  if (context == nullptr) return;
  if (context->decoder != nullptr) {
    context->decoder->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
    context->decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
    context->decoder->Release();
    context->decoder = nullptr;
  }
  std::memset(&context->output_info, 0, sizeof(context->output_info));
  std::memset(&context->output_subtype, 0, sizeof(context->output_subtype));
}

static HRESULT nl_set_decoder_output_type(nl_windows_video_context* context) {
  IMFMediaType* chosen = nullptr;
  IMFMediaType* candidate = nullptr;
  GUID subtype = GUID_NULL;
  HRESULT result = MF_E_INVALIDMEDIATYPE;

  for (DWORD index = 0;
       context->decoder->GetOutputAvailableType(0, index, &candidate) == S_OK;
       ++index) {
    GUID current = GUID_NULL;
    if (SUCCEEDED(candidate->GetGUID(MF_MT_SUBTYPE, &current)) &&
        (current == MFVideoFormat_NV12 || current == MFVideoFormat_YUY2 ||
         current == MFVideoFormat_RGB32)) {
      if (chosen == nullptr || current == MFVideoFormat_NV12) {
        if (chosen != nullptr) chosen->Release();
        chosen = candidate;
        candidate = nullptr;
        subtype = current;
        if (current == MFVideoFormat_NV12) break;
      }
    }
    if (candidate != nullptr) {
      candidate->Release();
      candidate = nullptr;
    }
  }

  if (chosen != nullptr) {
    result = context->decoder->SetOutputType(0, chosen, 0);
    if (SUCCEEDED(result)) {
      context->output_subtype = subtype;
      result = context->decoder->GetOutputStreamInfo(0, &context->output_info);
    }
    chosen->Release();
  }
  return result;
}

static HRESULT nl_create_decoder(nl_windows_video_context* context) {
  IMFActivate** activations = nullptr;
  UINT32 activation_count = 0;
  IMFMediaType* input_type = nullptr;
  GUID input_subtype = MFVideoFormat_H264;
  HRESULT result;

  nl_release_decoder(context);
  if ((context->video_format & VIDEO_FORMAT_MASK_H264) != 0) {
    input_subtype = MFVideoFormat_H264;
  } else if ((context->video_format & VIDEO_FORMAT_MASK_H265) != 0) {
    input_subtype = MFVideoFormat_HEVC;
  } else {
    return MF_E_INVALIDMEDIATYPE;
  }

  MFT_REGISTER_TYPE_INFO input_info = {MFMediaType_Video, input_subtype};
  result = MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER,
                     MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT |
                         MFT_ENUM_FLAG_SORTANDFILTER,
                     &input_info,
                     nullptr,
                     &activations,
                     &activation_count);
  if (FAILED(result) || activation_count == 0) {
    if (activations != nullptr) CoTaskMemFree(activations);
    return FAILED(result) ? result : MF_E_TOPO_CODEC_NOT_FOUND;
  }

  for (UINT32 index = 0; index < activation_count; ++index) {
    result = activations[index]->ActivateObject(IID_PPV_ARGS(&context->decoder));
    if (SUCCEEDED(result) && context->decoder != nullptr) break;
  }
  for (UINT32 index = 0; index < activation_count; ++index) {
    activations[index]->Release();
  }
  CoTaskMemFree(activations);
  if (context->decoder == nullptr) return FAILED(result) ? result : E_FAIL;

  result = MFCreateMediaType(&input_type);
  if (SUCCEEDED(result)) result = input_type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
  if (SUCCEEDED(result)) result = input_type->SetGUID(MF_MT_SUBTYPE, input_subtype);
  if (SUCCEEDED(result)) {
    result = MFSetAttributeSize(input_type, MF_MT_FRAME_SIZE,
                                static_cast<UINT32>(context->width),
                                static_cast<UINT32>(context->height));
  }
  if (SUCCEEDED(result)) {
    result = MFSetAttributeRatio(input_type, MF_MT_FRAME_RATE,
                                 static_cast<UINT32>(context->redraw_rate > 0
                                                         ? context->redraw_rate
                                                         : 60),
                                 1);
  }
  if (SUCCEEDED(result)) {
    result = input_type->SetUINT32(MF_MT_INTERLACE_MODE,
                                   MFVideoInterlace_Progressive);
  }
  if (SUCCEEDED(result)) result = input_type->SetUINT32(MF_MT_ALL_SAMPLES_INDEPENDENT, FALSE);
  if (SUCCEEDED(result)) result = context->decoder->SetInputType(0, input_type, 0);
  if (input_type != nullptr) input_type->Release();
  if (FAILED(result)) {
    nl_release_decoder(context);
    return result;
  }

  result = nl_set_decoder_output_type(context);
  if (FAILED(result)) {
    nl_release_decoder(context);
    return result;
  }

  context->decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
  context->decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
  context->bgra.resize(static_cast<size_t>(context->width) *
                       static_cast<size_t>(context->height) * 4U);
  return S_OK;
}

static uint8_t nl_clamp_channel(int value) {
  return static_cast<uint8_t>(std::max(0, std::min(255, value)));
}

static void nl_yuv_to_bgra(uint8_t y,
                           uint8_t u,
                           uint8_t v,
                           uint8_t* output) {
  int c = std::max(0, static_cast<int>(y) - 16);
  int d = static_cast<int>(u) - 128;
  int e = static_cast<int>(v) - 128;
  output[0] = nl_clamp_channel((298 * c + 516 * d + 128) >> 8);
  output[1] = nl_clamp_channel((298 * c - 100 * d - 208 * e + 128) >> 8);
  output[2] = nl_clamp_channel((298 * c + 409 * e + 128) >> 8);
  output[3] = 255;
}

static void nl_convert_nv12(nl_windows_video_context* context,
                            const uint8_t* data,
                            LONG stride) {
  LONG abs_stride = stride < 0 ? -stride : stride;
  const uint8_t* y_plane = data;
  const uint8_t* uv_plane = data + static_cast<size_t>(abs_stride) *
                                       static_cast<size_t>(context->height);
  for (int row = 0; row < context->height; ++row) {
    const uint8_t* y_row = y_plane + static_cast<size_t>(row) * abs_stride;
    const uint8_t* uv_row = uv_plane + static_cast<size_t>(row / 2) * abs_stride;
    uint8_t* output = context->bgra.data() +
                      static_cast<size_t>(row) * context->width * 4U;
    for (int column = 0; column < context->width; ++column) {
      nl_yuv_to_bgra(y_row[column], uv_row[(column / 2) * 2],
                     uv_row[(column / 2) * 2 + 1], output + column * 4);
    }
  }
}

static void nl_convert_yuy2(nl_windows_video_context* context,
                            const uint8_t* data,
                            LONG stride) {
  LONG abs_stride = stride < 0 ? -stride : stride;
  for (int row = 0; row < context->height; ++row) {
    const uint8_t* source = data + static_cast<size_t>(row) * abs_stride;
    uint8_t* output = context->bgra.data() +
                      static_cast<size_t>(row) * context->width * 4U;
    for (int column = 0; column < context->width; column += 2) {
      uint8_t y0 = source[column * 2];
      uint8_t u = source[column * 2 + 1];
      uint8_t y1 = source[column * 2 + 2];
      uint8_t v = source[column * 2 + 3];
      nl_yuv_to_bgra(y0, u, v, output + column * 4);
      if (column + 1 < context->width) {
        nl_yuv_to_bgra(y1, u, v, output + (column + 1) * 4);
      }
    }
  }
}

static void nl_present_bgra(nl_windows_video_context* context) {
  HWND hwnd;
  RECT client;
  HDC dc;
  HBRUSH black;
  int target_width;
  int target_height;
  int target_x;
  int target_y;
  double source_aspect;
  double target_aspect;
  BITMAPINFO bitmap;

  EnterCriticalSection(&context->mutex);
  hwnd = context->hwnd;
  LeaveCriticalSection(&context->mutex);
  if (hwnd == nullptr || !IsWindow(hwnd) || !GetClientRect(hwnd, &client)) return;

  dc = GetDC(hwnd);
  if (dc == nullptr) return;
  black = static_cast<HBRUSH>(GetStockObject(BLACK_BRUSH));
  FillRect(dc, &client, black);

  target_width = client.right - client.left;
  target_height = client.bottom - client.top;
  source_aspect = static_cast<double>(context->width) / context->height;
  target_aspect = target_height > 0
                      ? static_cast<double>(target_width) / target_height
                      : source_aspect;
  if (target_aspect > source_aspect) {
    target_width = static_cast<int>(target_height * source_aspect);
  } else {
    target_height = static_cast<int>(target_width / source_aspect);
  }
  target_x = ((client.right - client.left) - target_width) / 2;
  target_y = ((client.bottom - client.top) - target_height) / 2;

  std::memset(&bitmap, 0, sizeof(bitmap));
  bitmap.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
  bitmap.bmiHeader.biWidth = context->width;
  bitmap.bmiHeader.biHeight = -context->height;
  bitmap.bmiHeader.biPlanes = 1;
  bitmap.bmiHeader.biBitCount = 32;
  bitmap.bmiHeader.biCompression = BI_RGB;
  SetStretchBltMode(dc, HALFTONE);
  StretchDIBits(dc,
                target_x,
                target_y,
                target_width,
                target_height,
                0,
                0,
                context->width,
                context->height,
                context->bgra.data(),
                &bitmap,
                DIB_RGB_COLORS,
                SRCCOPY);
  ReleaseDC(hwnd, dc);
}

static HRESULT nl_render_sample(nl_windows_video_context* context,
                                IMFSample* sample) {
  IMFMediaBuffer* buffer = nullptr;
  IMF2DBuffer* buffer_2d = nullptr;
  BYTE* data = nullptr;
  LONG stride = 0;
  DWORD max_length = 0;
  DWORD current_length = 0;
  bool locked_2d = false;
  HRESULT result;

  if (sample == nullptr) return E_POINTER;
  result = sample->ConvertToContiguousBuffer(&buffer);
  if (FAILED(result)) return result;

  if (SUCCEEDED(buffer->QueryInterface(IID_PPV_ARGS(&buffer_2d)))) {
    result = buffer_2d->Lock2D(&data, &stride);
    locked_2d = SUCCEEDED(result);
  } else {
    result = buffer->Lock(&data, &max_length, &current_length);
    if (SUCCEEDED(result)) {
      stride = context->output_subtype == MFVideoFormat_RGB32
                   ? context->width * 4
                   : context->output_subtype == MFVideoFormat_YUY2
                         ? context->width * 2
                         : context->width;
    }
  }

  if (SUCCEEDED(result) && data != nullptr) {
    if (context->output_subtype == MFVideoFormat_NV12) {
      nl_convert_nv12(context, data, stride);
    } else if (context->output_subtype == MFVideoFormat_YUY2) {
      nl_convert_yuy2(context, data, stride);
    } else if (context->output_subtype == MFVideoFormat_RGB32) {
      LONG abs_stride = stride < 0 ? -stride : stride;
      for (int row = 0; row < context->height; ++row) {
        const uint8_t* source = data + static_cast<size_t>(row) * abs_stride;
        std::memcpy(context->bgra.data() +
                        static_cast<size_t>(row) * context->width * 4U,
                    source,
                    static_cast<size_t>(context->width) * 4U);
      }
    }
    nl_present_bgra(context);
  }

  if (locked_2d) {
    buffer_2d->Unlock2D();
  } else if (data != nullptr) {
    buffer->Unlock();
  }
  if (buffer_2d != nullptr) buffer_2d->Release();
  buffer->Release();
  return result;
}

static IMFSample* nl_create_output_sample(nl_windows_video_context* context) {
  IMFSample* sample = nullptr;
  IMFMediaBuffer* buffer = nullptr;
  DWORD size = std::max<DWORD>(context->output_info.cbSize,
                               static_cast<DWORD>(context->width *
                                                  context->height * 4));
  if (FAILED(MFCreateSample(&sample))) return nullptr;
  if (FAILED(MFCreateMemoryBuffer(size, &buffer)) ||
      FAILED(sample->AddBuffer(buffer))) {
    if (buffer != nullptr) buffer->Release();
    sample->Release();
    return nullptr;
  }
  buffer->Release();
  return sample;
}

static HRESULT nl_drain_decoder(nl_windows_video_context* context) {
  while (context != nullptr && context->decoder != nullptr) {
    MFT_OUTPUT_DATA_BUFFER output;
    DWORD status = 0;
    std::memset(&output, 0, sizeof(output));
    if ((context->output_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES) == 0 &&
        (context->output_info.dwFlags & MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES) == 0) {
      output.pSample = nl_create_output_sample(context);
      if (output.pSample == nullptr) return E_OUTOFMEMORY;
    }

    HRESULT result = context->decoder->ProcessOutput(0, 1, &output, &status);
    if (result == MF_E_TRANSFORM_NEED_MORE_INPUT) {
      if (output.pSample != nullptr) output.pSample->Release();
      return S_OK;
    }
    if (result == MF_E_TRANSFORM_STREAM_CHANGE) {
      if (output.pSample != nullptr) output.pSample->Release();
      result = nl_set_decoder_output_type(context);
      if (FAILED(result)) return result;
      continue;
    }
    if (FAILED(result)) {
      if (output.pSample != nullptr) output.pSample->Release();
      return result;
    }

    if (output.pSample != nullptr) {
      nl_render_sample(context, output.pSample);
      output.pSample->Release();
    }
    if (output.pEvents != nullptr) output.pEvents->Release();
  }
  return S_OK;
}

static DWORD WINAPI nl_windows_frame_thread(LPVOID data) {
  nl_video_renderer_t* renderer = static_cast<nl_video_renderer_t*>(data);
  while (renderer != nullptr) {
    nl_windows_video_context* context = nl_windows_context(renderer);
    VIDEO_FRAME_HANDLE handle;
    PDECODE_UNIT decode_unit;
    nl_video_frame_metadata_t metadata;
    bool submitted = false;
    if (context == nullptr || InterlockedCompareExchange(&context->running, TRUE, TRUE) == FALSE) {
      break;
    }

    while (LiPollNextVideoFrame(&handle, &decode_unit)) {
      int result;
      std::memset(&metadata, 0, sizeof(metadata));
      metadata.frame_number = decode_unit->frameNumber;
      metadata.frame_type = decode_unit->frameType;
      metadata.full_length = decode_unit->fullLength;
      metadata.host_processing_latency = decode_unit->frameHostProcessingLatency;
      metadata.receive_time_us = decode_unit->receiveTimeUs;
      metadata.enqueue_time_us = decode_unit->enqueueTimeUs;
      metadata.presentation_time_us = decode_unit->presentationTimeUs;
      metadata.rtp_timestamp = decode_unit->rtpTimestamp;
      metadata.hdr_active = decode_unit->hdrActive ? 1U : 0U;
      metadata.colorspace = decode_unit->colorspace;
      result = renderer->frame_processor != nullptr
                   ? renderer->frame_processor(renderer->frame_processor_user_data,
                                               decode_unit,
                                               &metadata)
                   : nl_video_renderer_submit_frame(renderer, decode_unit, &metadata);
      LiCompleteVideoFrame(handle, result);
      submitted = true;
      if (LiGetPendingVideoFrames() <= 1) break;
    }
    if (!submitted) Sleep(1);
  }
  return 0;
}

extern "C" void nl_video_renderer_platform_attach_surface(
    nl_video_renderer_t* renderer,
    const nl_surface_descriptor_t* surface) {
  nl_windows_video_context* context = nl_windows_ensure_context(renderer);
  if (context == nullptr || surface == nullptr ||
      surface->surface_type != NL_SURFACE_WINDOWS_HWND ||
      surface->window_handle == nullptr) {
    return;
  }
  EnterCriticalSection(&context->mutex);
  context->hwnd = static_cast<HWND>(surface->window_handle);
  LeaveCriticalSection(&context->mutex);
}

extern "C" void nl_video_renderer_platform_detach_surface(
    nl_video_renderer_t* renderer) {
  nl_windows_video_context* context = nl_windows_context(renderer);
  if (context == nullptr) return;
  EnterCriticalSection(&context->mutex);
  context->hwnd = nullptr;
  LeaveCriticalSection(&context->mutex);
}

extern "C" int nl_video_renderer_platform_setup(nl_video_renderer_t* renderer,
                                                 int video_format,
                                                 int width,
                                                 int height,
                                                 int redraw_rate) {
  nl_windows_video_context* context = nl_windows_ensure_context(renderer);
  HRESULT result;
  if (context == nullptr) return -1;
  if (!context->com_initialized) {
    result = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    if (SUCCEEDED(result)) {
      context->com_initialized = true;
    } else if (result != RPC_E_CHANGED_MODE) {
      return static_cast<int>(result);
    }
  }
  if (!context->mf_started) {
    result = MFStartup(MF_VERSION, MFSTARTUP_LITE);
    if (FAILED(result)) return static_cast<int>(result);
    context->mf_started = true;
  }
  context->video_format = video_format;
  context->width = width;
  context->height = height;
  context->redraw_rate = redraw_rate;
  result = nl_create_decoder(context);
  if (FAILED(result)) {
    std::fprintf(stderr,
                 "[noland-video] Media Foundation decoder setup failed: 0x%08lx\n",
                 static_cast<unsigned long>(result));
    return static_cast<int>(result);
  }
  return 0;
}

extern "C" void nl_video_renderer_platform_start(nl_video_renderer_t* renderer) {
  nl_windows_video_context* context = nl_windows_context(renderer);
  if (context == nullptr || context->decoder == nullptr ||
      context->frame_thread != nullptr) {
    return;
  }
  InterlockedExchange(&context->running, TRUE);
  context->frame_thread = CreateThread(nullptr, 0, nl_windows_frame_thread,
                                       renderer, 0, nullptr);
  if (context->frame_thread == nullptr) {
    InterlockedExchange(&context->running, FALSE);
  }
}

extern "C" void nl_video_renderer_platform_stop(nl_video_renderer_t* renderer) {
  nl_windows_video_context* context = nl_windows_context(renderer);
  if (context == nullptr) return;
  InterlockedExchange(&context->running, FALSE);
  if (context->frame_thread != nullptr) {
    WaitForSingleObject(context->frame_thread, INFINITE);
    CloseHandle(context->frame_thread);
    context->frame_thread = nullptr;
  }
  if (context->decoder != nullptr) {
    context->decoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
    nl_drain_decoder(context);
    context->decoder->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
  }
}

extern "C" void nl_video_renderer_platform_cleanup(nl_video_renderer_t* renderer) {
  nl_windows_video_context* context = nl_windows_context(renderer);
  if (context == nullptr) return;
  nl_video_renderer_platform_stop(renderer);
  nl_release_decoder(context);
  if (context->mf_started) {
    MFShutdown();
    context->mf_started = false;
  }
  if (context->com_initialized) {
    CoUninitialize();
    context->com_initialized = false;
  }
  DeleteCriticalSection(&context->mutex);
  delete context;
  renderer->platform_context = nullptr;
}

extern "C" int nl_video_renderer_platform_submit_frame(
    nl_video_renderer_t* renderer,
    const void* raw_decode_unit,
    const nl_video_frame_metadata_t* frame) {
  nl_windows_video_context* context = nl_windows_context(renderer);
  const DECODE_UNIT* decode_unit = static_cast<const DECODE_UNIT*>(raw_decode_unit);
  const LENTRY* entry;
  IMFSample* sample = nullptr;
  IMFMediaBuffer* buffer = nullptr;
  BYTE* destination = nullptr;
  DWORD maximum = 0;
  DWORD current = 0;
  size_t total = 0;
  size_t offset = 0;
  HRESULT result;

  (void)frame;
  if (context == nullptr || context->decoder == nullptr || decode_unit == nullptr) return DR_OK;
  for (entry = decode_unit->bufferList; entry != nullptr; entry = entry->next) {
    if (entry->data != nullptr && entry->length > 0) total += static_cast<size_t>(entry->length);
  }
  if (total == 0 || total > MAXDWORD) return DR_OK;

  result = MFCreateSample(&sample);
  if (SUCCEEDED(result)) result = MFCreateMemoryBuffer(static_cast<DWORD>(total), &buffer);
  if (SUCCEEDED(result)) result = buffer->Lock(&destination, &maximum, &current);
  if (SUCCEEDED(result)) {
    for (entry = decode_unit->bufferList; entry != nullptr; entry = entry->next) {
      if (entry->data == nullptr || entry->length <= 0) continue;
      std::memcpy(destination + offset, entry->data, static_cast<size_t>(entry->length));
      offset += static_cast<size_t>(entry->length);
    }
    buffer->Unlock();
    destination = nullptr;
    buffer->SetCurrentLength(static_cast<DWORD>(total));
    result = sample->AddBuffer(buffer);
  }
  if (SUCCEEDED(result)) {
    sample->SetSampleTime(static_cast<LONGLONG>(decode_unit->presentationTimeUs) * 10LL);
    if (context->redraw_rate > 0) {
      sample->SetSampleDuration(10000000LL / context->redraw_rate);
    }
    result = context->decoder->ProcessInput(0, sample, 0);
    if (result == MF_E_NOTACCEPTING) {
      result = nl_drain_decoder(context);
      if (SUCCEEDED(result)) result = context->decoder->ProcessInput(0, sample, 0);
    }
    if (SUCCEEDED(result)) result = nl_drain_decoder(context);
  }

  if (destination != nullptr) buffer->Unlock();
  if (buffer != nullptr) buffer->Release();
  if (sample != nullptr) sample->Release();
  if (FAILED(result)) {
    std::fprintf(stderr,
                 "[noland-video] Media Foundation frame decode failed: 0x%08lx\n",
                 static_cast<unsigned long>(result));
    return decode_unit->frameType == FRAME_TYPE_IDR ? DR_OK : DR_NEED_IDR;
  }
  return DR_OK;
}
