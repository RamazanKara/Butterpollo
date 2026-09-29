cmake_minimum_required(VERSION 3.20)

project(sunshine_tools)

set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
if(NOT DEFINED CMAKE_CUDA_STANDARD)
    set(CMAKE_CUDA_STANDARD 17)
    set(CMAKE_CUDA_STANDARD_REQUIRED ON)
endif()

include_directories(
        "${CMAKE_SOURCE_DIR}"
)

set(TOOL_SOURCES
        "${CMAKE_SOURCE_DIR}/src/logging.cpp"
        "${CMAKE_SOURCE_DIR}/src/logging_policy.cpp"
        "${CMAKE_SOURCE_DIR}/src/platform/windows/utf_utils.cpp"
)

add_executable(dxgi-info dxgi.cpp ${TOOL_SOURCES})
target_link_libraries(dxgi-info
        ${Boost_LIBRARIES}
        ${CMAKE_THREAD_LIBS_INIT}
        dxgi
        ${PLATFORM_LIBRARIES}
)
target_compile_options(dxgi-info PRIVATE ${SUNSHINE_COMPILE_OPTIONS})

add_executable(audio-info audio.cpp ${TOOL_SOURCES})
target_link_libraries(audio-info
        ${Boost_LIBRARIES}
        ${CMAKE_THREAD_LIBS_INIT}
        ksuser
        ${PLATFORM_LIBRARIES}
)
target_compile_options(audio-info PRIVATE ${SUNSHINE_COMPILE_OPTIONS})

add_executable(sunshinesvc sunshinesvc.cpp)
target_link_libraries(sunshinesvc
        ${CMAKE_THREAD_LIBS_INIT}
        wtsapi32
        ${PLATFORM_LIBRARIES})
target_compile_options(sunshinesvc PRIVATE ${SUNSHINE_COMPILE_OPTIONS})

add_custom_command(TARGET sunshinesvc POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E make_directory "${CMAKE_BINARY_DIR}/wix_payload/tools"
        COMMAND ${CMAKE_COMMAND} -E copy "$<TARGET_FILE:sunshinesvc>" "${CMAKE_BINARY_DIR}/wix_payload/tools/sunshinesvc.exe"
)

# Add sunshine_wgc_capture executable
add_executable(sunshine_wgc_capture
        sunshine_wgc_capture.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/ipc/pipes.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/ipc/misc_utils.cpp
        ${CMAKE_SOURCE_DIR}/src/logging.cpp
        ${CMAKE_SOURCE_DIR}/src/logging_policy.cpp)
set_target_properties(sunshine_wgc_capture PROPERTIES CXX_STANDARD 23)

# Define helper build flag to conditionally compile logging
target_compile_definitions(sunshine_wgc_capture PRIVATE
        SUNSHINE_EXTERNAL_PROCESS=1
)

target_link_libraries(sunshine_wgc_capture
        ${Boost_LIBRARIES}
        ${CMAKE_THREAD_LIBS_INIT}
        d3d11
        dxgi
        ole32
        user32
        gdi32
        windowsapp
        avrt
        shcore
        wtsapi32
        ${PLATFORM_LIBRARIES})
target_compile_options(sunshine_wgc_capture PRIVATE ${SUNSHINE_COMPILE_OPTIONS})

# Display settings helper: detached helper that applies/reverts display settings via IPC.
add_executable(sunshine_display_helper
        display_settings_helper_v2.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/diagnostics.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/diagnostics_boost.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/file_text_storage.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/topology_policy.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/async_dispatcher.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/golden_health.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/operations.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/snapshot.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/snapshot_codec.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/snapshot_file_storage.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/state_machine.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/win_display_settings.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/win_event_pump.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/win_platform_workarounds.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_shell_refresh_policy.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/display_helper_v2/win_scheduled_task_manager.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/ipc/pipes.cpp
        ${CMAKE_SOURCE_DIR}/src/platform/windows/ipc/misc_utils.cpp
        ${CMAKE_SOURCE_DIR}/src/logging.cpp
        ${CMAKE_SOURCE_DIR}/src/logging_policy.cpp)
set_target_properties(sunshine_display_helper PROPERTIES CXX_STANDARD 23)
target_compile_definitions(sunshine_display_helper PRIVATE SETUP_LIBDISPLAYDEVICE_LOGGING)
target_include_directories(sunshine_display_helper PRIVATE ${CMAKE_SOURCE_DIR}/third-party)
target_link_libraries(sunshine_display_helper
        ${Boost_LIBRARIES}
        ${CMAKE_THREAD_LIBS_INIT}
        libdisplaydevice::display_device
        user32
        wtsapi32
        advapi32
        secur32
        ole32
        taskschd
        ${PLATFORM_LIBRARIES})
target_compile_options(sunshine_display_helper PRIVATE ${SUNSHINE_COMPILE_OPTIONS})
