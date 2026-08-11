#include "noland_video_renderer.h"
#include "Limelight.h"

#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <codecapi.h>
#include <icodecapi.h>
#include <d3d10_1.h>
#include <d3d11_4.h>
#include <dxgi1_6.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <wrl/client.h>

#include <algorithm>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <deque>
#include <new>
#include <vector>

using Microsoft::WRL::ComPtr;

struct nl_windows_input_view {
  ComPtr<ID3D11Texture2D> texture;
  UINT subresource;
  ComPtr<ID3D11VideoProcessorInputView> view;
};

enum class nl_windows_pipeline_mode {
  none,
  gpu,
  software,
};

struct nl_windows_video_context {
  HWND hwnd;
  HWND swap_chain_hwnd;
  IMFTransform* decoder;
  GUID output_subtype;
  MFT_OUTPUT_STREAM_INFO output_info;
  int video_format;
  int width;
  int height;
  int redraw_rate;
  HANDLE frame_thread;
  CRITICAL_SECTION mutex;
  volatile LONG running;
  bool mf_started;
  bool com_initialized;
  bool has_received_input;
  nl_windows_pipeline_mode pipeline_mode;
  std::vector<uint8_t> bgra;

  ComPtr<IDXGIFactory2> factory;
  ComPtr<IDXGIAdapter1> adapter;
  ComPtr<ID3D11Device> device;
  ComPtr<ID3D11DeviceContext> device_context;
  ComPtr<ID3D11VideoDevice> video_device;
  ComPtr<ID3D11VideoContext> video_context;
  ComPtr<ID3D11VideoContext1> video_context_1;
  ComPtr<IMFDXGIDeviceManager> device_manager;
  UINT device_manager_token;

  ComPtr<IDXGISwapChain1> swap_chain;
  ComPtr<ID3D11VideoProcessorEnumerator> video_processor_enumerator;
  ComPtr<ID3D11VideoProcessor> video_processor;
  ComPtr<ID3D11VideoProcessorOutputView> output_view;
  std::vector<nl_windows_input_view> input_views;
  std::deque<uint8_t> pending_colorspaces;
  UINT swap_chain_width;
  UINT swap_chain_height;
  UINT swap_chain_flags;
  UINT output_frame_number;
  bool allow_tearing;
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
  context->swap_chain_hwnd = nullptr;
  context->decoder = nullptr;
  context->video_format = 0;
  context->width = 0;
  context->height = 0;
  context->redraw_rate = 0;
  context->frame_thread = nullptr;
  context->running = FALSE;
  context->mf_started = false;
  context->com_initialized = false;
  context->has_received_input = false;
  context->pipeline_mode = nl_windows_pipeline_mode::none;
  context->device_manager_token = 0;
  context->swap_chain_width = 0;
  context->swap_chain_height = 0;
  context->swap_chain_flags = 0;
  context->output_frame_number = 0;
  context->allow_tearing = false;
  InitializeCriticalSection(&context->mutex);
  renderer->platform_context = context;
  return context;
}

static HWND nl_get_hwnd(nl_windows_video_context* context) {
  HWND hwnd = nullptr;
  EnterCriticalSection(&context->mutex);
  hwnd = context->hwnd;
  LeaveCriticalSection(&context->mutex);
  return hwnd;
}

static void nl_release_video_processor_resources(nl_windows_video_context* context) {
  if (context == nullptr) return;
  context->input_views.clear();
  context->output_view.Reset();
  context->video_processor.Reset();
  context->video_processor_enumerator.Reset();
}

static void nl_release_swap_chain(nl_windows_video_context* context) {
  if (context == nullptr) return;
  nl_release_video_processor_resources(context);
  context->swap_chain.Reset();
  context->swap_chain_hwnd = nullptr;
  context->swap_chain_width = 0;
  context->swap_chain_height = 0;
  context->swap_chain_flags = 0;
  context->allow_tearing = false;
}

static void nl_release_decoder(nl_windows_video_context* context) {
  if (context == nullptr) return;
  if (context->decoder != nullptr) {
    context->decoder->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
    context->decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
    context->decoder->ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, 0);
    context->decoder->Release();
    context->decoder = nullptr;
  }
  context->pending_colorspaces.clear();
  context->has_received_input = false;
  context->pipeline_mode = nl_windows_pipeline_mode::none;
  context->bgra.clear();
  std::memset(&context->output_info, 0, sizeof(context->output_info));
  std::memset(&context->output_subtype, 0, sizeof(context->output_subtype));
}

static void nl_release_d3d_pipeline(nl_windows_video_context* context) {
  if (context == nullptr) return;
  nl_release_swap_chain(context);
  context->device_manager.Reset();
  context->video_context_1.Reset();
  context->video_context.Reset();
  context->video_device.Reset();
  if (context->device_context != nullptr) {
    context->device_context->ClearState();
    context->device_context->Flush();
  }
  context->device_context.Reset();
  context->device.Reset();
  context->adapter.Reset();
  context->factory.Reset();
  context->device_manager_token = 0;
}

static HRESULT nl_choose_window_adapter(nl_windows_video_context* context) {
  ComPtr<IDXGIAdapter1> first_hardware_adapter;
  HWND hwnd = nl_get_hwnd(context);
  HMONITOR target_monitor = hwnd != nullptr && IsWindow(hwnd)
                                ? MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)
                                : nullptr;

  for (UINT adapter_index = 0;; ++adapter_index) {
    ComPtr<IDXGIAdapter1> adapter;
    HRESULT result = context->factory->EnumAdapters1(adapter_index, &adapter);
    if (result == DXGI_ERROR_NOT_FOUND) break;
    if (FAILED(result)) return result;

    DXGI_ADAPTER_DESC1 adapter_desc = {};
    result = adapter->GetDesc1(&adapter_desc);
    if (FAILED(result)) continue;
    if ((adapter_desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) != 0) continue;
    if (first_hardware_adapter == nullptr) first_hardware_adapter = adapter;

    if (target_monitor != nullptr) {
      for (UINT output_index = 0;; ++output_index) {
        ComPtr<IDXGIOutput> output;
        result = adapter->EnumOutputs(output_index, &output);
        if (result == DXGI_ERROR_NOT_FOUND) break;
        if (FAILED(result)) continue;
        DXGI_OUTPUT_DESC output_desc = {};
        if (SUCCEEDED(output->GetDesc(&output_desc)) &&
            output_desc.Monitor == target_monitor) {
          context->adapter = adapter;
          return S_OK;
        }
      }
    }
  }

  if (first_hardware_adapter == nullptr) return DXGI_ERROR_NOT_FOUND;
  context->adapter = first_hardware_adapter;
  return S_OK;
}

