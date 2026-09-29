# windows specific target definitions
set_target_properties(sunshine PROPERTIES LINK_SEARCH_START_STATIC 1)
set(CMAKE_FIND_LIBRARY_SUFFIXES ".dll")

# Look for zlib1.dll in Sunshine install directory or Apollo
find_library(ZLIB ZLIB1
    HINTS
        "C:/Program Files (x86)/Sunshine"
        "C:/Program Files/Apollo"
)
list(APPEND SUNSHINE_EXTERNAL_LIBRARIES
        $<TARGET_OBJECTS:sunshine_rc_object>
        Windowsapp.lib
        Wtsapi32.lib
        avrt.lib
        Mscms.lib
        version.lib)

# Ensure the Windows display helper is built and staged under the Sunshine tools
# directory so the runtime launcher can find it reliably.
if (TARGET sunshine_display_helper)
    # Build helper before sunshine to make the copy step reliable
    add_dependencies(sunshine sunshine_display_helper)

    # Copy helper into the tools directory next to the sunshine executable after build
    add_custom_command(TARGET sunshine POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E make_directory "$<TARGET_FILE_DIR:sunshine>/tools"
        COMMAND ${CMAKE_COMMAND} -E copy_if_different
                $<TARGET_FILE:sunshine_display_helper>
                "$<TARGET_FILE_DIR:sunshine>/tools"
        COMMENT "Copying sunshine_display_helper into tools directory")
endif()

# Enable libdisplaydevice logging in the main Sunshine binary only
target_compile_definitions(sunshine PRIVATE SUNSHINE_USE_DISPLAYDEVICE_LOGGING)

# sunshine.exe imports the PyroWave runtime; keep a copy next to it so the
# build tree runs without an install.
if(SUNSHINE_ENABLE_PYROWAVE)
    add_custom_command(TARGET sunshine POST_BUILD
        COMMAND ${CMAKE_COMMAND} -E copy_if_different "${PYROWAVE_RUNTIME_DLL}" "$<TARGET_FILE_DIR:sunshine>"
        COMMENT "Copying the PyroWave runtime next to sunshine")
endif()

# Build lightweight uninstall UI executable (same UX as installer, no embedded MSI payload)
set(SUNSHINE_UNINSTALL_UI_EXE "${CMAKE_BINARY_DIR}/uninstall.exe")
add_custom_command(
    OUTPUT "${SUNSHINE_UNINSTALL_UI_EXE}"
    COMMAND powershell -NoProfile -ExecutionPolicy Bypass -File "${CMAKE_SOURCE_DIR}/packaging/windows/bootstrapper/build_bootstrapper.ps1" -BuildDir "${CMAKE_BINARY_DIR}" -UninstallOnly -OutputName "uninstall.exe" -DisableSignPath
    COMMAND ${CMAKE_COMMAND} -E copy_if_different "${CMAKE_BINARY_DIR}/cpack_artifacts/uninstall.exe" "${SUNSHINE_UNINSTALL_UI_EXE}"
    DEPENDS "${CMAKE_SOURCE_DIR}/packaging/windows/bootstrapper/build_bootstrapper.ps1"
            "${CMAKE_SOURCE_DIR}/packaging/windows/bootstrapper/VibeshineInstaller.cs"
            "${CMAKE_SOURCE_DIR}/packaging/windows/bootstrapper/app.manifest"
            "${CMAKE_SOURCE_DIR}/LICENSE"
            "${CMAKE_SOURCE_DIR}/apollo.ico"
            "${SUNSHINE_WINDOWS_VERSIONINFO_STAMP}"
            generate_windows_versioninfo
    COMMENT "Building lightweight Vibepollo uninstaller UI"
)
add_custom_target(build_uninstall_ui ALL DEPENDS "${SUNSHINE_UNINSTALL_UI_EXE}")

set(SUNSHINE_WINDOWS_PACKAGED_TARGETS sunshine)
foreach(_packaged_target IN ITEMS
        dxgi-info
        audio-info
        sunshinesvc
        sunshine_wgc_capture
        sunshine_display_helper)
    if(TARGET "${_packaged_target}")
        list(APPEND SUNSHINE_WINDOWS_PACKAGED_TARGETS "${_packaged_target}")
    endif()
endforeach()

# Convenience target to build MSI via CPack (WiX)
add_custom_target(package_msi
    COMMAND "${CMAKE_CPACK_COMMAND}" -G WIX -C "$<IF:$<CONFIG:>,${CMAKE_BUILD_TYPE},$<CONFIG>>"
    DEPENDS ${SUNSHINE_WINDOWS_PACKAGED_TARGETS} build_uninstall_ui web_ui
    COMMENT "Building MSI installer via CPack (WiX)"
)

# This target never signs: it always builds an unsigned installer. Release
# signing is done exclusively through SignPath origin verification in CI
# (.github/workflows/ci-windows.yml, docs/signpath/), which invokes
# build_bootstrapper.ps1 directly with the already-signed MSI. A runner-local
# signature could never satisfy origin verification anyway.
set(SUNSHINE_BOOTSTRAPPER_SIGNPATH_ARGS -DisableSignPath)

# Build custom elevated installer EXE that wraps the generated MSI
add_custom_target(package_installer
    COMMAND powershell -NoProfile -ExecutionPolicy Bypass -File "${CMAKE_SOURCE_DIR}/packaging/windows/bootstrapper/build_bootstrapper.ps1" -BuildDir "${CMAKE_BINARY_DIR}" -MsiPath "${CMAKE_BINARY_DIR}/cpack_artifacts/${PROJECT_NAME}.msi" ${SUNSHINE_BOOTSTRAPPER_SIGNPATH_ARGS}
    DEPENDS package_msi
    COMMENT "Building custom installer executable"
)
