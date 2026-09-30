# Publisher Metadata
set(SUNSHINE_PUBLISHER_NAME "RamazanKara"
        CACHE STRING "The name of the publisher (not developer) of the application.")
set(SUNSHINE_PUBLISHER_WEBSITE "https://github.com/RamazanKara/Butterpollo"
        CACHE STRING "The URL of the publisher's website.")
set(SUNSHINE_PUBLISHER_ISSUE_URL "https://github.com/RamazanKara/Butterpollo/issues"
        CACHE STRING "The URL of the publisher's support site or issue tracker.
        If you provide a modified version of Sunshine, we kindly request that you use your own url.")

option(BUILD_TESTS "Build unit tests." ON)
option(BUILD_WERROR "Enable -Werror flag." OFF)

option(SUNSHINE_ENABLE_TRAY "Enable system tray icon." ON)

option(SUNSHINE_ENABLE_PYROWAVE
        "Build the PyroWave codec for PyroWave-enabled Moonlight clients. Needs the PyroWave C API
        (scripts/build_pyrowave.sh) found through SUNSHINE_PYROWAVE_ROOT and the Vulkan headers." OFF)

option(BOOST_USE_STATIC "Use static boost libraries." ON)

option(CUDA_INHERIT_COMPILE_OPTIONS
        "When building CUDA code, inherit compile options from the the main project. You may want to disable this if
        your IDE throws errors about unknown flags after running cmake." ON)
