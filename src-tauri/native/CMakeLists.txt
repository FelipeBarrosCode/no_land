cmake_minimum_required(VERSION 3.20)
project(noland_moonlight_native C)

if(APPLE)
  enable_language(OBJC)
endif()
if(WIN32)
  enable_language(CXX)
endif()

option(BUILD_NOLAND_MOONLIGHT_HARNESS "Build the standalone native harness" ON)
option(BUILD_NOLAND_MOONLIGHT_TESTS "Build native tests" ON)

if(BUILD_NOLAND_MOONLIGHT_TESTS)
  enable_testing()
endif()

set(BUILD_SHARED_LIBS OFF CACHE BOOL "Build moonlight-common-c as a static library" FORCE)
add_subdirectory(moonlight-common-c)
add_subdirectory(noland-moonlight)

if(BUILD_NOLAND_MOONLIGHT_HARNESS)
  add_subdirectory(harness/noland-stream-harness)
endif()
