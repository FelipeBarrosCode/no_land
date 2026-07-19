cmake_minimum_required(VERSION 3.20)
project(noland_moonlight_native C)

option(BUILD_NOLAND_MOONLIGHT_HARNESS "Build the standalone native harness" ON)
option(BUILD_NOLAND_MOONLIGHT_TESTS "Build native tests" ON)

if(BUILD_NOLAND_MOONLIGHT_TESTS)
  enable_testing()
endif()

add_subdirectory(noland-moonlight)

if(BUILD_NOLAND_MOONLIGHT_HARNESS)
  add_subdirectory(harness/noland-stream-harness)
endif()
