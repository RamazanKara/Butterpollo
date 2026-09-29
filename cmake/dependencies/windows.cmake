# windows specific dependencies

# MinHook setup - use installed minhook for AMD64, otherwise download minhook-detours for ARM64
if(CMAKE_SYSTEM_PROCESSOR MATCHES "AMD64")
    # Make sure MinHook is installed for x86/x64
    find_library(MINHOOK_LIBRARY libMinHook.a REQUIRED)
    find_path(MINHOOK_INCLUDE_DIR MinHook.h PATH_SUFFIXES include REQUIRED)

    add_library(minhook::minhook STATIC IMPORTED)
    set_property(TARGET minhook::minhook PROPERTY IMPORTED_LOCATION ${MINHOOK_LIBRARY})
    target_include_directories(minhook::minhook INTERFACE ${MINHOOK_INCLUDE_DIR})
else()
    # Download pre-built minhook-detours for ARM64
    message(STATUS "Downloading minhook-detours pre-built binaries for ARM64")
    include(FetchContent)

    FetchContent_Declare(
        minhook-detours
        URL      https://github.com/m417z/minhook-detours/releases/download/v1.0.6/minhook-detours-1.0.6.zip
        URL_HASH SHA256=E719959D824511E27395A82AEDA994CAAD53A67EE5894BA5FC2F4BF1FA41E38E
    )
    FetchContent_MakeAvailable(minhook-detours)

    # Create imported library for the pre-built DLL
    set(_MINHOOK_DLL
        "${minhook-detours_SOURCE_DIR}/Release/minhook-detours.ARM64.Release.dll"
        CACHE INTERNAL "Path to minhook-detours DLL")
    add_library(minhook::minhook SHARED IMPORTED GLOBAL)
    set_property(TARGET minhook::minhook PROPERTY IMPORTED_LOCATION "${_MINHOOK_DLL}")
    set_property(TARGET minhook::minhook PROPERTY IMPORTED_IMPLIB
        "${minhook-detours_SOURCE_DIR}/Release/minhook-detours.ARM64.Release.lib")
    set_target_properties(minhook::minhook PROPERTIES
        INTERFACE_INCLUDE_DIRECTORIES "${minhook-detours_SOURCE_DIR}/src"
    )
endif()

# PyroWave C API (libpyrowave-shared-0.dll, built by scripts/build_pyrowave.sh).
# The DLL loads the Vulkan loader itself at runtime, so the build only needs
# the Vulkan headers (>= 1.4 for VkQueueGlobalPriority in pyrowave.h).
if(SUNSHINE_ENABLE_PYROWAVE)
    set(SUNSHINE_PYROWAVE_ROOT "" CACHE PATH "Install prefix of the PyroWave C API")
    find_package(pyrowave-shared CONFIG REQUIRED HINTS "${SUNSHINE_PYROWAVE_ROOT}")
    find_package(VulkanHeaders 1.4 CONFIG REQUIRED)

    # <prefix>/share/pyrowave-shared/cmake -> <prefix>
    get_filename_component(PYROWAVE_PREFIX "${pyrowave-shared_DIR}/../../.." ABSOLUTE)
    set(PYROWAVE_INCLUDE_DIR "${PYROWAVE_PREFIX}/include/pyrowave")
    if(NOT EXISTS "${PYROWAVE_INCLUDE_DIR}/pyrowave.h")
        message(FATAL_ERROR "pyrowave.h not found in ${PYROWAVE_INCLUDE_DIR}")
    endif()
    # The exported package sets no include directories.
    target_include_directories(pyrowave-shared INTERFACE "${PYROWAVE_INCLUDE_DIR}")
    target_link_libraries(pyrowave-shared INTERFACE Vulkan::Headers)

    get_target_property(PYROWAVE_RUNTIME_DLL pyrowave-shared IMPORTED_LOCATION_RELEASE)
    if(NOT PYROWAVE_RUNTIME_DLL)
        get_target_property(PYROWAVE_RUNTIME_DLL pyrowave-shared IMPORTED_LOCATION)
    endif()
    if(NOT PYROWAVE_RUNTIME_DLL OR NOT EXISTS "${PYROWAVE_RUNTIME_DLL}")
        message(FATAL_ERROR "PyroWave runtime DLL not found (pyrowave-shared IMPORTED_LOCATION)")
    endif()

    # MIT notices of everything linked into the DLL (PyroWave, Granite, volk).
    set(PYROWAVE_LICENSE_DIR "${PYROWAVE_PREFIX}/share/licenses/pyrowave")
    foreach(_pyrowave_license IN ITEMS LICENSE LICENSE.Granite LICENSE.volk.md)
        if(NOT EXISTS "${PYROWAVE_LICENSE_DIR}/${_pyrowave_license}")
            message(FATAL_ERROR "PyroWave license file missing: ${PYROWAVE_LICENSE_DIR}/${_pyrowave_license}")
        endif()
    endforeach()
    unset(_pyrowave_license)

    message(STATUS "PyroWave: ${PYROWAVE_PREFIX} (runtime ${PYROWAVE_RUNTIME_DLL})")
    list(APPEND SUNSHINE_DEFINITIONS SUNSHINE_ENABLE_PYROWAVE=1)
    list(APPEND SUNSHINE_EXTERNAL_LIBRARIES pyrowave-shared)
endif()