static HRESULT nl_create_d3d_pipeline(nl_windows_video_context* context) {
  static const D3D_FEATURE_LEVEL feature_levels[] = {
      D3D_FEATURE_LEVEL_11_1,
      D3D_FEATURE_LEVEL_11_0,
  };
  static const D3D_FEATURE_LEVEL fallback_feature_levels[] = {
      D3D_FEATURE_LEVEL_11_0,
  };
  D3D_FEATURE_LEVEL selected_feature_level = D3D_FEATURE_LEVEL_11_0;
  UINT creation_flags = D3D11_CREATE_DEVICE_VIDEO_SUPPORT |
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT;
  HRESULT result;

  nl_release_d3d_pipeline(context);
  result = CreateDXGIFactory1(IID_PPV_ARGS(context->factory.GetAddressOf()));
  if (FAILED(result)) return result;
  result = nl_choose_window_adapter(context);
  if (FAILED(result)) return result;

  result = D3D11CreateDevice(context->adapter.Get(),
                             D3D_DRIVER_TYPE_UNKNOWN,
                             nullptr,
                             creation_flags,
                             feature_levels,
                             ARRAYSIZE(feature_levels),
                             D3D11_SDK_VERSION,
                             &context->device,
                             &selected_feature_level,
                             &context->device_context);
  if (result == E_INVALIDARG) {
    result = D3D11CreateDevice(context->adapter.Get(),
                               D3D_DRIVER_TYPE_UNKNOWN,
                               nullptr,
                               creation_flags,
                               fallback_feature_levels,
                               ARRAYSIZE(fallback_feature_levels),
                               D3D11_SDK_VERSION,
                               &context->device,
                               &selected_feature_level,
                               &context->device_context);
  }
  if (FAILED(result)) return result;

  ComPtr<ID3D10Multithread> multithread;
  if (SUCCEEDED(context->device.As(&multithread))) {
    multithread->SetMultithreadProtected(TRUE);
  }

  result = context->device.As(&context->video_device);
  if (FAILED(result)) return result;
  result = context->device_context.As(&context->video_context);
  if (FAILED(result)) return result;
  context->device_context.As(&context->video_context_1);

  BOOL decoder_format_supported = FALSE;
  result = context->video_device->CheckVideoDecoderFormat(
      &D3D11_DECODER_PROFILE_H264_VLD_NOFGT,
      DXGI_FORMAT_NV12,
      &decoder_format_supported);
  if (FAILED(result)) return result;
  if (!decoder_format_supported) return MF_E_UNSUPPORTED_D3D_TYPE;

  result = MFCreateDXGIDeviceManager(&context->device_manager_token,
                                     &context->device_manager);
  if (FAILED(result)) return result;
  result = context->device_manager->ResetDevice(context->device.Get(),
                                                 context->device_manager_token);
  if (FAILED(result)) return result;
  return S_OK;
}

static HRESULT nl_set_gpu_decoder_output_type(nl_windows_video_context* context) {
  ComPtr<IMFMediaType> chosen;
  HRESULT result = MF_E_INVALIDMEDIATYPE;

  for (DWORD index = 0;; ++index) {
    ComPtr<IMFMediaType> candidate;
    result = context->decoder->GetOutputAvailableType(0, index, &candidate);
    if (result == MF_E_NO_MORE_TYPES) break;
    if (FAILED(result)) return result;
    GUID subtype = GUID_NULL;
    if (SUCCEEDED(candidate->GetGUID(MF_MT_SUBTYPE, &subtype)) &&
        subtype == MFVideoFormat_NV12) {
      chosen = candidate;
      break;
    }
  }

  if (chosen == nullptr) return MF_E_INVALIDMEDIATYPE;
  result = context->decoder->SetOutputType(0, chosen.Get(), 0);
  if (FAILED(result)) return result;
  context->output_subtype = MFVideoFormat_NV12;
  result = context->decoder->GetOutputStreamInfo(0, &context->output_info);
  if (FAILED(result)) return result;
  if ((context->output_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES) == 0 &&
      (context->output_info.dwFlags & MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES) == 0) {
    return MF_E_UNSUPPORTED_D3D_TYPE;
  }
  return S_OK;
}

static HRESULT nl_create_gpu_decoder(nl_windows_video_context* context) {
  IMFActivate** activations = nullptr;
  UINT32 activation_count = 0;
  ComPtr<IMFMediaType> input_type;
  HRESULT result;

  nl_release_decoder(context);
  if (context->video_format != VIDEO_FORMAT_H264) {
    return MF_E_INVALIDMEDIATYPE;
  }

  MFT_REGISTER_TYPE_INFO input_info = {MFMediaType_Video, MFVideoFormat_H264};
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

  result = MF_E_TOPO_CODEC_NOT_FOUND;
  for (UINT32 index = 0; index < activation_count; ++index) {
    IMFTransform* candidate = nullptr;
    HRESULT candidate_result = activations[index]->ActivateObject(IID_PPV_ARGS(&candidate));
    if (SUCCEEDED(candidate_result) && candidate != nullptr) {
      ComPtr<IMFAttributes> attributes;
      UINT32 d3d11_aware = FALSE;
      if (SUCCEEDED(candidate->GetAttributes(&attributes)) &&
          SUCCEEDED(attributes->GetUINT32(MF_SA_D3D11_AWARE, &d3d11_aware)) &&
          d3d11_aware) {
        context->decoder = candidate;
        candidate = nullptr;
        result = S_OK;
        break;
      }
      candidate->Release();
      result = candidate_result;
    }
  }
  for (UINT32 index = 0; index < activation_count; ++index) {
    activations[index]->Release();
  }
  CoTaskMemFree(activations);
  if (context->decoder == nullptr) {
    return FAILED(result) ? result : MF_E_UNSUPPORTED_D3D_TYPE;
  }

  result = context->decoder->ProcessMessage(
      MFT_MESSAGE_SET_D3D_MANAGER,
      reinterpret_cast<ULONG_PTR>(context->device_manager.Get()));
  if (FAILED(result)) {
    nl_release_decoder(context);
    return result;
  }

  ComPtr<IMFAttributes> decoder_attributes;
  if (SUCCEEDED(context->decoder->GetAttributes(&decoder_attributes))) {
    decoder_attributes->SetUINT32(MF_LOW_LATENCY, TRUE);
  }
  ComPtr<ICodecAPI> codec_api;
  if (SUCCEEDED(context->decoder->QueryInterface(IID_PPV_ARGS(&codec_api)))) {
    VARIANT low_latency;
    VariantInit(&low_latency);
    low_latency.vt = VT_BOOL;
    low_latency.boolVal = VARIANT_TRUE;
    codec_api->SetValue(&CODECAPI_AVLowLatencyMode, &low_latency);
    VariantClear(&low_latency);
  }

  result = MFCreateMediaType(&input_type);
  if (SUCCEEDED(result)) {
    result = input_type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
  }
  if (SUCCEEDED(result)) {
    result = input_type->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_H264);
  }
  if (SUCCEEDED(result)) {
    result = MFSetAttributeSize(input_type.Get(),
                                MF_MT_FRAME_SIZE,
                                static_cast<UINT32>(context->width),
                                static_cast<UINT32>(context->height));
  }
  if (SUCCEEDED(result)) {
    result = MFSetAttributeRatio(input_type.Get(),
                                 MF_MT_FRAME_RATE,
                                 static_cast<UINT32>(context->redraw_rate > 0
                                                         ? context->redraw_rate
                                                         : 60),
                                 1);
  }
  if (SUCCEEDED(result)) {
    result = MFSetAttributeRatio(input_type.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1);
  }
  if (SUCCEEDED(result)) {
    result = input_type->SetUINT32(MF_MT_INTERLACE_MODE,
                                   MFVideoInterlace_Progressive);
  }
  if (SUCCEEDED(result)) {
    result = input_type->SetUINT32(MF_MT_ALL_SAMPLES_INDEPENDENT, FALSE);
  }
  if (SUCCEEDED(result)) {
    result = context->decoder->SetInputType(0, input_type.Get(), 0);
  }
  if (FAILED(result)) {
    nl_release_decoder(context);
    return result;
  }

  ComPtr<IMFAttributes> output_attributes;
  if (SUCCEEDED(context->decoder->GetOutputStreamAttributes(0, &output_attributes))) {
    output_attributes->SetUINT32(MF_SA_D3D11_BINDFLAGS, D3D11_BIND_DECODER);
  }

  result = nl_set_gpu_decoder_output_type(context);
  if (FAILED(result)) {
    nl_release_decoder(context);
    return result;
  }

  result = context->decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
  if (SUCCEEDED(result)) {
    result = context->decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
  }
  if (FAILED(result)) {
    nl_release_decoder(context);
    return result;
  }
  context->pipeline_mode = nl_windows_pipeline_mode::gpu;
  return S_OK;
}

