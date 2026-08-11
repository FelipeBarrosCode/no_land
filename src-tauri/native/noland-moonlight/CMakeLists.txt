set(NOLAND_NATIVE_PREFIX "" CACHE PATH "Prefix containing native SDL2/Opus dependencies")

set(NOLAND_NATIVE_INCLUDE_HINTS)
set(NOLAND_NATIVE_LIB_HINTS)
if(NOLAND_NATIVE_PREFIX)
  list(APPEND NOLAND_NATIVE_INCLUDE_HINTS "${NOLAND_NATIVE_PREFIX}/include")
  list(APPEND NOLAND_NATIVE_LIB_HINTS "${NOLAND_NATIVE_PREFIX}/lib")
endif()

add_library(noland_moonlight STATIC
  src/noland_moonlight.c
  src/noland_controller_manager.c
  src/noland_video_renderer.c
)

find_path(SDL2_INCLUDE_ROOT SDL2/SDL.h HINTS ${NOLAND_NATIVE_INCLUDE_HINTS} REQUIRED)
find_library(SDL2_LIBRARY NAMES SDL2 SDL2-static HINTS ${NOLAND_NATIVE_LIB_HINTS} REQUIRED)
target_include_directories(noland_moonlight PRIVATE ${SDL2_INCLUDE_ROOT} ${SDL2_INCLUDE_ROOT}/SDL2)
target_link_libraries(noland_moonlight PRIVATE ${SDL2_LIBRARY})

if(APPLE)
  target_sources(noland_moonlight PRIVATE src/noland_video_renderer_macos.m src/noland_audio_renderer_macos.m)
elseif(UNIX)
  target_sources(noland_moonlight PRIVATE
    src/noland_audio_renderer_sdl.c
    src/noland_desktop_input_sdl.c
    src/noland_video_renderer_linux.c
  )
  find_package(PkgConfig REQUIRED)
  pkg_check_modules(GSTREAMER REQUIRED gstreamer-1.0 gstreamer-app-1.0 gstreamer-video-1.0)
  target_include_directories(noland_moonlight PRIVATE ${GSTREAMER_INCLUDE_DIRS})
  target_link_directories(noland_moonlight PRIVATE ${GSTREAMER_LIBRARY_DIRS})
  target_link_libraries(noland_moonlight PRIVATE ${GSTREAMER_LIBRARIES})
elseif(WIN32)
  target_sources(noland_moonlight PRIVATE
    src/noland_audio_renderer_sdl.c
    src/noland_desktop_input_sdl.c
    src/noland_video_renderer_windows.cpp
  )
  target_compile_features(noland_moonlight PRIVATE cxx_std_17)
  target_link_libraries(noland_moonlight PRIVATE d3d11 dxgi mf mfplat mfuuid ole32 user32 gdi32)
endif()

find_path(OPUS_INCLUDE_DIR opus/opus_multistream.h HINTS ${NOLAND_NATIVE_INCLUDE_HINTS})
find_library(OPUS_LIBRARY opus HINTS ${NOLAND_NATIVE_LIB_HINTS})

if(WIN32)
  if(OPUS_INCLUDE_DIR AND OPUS_LIBRARY)
    set(OPUS_LIBRARY_TARGET ${OPUS_LIBRARY})
  else()
    include(FetchContent)
    set(OPUS_BUILD_SHARED_LIBRARY OFF CACHE BOOL "" FORCE)
    set(OPUS_BUILD_TESTING OFF CACHE BOOL "" FORCE)
    set(OPUS_BUILD_PROGRAMS OFF CACHE BOOL "" FORCE)
    set(OPUS_STACK_PROTECTOR OFF CACHE BOOL "" FORCE)
    set(OPUS_DISABLE_INTRINSICS ON CACHE BOOL "" FORCE)
    FetchContent_Declare(
      opus_dep
      URL https://downloads.xiph.org/releases/opus/opus-1.5.2.tar.gz
      DOWNLOAD_EXTRACT_TIMESTAMP true
    )
    FetchContent_MakeAvailable(opus_dep)
    set(OPUS_INCLUDE_DIR "${CMAKE_CURRENT_BINARY_DIR}/generated-opus-include")
    file(MAKE_DIRECTORY "${OPUS_INCLUDE_DIR}/opus")
    file(COPY "${opus_dep_SOURCE_DIR}/include/" DESTINATION "${OPUS_INCLUDE_DIR}/opus")
    set(OPUS_LIBRARY_TARGET opus)
  endif()
else()
  if(NOT OPUS_INCLUDE_DIR)
    find_path(OPUS_INCLUDE_DIR opus/opus_multistream.h HINTS ${NOLAND_NATIVE_INCLUDE_HINTS} REQUIRED)
  endif()
  if(NOT OPUS_LIBRARY)
    find_library(OPUS_LIBRARY opus HINTS ${NOLAND_NATIVE_LIB_HINTS} REQUIRED)
  endif()
endif()

target_include_directories(noland_moonlight
  PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
  PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/../moonlight-common-c/src
    ${OPUS_INCLUDE_DIR}
)

target_compile_features(noland_moonlight PRIVATE c_std_11)
set_target_properties(noland_moonlight PROPERTIES POSITION_INDEPENDENT_CODE ON)
if(WIN32)
  target_link_libraries(noland_moonlight PRIVATE moonlight-common-c ${OPUS_LIBRARY_TARGET})
else()
  target_link_libraries(noland_moonlight PRIVATE moonlight-common-c ${OPUS_LIBRARY})
endif()

if(APPLE)
  find_library(APPKIT_FRAMEWORK AppKit REQUIRED)
  find_library(AVFOUNDATION_FRAMEWORK AVFoundation REQUIRED)
  find_library(COREMEDIA_FRAMEWORK CoreMedia REQUIRED)
  find_library(COREVIDEO_FRAMEWORK CoreVideo REQUIRED)
  find_library(QUARTZCORE_FRAMEWORK QuartzCore REQUIRED)
  target_link_libraries(noland_moonlight PRIVATE
    ${APPKIT_FRAMEWORK}
    ${AVFOUNDATION_FRAMEWORK}
    ${COREMEDIA_FRAMEWORK}
    ${COREVIDEO_FRAMEWORK}
    ${QUARTZCORE_FRAMEWORK}
  )
  set_source_files_properties(
    src/noland_video_renderer_macos.m
    src/noland_audio_renderer_macos.m
    PROPERTIES COMPILE_FLAGS "-fobjc-arc"
  )
endif()

install(TARGETS noland_moonlight ARCHIVE DESTINATION lib LIBRARY DESTINATION lib RUNTIME DESTINATION bin)
install(FILES include/noland_moonlight.h DESTINATION include)

if(BUILD_NOLAND_MOONLIGHT_TESTS)
  enable_testing()
  add_executable(noland_moonlight_smoke_test tests/smoke_test.c)
  target_link_libraries(noland_moonlight_smoke_test PRIVATE noland_moonlight)
  add_test(NAME noland_moonlight_smoke_test COMMAND noland_moonlight_smoke_test)
endif()
