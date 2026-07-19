add_library(noland_moonlight STATIC
  src/noland_moonlight.c
  src/noland_video_renderer.c
)

target_include_directories(noland_moonlight
  PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
  PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/../moonlight-common-c/src
)

target_compile_features(noland_moonlight PRIVATE c_std_11)
set_target_properties(noland_moonlight PROPERTIES POSITION_INDEPENDENT_CODE ON)
target_link_libraries(noland_moonlight PRIVATE moonlight-common-c)

install(TARGETS noland_moonlight ARCHIVE DESTINATION lib LIBRARY DESTINATION lib RUNTIME DESTINATION bin)
install(FILES include/noland_moonlight.h DESTINATION include)

if(BUILD_NOLAND_MOONLIGHT_TESTS)
  enable_testing()
  add_executable(noland_moonlight_smoke_test tests/smoke_test.c)
  target_link_libraries(noland_moonlight_smoke_test PRIVATE noland_moonlight)
  add_test(NAME noland_moonlight_smoke_test COMMAND noland_moonlight_smoke_test)
endif()