static HRESULT nl_set_software_decoder_output_type(
    nl_windows_video_context* context) {
  ComPtr<IMFMediaType> chosen;
  GUID chosen_subtype = GUID_NULL;
  int chosen_priority = 100;

  for (DWORD index = 0;; ++index) {
    ComPtr<IMFMediaType> candidate;
    HRESULT result = context->decoder->GetOutputAvailableType(0, index, &candidate);
    if (result == MF_E_NO_MORE_TYPES) break;
    if (FAILED(result)) return result;
    GUID subtype = GUID_NULL;
    if (FAILED(candidate->GetGUID(MF_MT_SUBTYPE, &subtype))) continue;

    int priority = 100;
    if (subtype == MFVideoFormat_NV12) {
      priority = 0;
    } else if (subtype == MFVideoFormat_YUY2) {
      priority = 1;
    } else if (subtype == MFVideoFormat_RGB32) {
      priority = 2;
    }
    if (priority < chosen_priority) {
      chosen = candidate;
      chosen_subtype = subtype;
      chosen_priority = priority;
      if (priority == 0) break;
    }
  }

  if (chosen == nullptr) return MF_E_INVALIDMEDIATYPE;
  HRESULT result = context->decoder->SetOutputType(0, chosen.Get(), 0);
  if (FAILED(result)) return result;
  context->output_subtype = chosen_subtype;
  return context->decoder->GetOutputStreamInfo(0, &context->output_info);
}

static HRESULT nl_create_software_decoder(nl_windows_video_context* context) {
  IMFActivate** activations = nullptr;
  UINT32 activation_count = 0;
  ComPtr<IMFMediaType> input_type;
  HRESULT result;

  nl_release_decoder(context);
  if (context->video_format != VIDEO_FORMAT_H264) {
    return MF_E_INVALIDMEDIATYPE;
  }

  MFT_REGISTER_TYPE_INFO input_info = {MFMediaType_Video, MFVideoFormat_H264};
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

  result = MF_E_TOPO_CODEC_NOT_FOUND;
  for (UINT32 index = 0; index < activation_count; ++index) {
    IMFTransform* candidate = nullptr;
    HRESULT candidate_result = activations[index]->ActivateObject(IID_PPV_ARGS(&candidate));
    if (SUCCEEDED(candidate_result) && candidate != nullptr) {
      context->decoder = candidate;
      result = S_OK;
      break;
    }
    result = candidate_result;
  }
  for (UINT32 index = 0; index < activation_count; ++index) {
    activations[index]->Release();
  }
  CoTaskMemFree(activations);
  if (context->decoder == nullptr) {
    return FAILED(result) ? result : MF_E_TOPO_CODEC_NOT_FOUND;
  }

  ComPtr<IMFAttributes> decoder_attributes;
  if (SUCCEEDED(context->decoder->GetAttributes(&decoder_attributes))) {
    decoder_attributes->SetUINT32(MF_LOW_LATENCY, TRUE);
  }
  ComPtr<ICodecAPI> codec_api;
  if (SUCCEEDED(context->decoder->QueryInterface(IID_PPV_ARGS(&codec_api)))) {
    VARIANT low_latency;
    VariantInit(&low_latency);
    low_latency.vt = VT_BOOL;
    low_latency.boolVal = VARIANT_TRUE;
    codec_api->SetValue(&CODECAPI_AVLowLatencyMode, &low_latency);
    VariantClear(&low_latency);
  }

  result = MFCreateMediaType(&input_type);
  if (SUCCEEDED(result)) {
    result = input_type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
  }
  if (SUCCEEDED(result)) {
    result = input_type->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_H264);
  }
  if (SUCCEEDED(result)) {
    result = MFSetAttributeSize(input_type.Get(),
                                MF_MT_FRAME_SIZE,
                                static_cast<UINT32>(context->width),
                                static_cast<UINT32>(context->height));
  }
  if (SUCCEEDED(result)) {
    result = MFSetAttributeRatio(input_type.Get(),
                                 MF_MT_FRAME_RATE,
                                 static_cast<UINT32>(context->redraw_rate > 0
                                                         ? context->redraw_rate
                                                         : 60),
                                 1);
  }
  if (SUCCEEDED(result)) {
    result = MFSetAttributeRatio(input_type.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1);
  }
  if (SUCCEEDED(result)) {
    result = input_type->SetUINT32(MF_MT_INTERLACE_MODE,
                                   MFVideoInterlace_Progressive);
  }
  if (SUCCEEDED(result)) {
    result = input_type->SetUINT32(MF_MT_ALL_SAMPLES_INDEPENDENT, FALSE);
  }
  if (SUCCEEDED(result)) {
    result = context->decoder->SetInputType(0, input_type.Get(), 0);
  }
  if (FAILED(result)) {
    nl_release_decoder(context);
    return result;
  }

  result = nl_set_software_decoder_output_type(context);
  if (FAILED(result)) {
    nl_release_decoder(context);
    return result;
  }
  result = context->decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
  if (SUCCEEDED(result)) {
    result = context->decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
  }
  if (FAILED(result)) {
    nl_release_decoder(context);
    return result;
  }

  size_t pixel_count = static_cast<size_t>(context->width) *
                       static_cast<size_t>(context->height);
  if (pixel_count > static_cast<size_t>(MAXDWORD) / 4U) {
    nl_release_decoder(context);
    return E_OUTOFMEMORY;
  }
  try {
    context->bgra.resize(pixel_count * 4U);
  } catch (...) {
    nl_release_decoder(context);
    return E_OUTOFMEMORY;
  }
  context->pipeline_mode = nl_windows_pipeline_mode::software;
  return S_OK;
}

