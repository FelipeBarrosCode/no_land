add_library(noland_moonlight STATIC
  src/noland_moonlight.c
  src/noland_video_renderer.c
)

if(APPLE)
  target_sources(noland_moonlight PRIVATE src/noland_video_renderer_macos.m src/noland_audio_renderer_macos.m)
elseif(UNIX)
  target_sources(noland_moonlight PRIVATE src/noland_audio_renderer_linux.c)
  find_package(PkgConfig REQUIRED)
  pkg_check_modules(LIBPULSE_SIMPLE REQUIRED libpulse-simple)
  target_include_directories(noland_moonlight PRIVATE ${LIBPULSE_SIMPLE_INCLUDE_DIRS})
  target_link_libraries(noland_moonlight PRIVATE ${LIBPULSE_SIMPLE_LIBRARIES})
elseif(WIN32)
  target_sources(noland_moonlight PRIVATE src/noland_audio_renderer_windows.c)
  target_link_libraries(noland_moonlight PRIVATE winmm)
endif()

find_path(OPUS_INCLUDE_DIR opus/opus_multistream.h HINTS /opt/homebrew/include /usr/local/include REQUIRED)
find_library(OPUS_LIBRARY opus HINTS /opt/homebrew/lib /usr/local/lib REQUIRED)

target_include_directories(noland_moonlight
  PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
  PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/../moonlight-common-c/src
    ${OPUS_INCLUDE_DIR}
)

target_compile_features(noland_moonlight PRIVATE c_std_11)
set_target_properties(noland_moonlight PROPERTIES POSITION_INDEPENDENT_CODE ON)
target_link_libraries(noland_moonlight PRIVATE moonlight-common-c ${OPUS_LIBRARY})

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