static HRESULT nl_create_video_processor_resources(
    nl_windows_video_context* context,
    UINT output_width,
    UINT output_height) {
  D3D11_VIDEO_PROCESSOR_CONTENT_DESC content_desc = {};
  content_desc.InputFrameFormat = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
  content_desc.InputFrameRate.Numerator =
      static_cast<UINT>(context->redraw_rate > 0 ? context->redraw_rate : 60);
  content_desc.InputFrameRate.Denominator = 1;
  content_desc.InputWidth = static_cast<UINT>(context->width);
  content_desc.InputHeight = static_cast<UINT>(context->height);
  content_desc.OutputFrameRate = content_desc.InputFrameRate;
  content_desc.OutputWidth = output_width;
  content_desc.OutputHeight = output_height;
  content_desc.Usage = D3D11_VIDEO_USAGE_PLAYBACK_NORMAL;

  nl_release_video_processor_resources(context);
  HRESULT result = context->video_device->CreateVideoProcessorEnumerator(
      &content_desc,
      &context->video_processor_enumerator);
  if (FAILED(result)) return result;

  UINT input_format_flags = 0;
  result = context->video_processor_enumerator->CheckVideoProcessorFormat(
      DXGI_FORMAT_NV12,
      &input_format_flags);
  if (FAILED(result)) return result;
  if ((input_format_flags & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT) == 0) {
    return MF_E_UNSUPPORTED_D3D_TYPE;
  }

  UINT output_format_flags = 0;
  result = context->video_processor_enumerator->CheckVideoProcessorFormat(
      DXGI_FORMAT_R8G8B8A8_UNORM,
      &output_format_flags);
  if (FAILED(result)) return result;
  if ((output_format_flags & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT) == 0) {
    return MF_E_UNSUPPORTED_D3D_TYPE;
  }

  result = context->video_device->CreateVideoProcessor(
      context->video_processor_enumerator.Get(),
      0,
      &context->video_processor);
  if (FAILED(result)) return result;

  ComPtr<ID3D11Texture2D> back_buffer;
  result = context->swap_chain->GetBuffer(0, IID_PPV_ARGS(&back_buffer));
  if (FAILED(result)) return result;
  D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC output_view_desc = {};
  output_view_desc.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
  output_view_desc.Texture2D.MipSlice = 0;
  result = context->video_device->CreateVideoProcessorOutputView(
      back_buffer.Get(),
      context->video_processor_enumerator.Get(),
      &output_view_desc,
      &context->output_view);
  if (FAILED(result)) return result;

  RECT target_rect = {0, 0, static_cast<LONG>(output_width),
                      static_cast<LONG>(output_height)};
  context->video_context->VideoProcessorSetOutputTargetRect(
      context->video_processor.Get(), TRUE, &target_rect);
  D3D11_VIDEO_COLOR background = {};
  background.RGBA.A = 1.0f;
  context->video_context->VideoProcessorSetOutputBackgroundColor(
      context->video_processor.Get(), FALSE, &background);
  return S_OK;
}

static HRESULT nl_create_swap_chain(nl_windows_video_context* context,
                                    HWND hwnd,
                                    UINT width,
                                    UINT height) {
  DXGI_SWAP_CHAIN_DESC1 swap_chain_desc = {};
  swap_chain_desc.Width = width;
  swap_chain_desc.Height = height;
  swap_chain_desc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
  swap_chain_desc.Stereo = FALSE;
  swap_chain_desc.SampleDesc.Count = 1;
  swap_chain_desc.SampleDesc.Quality = 0;
  swap_chain_desc.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
  swap_chain_desc.BufferCount = 5;
  swap_chain_desc.Scaling = DXGI_SCALING_STRETCH;
  swap_chain_desc.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;
  swap_chain_desc.AlphaMode = DXGI_ALPHA_MODE_UNSPECIFIED;

  context->allow_tearing = false;
  ComPtr<IDXGIFactory5> factory_5;
  if (SUCCEEDED(context->factory.As(&factory_5))) {
    BOOL allow_tearing = FALSE;
    if (SUCCEEDED(factory_5->CheckFeatureSupport(
            DXGI_FEATURE_PRESENT_ALLOW_TEARING,
            &allow_tearing,
            sizeof(allow_tearing))) &&
        allow_tearing) {
      swap_chain_desc.Flags |= DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING;
      context->allow_tearing = true;
    }
  }

  HRESULT result = context->factory->CreateSwapChainForHwnd(
      context->device.Get(),
      hwnd,
      &swap_chain_desc,
      nullptr,
      nullptr,
      &context->swap_chain);
  if (FAILED(result)) return result;
  context->factory->MakeWindowAssociation(hwnd, DXGI_MWA_NO_WINDOW_CHANGES);

  context->swap_chain_hwnd = hwnd;
  context->swap_chain_width = width;
  context->swap_chain_height = height;
  context->swap_chain_flags = swap_chain_desc.Flags;
  result = nl_create_video_processor_resources(context, width, height);
  if (FAILED(result)) nl_release_swap_chain(context);
  return result;
}

static HRESULT nl_ensure_swap_chain(nl_windows_video_context* context) {
  HWND hwnd = nl_get_hwnd(context);
  if (hwnd == nullptr || !IsWindow(hwnd)) return S_FALSE;
  RECT client = {};
  if (!GetClientRect(hwnd, &client)) return HRESULT_FROM_WIN32(GetLastError());
  UINT width = static_cast<UINT>(std::max<LONG>(0, client.right - client.left));
  UINT height = static_cast<UINT>(std::max<LONG>(0, client.bottom - client.top));
  if (width == 0 || height == 0) return S_FALSE;

  if (context->swap_chain == nullptr || context->swap_chain_hwnd != hwnd) {
    nl_release_swap_chain(context);
    return nl_create_swap_chain(context, hwnd, width, height);
  }
  if (context->swap_chain_width == width &&
      context->swap_chain_height == height &&
      context->output_view != nullptr) {
    return S_OK;
  }

  nl_release_video_processor_resources(context);
  context->device_context->Flush();
  HRESULT result = context->swap_chain->ResizeBuffers(
      0,
      width,
      height,
      DXGI_FORMAT_UNKNOWN,
      context->swap_chain_flags);
  if (FAILED(result)) return result;
  context->swap_chain_width = width;
  context->swap_chain_height = height;
  return nl_create_video_processor_resources(context, width, height);
}

static HRESULT nl_get_input_view(
    nl_windows_video_context* context,
    ID3D11Texture2D* texture,
    UINT subresource,
    ID3D11VideoProcessorInputView** output) {
  if (output == nullptr) return E_POINTER;
  *output = nullptr;
  for (auto& cached : context->input_views) {
    if (cached.texture.Get() == texture && cached.subresource == subresource) {
      *output = cached.view.Get();
      (*output)->AddRef();
      return S_OK;
    }
  }

  D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC input_view_desc = {};
  input_view_desc.FourCC = 0;
  input_view_desc.ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D;
  input_view_desc.Texture2D.MipSlice = 0;
  input_view_desc.Texture2D.ArraySlice = subresource;

  nl_windows_input_view cached;
  cached.texture = texture;
  cached.subresource = subresource;
  HRESULT result = context->video_device->CreateVideoProcessorInputView(
      texture,
      context->video_processor_enumerator.Get(),
      &input_view_desc,
      &cached.view);
  if (FAILED(result)) return result;
  *output = cached.view.Get();
  (*output)->AddRef();
  context->input_views.push_back(std::move(cached));
  return S_OK;
}

static void nl_set_processor_colorspace(nl_windows_video_context* context,
                                        uint8_t colorspace) {
  if (context->video_context_1 != nullptr) {
    DXGI_COLOR_SPACE_TYPE input_colorspace;
    switch (colorspace) {
      case COLORSPACE_REC_601:
        input_colorspace = DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601;
        break;
      case COLORSPACE_REC_2020:
        input_colorspace = DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P2020;
        break;
      case COLORSPACE_REC_709:
      default:
        input_colorspace = DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709;
        break;
    }
    context->video_context_1->VideoProcessorSetStreamColorSpace1(
        context->video_processor.Get(), 0, input_colorspace);
    context->video_context_1->VideoProcessorSetOutputColorSpace1(
        context->video_processor.Get(),
        DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
    return;
  }

  D3D11_VIDEO_PROCESSOR_COLOR_SPACE input_colorspace = {};
  input_colorspace.Usage = 0;
  input_colorspace.RGB_Range = 0;
  input_colorspace.YCbCr_Matrix = colorspace == COLORSPACE_REC_601 ? 0 : 1;
  input_colorspace.YCbCr_xvYCC = 0;
  input_colorspace.Nominal_Range = 1;
  context->video_context->VideoProcessorSetStreamColorSpace(
      context->video_processor.Get(), 0, &input_colorspace);

  D3D11_VIDEO_PROCESSOR_COLOR_SPACE output_colorspace = {};
  output_colorspace.Usage = 0;
  output_colorspace.RGB_Range = 0;
  output_colorspace.YCbCr_Matrix = 1;
  output_colorspace.YCbCr_xvYCC = 0;
  output_colorspace.Nominal_Range = 2;
  context->video_context->VideoProcessorSetOutputColorSpace(
      context->video_processor.Get(), &output_colorspace);
}

static HRESULT nl_render_sample(nl_windows_video_context* context,
                                IMFSample* sample,
                                uint8_t colorspace) {
  if (sample == nullptr) return E_POINTER;
  ComPtr<IMFMediaBuffer> buffer;
  HRESULT result = sample->GetBufferByIndex(0, &buffer);
  if (FAILED(result)) return result;

  ComPtr<IMFDXGIBuffer> dxgi_buffer;
  result = buffer.As(&dxgi_buffer);
  if (FAILED(result)) return MF_E_UNSUPPORTED_D3D_TYPE;

  ComPtr<ID3D11Texture2D> texture;
  result = dxgi_buffer->GetResource(IID_PPV_ARGS(&texture));
  if (FAILED(result)) return result;
  UINT subresource = 0;
  result = dxgi_buffer->GetSubresourceIndex(&subresource);
  if (FAILED(result)) return result;

  ComPtr<ID3D11Device> sample_device;
  texture->GetDevice(&sample_device);
  if (sample_device.Get() != context->device.Get()) {
    return MF_E_UNSUPPORTED_D3D_TYPE;
  }

  D3D11_TEXTURE2D_DESC texture_desc = {};
  texture->GetDesc(&texture_desc);
  if (texture_desc.Format != DXGI_FORMAT_NV12) {
    return MF_E_INVALIDMEDIATYPE;
  }

  result = nl_ensure_swap_chain(context);
  if (result == S_FALSE) return S_OK;
  if (FAILED(result)) return result;

  ComPtr<ID3D11VideoProcessorInputView> input_view;
  result = nl_get_input_view(context,
                             texture.Get(),
                             subresource,
                             &input_view);
  if (FAILED(result)) return result;

  LONG source_width = static_cast<LONG>(std::min<UINT>(
      static_cast<UINT>(context->width), texture_desc.Width));
  LONG source_height = static_cast<LONG>(std::min<UINT>(
      static_cast<UINT>(context->height), texture_desc.Height));
  RECT source_rect = {0, 0, source_width, source_height};

  double source_aspect = static_cast<double>(source_width) /
                         static_cast<double>(source_height);
  UINT target_width = context->swap_chain_width;
  UINT target_height = context->swap_chain_height;
  UINT destination_width = target_width;
  UINT destination_height = target_height;
  double target_aspect = static_cast<double>(target_width) /
                         static_cast<double>(target_height);
  if (target_aspect > source_aspect) {
    destination_width = static_cast<UINT>(target_height * source_aspect);
  } else {
    destination_height = static_cast<UINT>(target_width / source_aspect);
  }
  LONG destination_x = static_cast<LONG>((target_width - destination_width) / 2U);
  LONG destination_y = static_cast<LONG>((target_height - destination_height) / 2U);
  RECT destination_rect = {
      destination_x,
      destination_y,
      destination_x + static_cast<LONG>(destination_width),
      destination_y + static_cast<LONG>(destination_height),
  };
  RECT target_rect = {0, 0, static_cast<LONG>(target_width),
                      static_cast<LONG>(target_height)};

  context->video_context->VideoProcessorSetOutputTargetRect(
      context->video_processor.Get(), TRUE, &target_rect);
  context->video_context->VideoProcessorSetStreamFrameFormat(
      context->video_processor.Get(),
      0,
      D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
  context->video_context->VideoProcessorSetStreamSourceRect(
      context->video_processor.Get(), 0, TRUE, &source_rect);
  context->video_context->VideoProcessorSetStreamDestRect(
      context->video_processor.Get(), 0, TRUE, &destination_rect);
  context->video_context->VideoProcessorSetStreamAlpha(
      context->video_processor.Get(), 0, TRUE, 1.0f);
  nl_set_processor_colorspace(context, colorspace);

  D3D11_VIDEO_PROCESSOR_STREAM stream = {};
  stream.Enable = TRUE;
  stream.OutputIndex = 0;
  stream.InputFrameOrField = 0;
  stream.PastFrames = 0;
  stream.FutureFrames = 0;
  stream.pInputSurface = input_view.Get();

  result = context->video_context->VideoProcessorBlt(
      context->video_processor.Get(),
      context->output_view.Get(),
      context->output_frame_number++,
      1,
      &stream);
  if (FAILED(result)) return result;

  UINT present_flags = context->allow_tearing ? DXGI_PRESENT_ALLOW_TEARING : 0;
  return context->swap_chain->Present(0, present_flags);
}

static IMFSample* nl_create_software_output_sample(
    nl_windows_video_context* context) {
  ComPtr<IMFSample> sample;
  ComPtr<IMFMediaBuffer> buffer;
  size_t pixel_count = static_cast<size_t>(context->width) *
                       static_cast<size_t>(context->height);
  size_t required_size = pixel_count * 4U;
  DWORD size = context->output_info.cbSize;
  if (required_size > MAXDWORD) return nullptr;
  size = std::max<DWORD>(size, static_cast<DWORD>(required_size));
  if (FAILED(MFCreateSample(&sample))) return nullptr;
  if (FAILED(MFCreateMemoryBuffer(size, &buffer)) ||
      FAILED(sample->AddBuffer(buffer.Get()))) {
    return nullptr;
  }
  return sample.Detach();
}

static uint8_t nl_clamp_channel(int value) {
  return static_cast<uint8_t>(std::max(0, std::min(255, value)));
}

static void nl_yuv_to_bgra(uint8_t y,
                           uint8_t u,
                           uint8_t v,
                           uint8_t colorspace,
                           uint8_t* output) {
  int c = std::max(0, static_cast<int>(y) - 16);
  int d = static_cast<int>(u) - 128;
  int e = static_cast<int>(v) - 128;
  int red_coefficient = 459;
  int green_u_coefficient = 55;
  int green_v_coefficient = 136;
  int blue_coefficient = 541;

  if (colorspace == COLORSPACE_REC_601) {
    red_coefficient = 409;
    green_u_coefficient = 100;
    green_v_coefficient = 208;
    blue_coefficient = 516;
  } else if (colorspace == COLORSPACE_REC_2020) {
    red_coefficient = 430;
    green_u_coefficient = 48;
    green_v_coefficient = 167;
    blue_coefficient = 548;
  }

  output[0] = nl_clamp_channel((298 * c + blue_coefficient * d + 128) >> 8);
  output[1] = nl_clamp_channel(
      (298 * c - green_u_coefficient * d - green_v_coefficient * e + 128) >> 8);
  output[2] = nl_clamp_channel((298 * c + red_coefficient * e + 128) >> 8);
  output[3] = 255;
}

static HRESULT nl_convert_software_nv12(nl_windows_video_context* context,
                                        const uint8_t* data,
                                        LONG stride,
                                        uint8_t colorspace) {
  if (stride <= 0 || stride < context->width) return MF_E_INVALIDMEDIATYPE;
  const uint8_t* y_plane = data;
  const uint8_t* uv_plane = data + static_cast<size_t>(stride) *
                                       static_cast<size_t>(context->height);
  for (int row = 0; row < context->height; ++row) {
    const uint8_t* y_row = y_plane + static_cast<size_t>(row) * stride;
    const uint8_t* uv_row = uv_plane + static_cast<size_t>(row / 2) * stride;
    uint8_t* output = context->bgra.data() +
                      static_cast<size_t>(row) * context->width * 4U;
    for (int column = 0; column < context->width; ++column) {
      nl_yuv_to_bgra(y_row[column],
                     uv_row[(column / 2) * 2],
                     uv_row[(column / 2) * 2 + 1],
                     colorspace,
                     output + column * 4);
    }
  }
  return S_OK;
}

static HRESULT nl_convert_software_yuy2(nl_windows_video_context* context,
                                        const uint8_t* data,
                                        LONG stride,
                                        uint8_t colorspace) {
  if (stride <= 0 || stride < context->width * 2) return MF_E_INVALIDMEDIATYPE;
  for (int row = 0; row < context->height; ++row) {
    const uint8_t* source = data + static_cast<size_t>(row) * stride;
    uint8_t* output = context->bgra.data() +
                      static_cast<size_t>(row) * context->width * 4U;
    for (int column = 0; column < context->width; column += 2) {
      uint8_t y0 = source[column * 2];
      uint8_t u = source[column * 2 + 1];
      uint8_t y1 = source[column * 2 + 2];
      uint8_t v = source[column * 2 + 3];
      nl_yuv_to_bgra(y0, u, v, colorspace, output + column * 4);
      if (column + 1 < context->width) {
        nl_yuv_to_bgra(y1, u, v, colorspace, output + (column + 1) * 4);
      }
    }
  }
  return S_OK;
}

static void nl_present_software_bgra(nl_windows_video_context* context) {
  HWND hwnd = nl_get_hwnd(context);
  RECT client = {};
  if (hwnd == nullptr || !IsWindow(hwnd) || !GetClientRect(hwnd, &client)) return;
  int client_width = client.right - client.left;
  int client_height = client.bottom - client.top;
  if (client_width <= 0 || client_height <= 0) return;

  HDC dc = GetDC(hwnd);
  if (dc == nullptr) return;
  FillRect(dc, &client, static_cast<HBRUSH>(GetStockObject(BLACK_BRUSH)));

  int target_width = client_width;
  int target_height = client_height;
  double source_aspect = static_cast<double>(context->width) /
                         static_cast<double>(context->height);
  double target_aspect = static_cast<double>(client_width) /
                         static_cast<double>(client_height);
  if (target_aspect > source_aspect) {
    target_width = static_cast<int>(client_height * source_aspect);
  } else {
    target_height = static_cast<int>(client_width / source_aspect);
  }
  int target_x = (client_width - target_width) / 2;
  int target_y = (client_height - target_height) / 2;

  BITMAPINFO bitmap = {};
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

static HRESULT nl_render_software_sample(nl_windows_video_context* context,
                                         IMFSample* sample,
                                         uint8_t colorspace) {
  if (sample == nullptr) return E_POINTER;
  ComPtr<IMFMediaBuffer> buffer;
  HRESULT result = sample->ConvertToContiguousBuffer(&buffer);
  if (FAILED(result)) return result;

  ComPtr<IMF2DBuffer> buffer_2d;
  BYTE* data = nullptr;
  LONG stride = 0;
  DWORD maximum = 0;
  DWORD current = 0;
  bool locked_2d = false;
  if (SUCCEEDED(buffer.As(&buffer_2d))) {
    result = buffer_2d->Lock2D(&data, &stride);
    locked_2d = SUCCEEDED(result);
  } else {
    result = buffer->Lock(&data, &maximum, &current);
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
      result = nl_convert_software_nv12(context, data, stride, colorspace);
    } else if (context->output_subtype == MFVideoFormat_YUY2) {
      result = nl_convert_software_yuy2(context, data, stride, colorspace);
    } else if (context->output_subtype == MFVideoFormat_RGB32) {
      LONG absolute_stride = stride < 0 ? -stride : stride;
      if (absolute_stride < context->width * 4) {
        result = MF_E_INVALIDMEDIATYPE;
      } else {
        for (int row = 0; row < context->height; ++row) {
          const uint8_t* source = stride >= 0
                                      ? data + static_cast<size_t>(row) * absolute_stride
                                      : data + static_cast<size_t>(context->height - 1 - row) *
                                                   absolute_stride;
          std::memcpy(context->bgra.data() +
                          static_cast<size_t>(row) * context->width * 4U,
                      source,
                      static_cast<size_t>(context->width) * 4U);
        }
      }
    } else {
      result = MF_E_INVALIDMEDIATYPE;
    }
    if (SUCCEEDED(result)) nl_present_software_bgra(context);
  }

  if (locked_2d) {
    buffer_2d->Unlock2D();
  } else if (data != nullptr) {
    buffer->Unlock();
  }
  return result;
}

static HRESULT nl_drain_decoder(nl_windows_video_context* context) {
  while (context != nullptr && context->decoder != nullptr) {
    MFT_OUTPUT_DATA_BUFFER output = {};
    DWORD status = 0;
    if (context->pipeline_mode == nl_windows_pipeline_mode::software &&
        (context->output_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES) == 0 &&
        (context->output_info.dwFlags & MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES) == 0) {
      output.pSample = nl_create_software_output_sample(context);
      if (output.pSample == nullptr) return E_OUTOFMEMORY;
    }
    HRESULT result = context->decoder->ProcessOutput(0, 1, &output, &status);
    if (result == MF_E_TRANSFORM_NEED_MORE_INPUT) {
      if (output.pSample != nullptr) output.pSample->Release();
      if (output.pEvents != nullptr) output.pEvents->Release();
      return S_OK;
    }
    if (result == MF_E_TRANSFORM_STREAM_CHANGE) {
      if (output.pSample != nullptr) output.pSample->Release();
      if (output.pEvents != nullptr) output.pEvents->Release();
      context->input_views.clear();
      result = context->pipeline_mode == nl_windows_pipeline_mode::gpu
                   ? nl_set_gpu_decoder_output_type(context)
                   : nl_set_software_decoder_output_type(context);
      if (FAILED(result)) return result;
      continue;
    }
    if (FAILED(result)) {
      if (output.pSample != nullptr) output.pSample->Release();
      if (output.pEvents != nullptr) output.pEvents->Release();
      return result;
    }

    uint8_t colorspace = COLORSPACE_REC_709;
    if (!context->pending_colorspaces.empty()) {
      colorspace = context->pending_colorspaces.front();
      context->pending_colorspaces.pop_front();
    }
    if (output.pSample == nullptr) {
      if (output.pEvents != nullptr) output.pEvents->Release();
      return MF_E_UNSUPPORTED_D3D_TYPE;
    }
    result = context->pipeline_mode == nl_windows_pipeline_mode::gpu
                 ? nl_render_sample(context, output.pSample, colorspace)
                 : nl_render_software_sample(context, output.pSample, colorspace);
    output.pSample->Release();
    if (output.pEvents != nullptr) output.pEvents->Release();
    if (FAILED(result)) return result;
  }
  return S_OK;
}

static bool nl_is_device_loss(nl_windows_video_context* context, HRESULT result) {
  if (result == DXGI_ERROR_DEVICE_HUNG ||
      result == DXGI_ERROR_DEVICE_REMOVED ||
      result == DXGI_ERROR_DEVICE_RESET ||
      result == DXGI_ERROR_DRIVER_INTERNAL_ERROR) {
    return true;
  }
  if (context != nullptr && context->device != nullptr) {
    HRESULT removed_reason = context->device->GetDeviceRemovedReason();
    if (FAILED(removed_reason)) return true;
  }
  return false;
}

static HRESULT nl_create_software_fallback_pipeline(
    nl_windows_video_context* context,
    HRESULT gpu_failure) {
  nl_release_decoder(context);
  nl_release_d3d_pipeline(context);
  std::fprintf(stderr,
               "[noland-video] GPU pipeline unavailable (0x%08lx); using software fallback\n",
               static_cast<unsigned long>(gpu_failure));
  HRESULT result = nl_create_software_decoder(context);
  if (FAILED(result)) {
    nl_release_decoder(context);
    std::fprintf(stderr,
                 "[noland-video] software Media Foundation fallback failed: 0x%08lx\n",
                 static_cast<unsigned long>(result));
  }
  return result;
}

static HRESULT nl_recreate_pipeline(nl_windows_video_context* context) {
  nl_release_decoder(context);
  nl_release_d3d_pipeline(context);
  HRESULT result = nl_create_d3d_pipeline(context);
  if (SUCCEEDED(result)) result = nl_create_gpu_decoder(context);
  if (SUCCEEDED(result)) {
    HRESULT output_result = nl_ensure_swap_chain(context);
    if (FAILED(output_result)) result = output_result;
  }
  if (SUCCEEDED(result)) {
    std::fprintf(stderr,
                 "[noland-video] using GPU-native D3D11/Media Foundation pipeline\n");
    return S_OK;
  }
  return nl_create_software_fallback_pipeline(context, result);
}

static DWORD WINAPI nl_windows_frame_thread(LPVOID data) {
  HRESULT com_result = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
  bool com_initialized = SUCCEEDED(com_result);
  nl_video_renderer_t* renderer = static_cast<nl_video_renderer_t*>(data);
  if (FAILED(com_result) && com_result != RPC_E_CHANGED_MODE) {
    std::fprintf(stderr,
                 "[noland-video] frame thread COM initialization failed: 0x%08lx\n",
                 static_cast<unsigned long>(com_result));
    if (renderer != nullptr) {
      nl_windows_video_context* context = nl_windows_context(renderer);
      if (context != nullptr) InterlockedExchange(&context->running, FALSE);
    }
    return static_cast<DWORD>(com_result);
  }
  while (renderer != nullptr) {
    nl_windows_video_context* context = nl_windows_context(renderer);
    VIDEO_FRAME_HANDLE handle = nullptr;
    PDECODE_UNIT decode_unit = nullptr;
    nl_video_frame_metadata_t metadata;
    int result;
    if (context == nullptr ||
        InterlockedCompareExchange(&context->running, TRUE, TRUE) == FALSE) {
      break;
    }
    if (!LiWaitForNextVideoFrame(&handle, &decode_unit)) {
      continue;
    }
    if (InterlockedCompareExchange(&context->running, TRUE, TRUE) == FALSE) {
      if (handle != nullptr) LiCompleteVideoFrame(handle, DR_OK);
      break;
    }
    if (decode_unit == nullptr) {
      if (handle != nullptr) LiCompleteVideoFrame(handle, DR_NEED_IDR);
      continue;
    }

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
  }
  if (com_initialized) CoUninitialize();
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
  (void)video_format;
  (void)width;
  (void)height;
  (void)redraw_rate;
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

  context->video_format = renderer->video_format;
  context->width = renderer->width;
  context->height = renderer->height;
  context->redraw_rate = renderer->redraw_rate;
  if (context->hwnd == nullptr && renderer->surface_attached &&
      renderer->surface.surface_type == NL_SURFACE_WINDOWS_HWND) {
    context->hwnd = static_cast<HWND>(renderer->surface.window_handle);
  }
  if (context->width <= 0 || context->height <= 0 ||
      context->video_format != VIDEO_FORMAT_H264) {
    return static_cast<int>(MF_E_INVALIDMEDIATYPE);
  }

  result = nl_recreate_pipeline(context);
  if (FAILED(result)) {
    std::fprintf(stderr,
                 "[noland-video] GPU and software decoder setup failed: 0x%08lx\n",
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
  LiWakeWaitForVideoFrame();
  if (context->frame_thread != nullptr) {
    WaitForSingleObject(context->frame_thread, INFINITE);
    CloseHandle(context->frame_thread);
    context->frame_thread = nullptr;
  }
  if (context->decoder != nullptr) {
    context->decoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
    nl_drain_decoder(context);
    context->decoder->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
    context->pending_colorspaces.clear();
  }
}

extern "C" void nl_video_renderer_platform_cleanup(nl_video_renderer_t* renderer) {
  nl_windows_video_context* context = nl_windows_context(renderer);
  if (context == nullptr) return;
  nl_video_renderer_platform_stop(renderer);
  nl_release_decoder(context);
  nl_release_d3d_pipeline(context);
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
  ComPtr<IMFSample> sample;
  ComPtr<IMFMediaBuffer> buffer;
  BYTE* destination = nullptr;
  DWORD maximum = 0;
  DWORD current = 0;
  size_t total = 0;
  size_t offset = 0;
  HRESULT result;

  if (context == nullptr || decode_unit == nullptr) return DR_NEED_IDR;
  if (context->decoder == nullptr) {
    result = nl_recreate_pipeline(context);
    if (FAILED(result)) return DR_NEED_IDR;
  }
  if (!context->has_received_input && decode_unit->frameType != FRAME_TYPE_IDR) {
    return DR_NEED_IDR;
  }
  for (entry = decode_unit->bufferList; entry != nullptr; entry = entry->next) {
    if (entry->data != nullptr && entry->length > 0) {
      total += static_cast<size_t>(entry->length);
    }
  }
  if (total == 0 || total > MAXDWORD) return DR_NEED_IDR;

  result = MFCreateSample(&sample);
  if (SUCCEEDED(result)) {
    result = MFCreateMemoryBuffer(static_cast<DWORD>(total), &buffer);
  }
  if (SUCCEEDED(result)) {
    result = buffer->Lock(&destination, &maximum, &current);
  }
  if (SUCCEEDED(result)) {
    for (entry = decode_unit->bufferList; entry != nullptr; entry = entry->next) {
      if (entry->data == nullptr || entry->length <= 0) continue;
      std::memcpy(destination + offset,
                  entry->data,
                  static_cast<size_t>(entry->length));
      offset += static_cast<size_t>(entry->length);
    }
    buffer->Unlock();
    destination = nullptr;
    result = buffer->SetCurrentLength(static_cast<DWORD>(total));
  }
  if (SUCCEEDED(result)) result = sample->AddBuffer(buffer.Get());
  if (SUCCEEDED(result)) {
    result = sample->SetSampleTime(
        static_cast<LONGLONG>(decode_unit->presentationTimeUs) * 10LL);
  }
  if (SUCCEEDED(result) && context->redraw_rate > 0) {
    result = sample->SetSampleDuration(10000000LL / context->redraw_rate);
  }
  if (SUCCEEDED(result) && decode_unit->frameType == FRAME_TYPE_IDR) {
    result = sample->SetUINT32(MFSampleExtension_CleanPoint, TRUE);
  }
  if (SUCCEEDED(result)) {
    result = context->decoder->ProcessInput(0, sample.Get(), 0);
    if (result == MF_E_NOTACCEPTING) {
      result = nl_drain_decoder(context);
      if (SUCCEEDED(result)) {
        result = context->decoder->ProcessInput(0, sample.Get(), 0);
      }
    }
    if (SUCCEEDED(result)) {
      context->has_received_input = true;
      context->pending_colorspaces.push_back(
          frame != nullptr ? frame->colorspace : COLORSPACE_REC_709);
      result = nl_drain_decoder(context);
    }
  }

  if (destination != nullptr) buffer->Unlock();
  if (FAILED(result)) {
    bool was_gpu = context->pipeline_mode == nl_windows_pipeline_mode::gpu;
    bool device_lost = was_gpu && nl_is_device_loss(context, result);
    std::fprintf(stderr,
                 "[noland-video] %s Media Foundation frame failed: 0x%08lx%s\n",
                 was_gpu ? "GPU" : "software",
                 static_cast<unsigned long>(result),
                 device_lost ? " (device lost)" : "");
    if (was_gpu) {
      HRESULT recreate_result = device_lost
                                    ? nl_recreate_pipeline(context)
                                    : nl_create_software_fallback_pipeline(context, result);
      if (FAILED(recreate_result)) {
        std::fprintf(stderr,
                     "[noland-video] decoder recovery failed: 0x%08lx\n",
                     static_cast<unsigned long>(recreate_result));
      }
    }
    return DR_NEED_IDR;
  }
  return DR_OK;
}
