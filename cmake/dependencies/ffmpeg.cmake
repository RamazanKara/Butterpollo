#
# Loads FFmpeg pre-compiled binaries from GitHub releases or a user-specified path
#
include_guard(GLOBAL)

# ffmpeg pre-compiled binaries
if(NOT DEFINED FFMPEG_PREPARED_BINARIES)
    # Set platform-specific libraries
    set(FFMPEG_PLATFORM_LIBRARIES mfplat ole32 strmiids mfuuid vpl)

    # Pinned LizardByte/build-deps release. Its static FFmpeg also provides the
    # AMF SDK headers (include/AMF). Update the tag and hash together.
    set(FFMPEG_GITHUB_REPO "LizardByte/build-deps")
    set(FFMPEG_RELEASE_TAG "v2026.516.30821")
    set(FFMPEG_ARCHIVE_NAME "Windows-AMD64-ffmpeg.tar.gz")
    set(FFMPEG_ARCHIVE_SHA256 "2f7a2c2fc6be9b96de3c6f654389f73a5e5d369d7e802d017894fae96247661d")
    set(FFMPEG_DOWNLOAD_URL
        "https://github.com/${FFMPEG_GITHUB_REPO}/releases/download/${FFMPEG_RELEASE_TAG}/${FFMPEG_ARCHIVE_NAME}")

    # Extract per tag so bumping the pin cannot reuse an older extraction.
    set(FFMPEG_VERSION_DIR "${CMAKE_BINARY_DIR}/_deps/ffmpeg-${FFMPEG_RELEASE_TAG}")
    set(FFMPEG_ARCHIVE_PATH "${FFMPEG_VERSION_DIR}/${FFMPEG_ARCHIVE_NAME}")
    set(FFMPEG_PREPARED_BINARIES "${FFMPEG_VERSION_DIR}/ffmpeg")

    if(NOT EXISTS "${FFMPEG_PREPARED_BINARIES}/lib/libavcodec.a")
        if(NOT EXISTS "${FFMPEG_ARCHIVE_PATH}")
            message(STATUS "Downloading FFmpeg ${FFMPEG_RELEASE_TAG} from ${FFMPEG_DOWNLOAD_URL}")
            file(DOWNLOAD
                "${FFMPEG_DOWNLOAD_URL}"
                "${FFMPEG_ARCHIVE_PATH}"
                EXPECTED_HASH SHA256=${FFMPEG_ARCHIVE_SHA256}
                STATUS FFMPEG_DOWNLOAD_STATUS
                TIMEOUT 300
            )
            list(GET FFMPEG_DOWNLOAD_STATUS 0 FFMPEG_DOWNLOAD_STATUS_CODE)
            list(GET FFMPEG_DOWNLOAD_STATUS 1 FFMPEG_DOWNLOAD_STATUS_MESSAGE)
            if(NOT FFMPEG_DOWNLOAD_STATUS_CODE EQUAL 0)
                file(REMOVE "${FFMPEG_ARCHIVE_PATH}")
                message(FATAL_ERROR "Failed to download FFmpeg binaries: ${FFMPEG_DOWNLOAD_STATUS_MESSAGE}")
            endif()
        else()
            message(STATUS "Using cached FFmpeg archive at ${FFMPEG_ARCHIVE_PATH}")
        endif()

        message(STATUS "Extracting FFmpeg binaries to ${FFMPEG_VERSION_DIR}")
        file(ARCHIVE_EXTRACT  # cmake-lint: disable=E1126
            INPUT "${FFMPEG_ARCHIVE_PATH}"
            DESTINATION "${FFMPEG_VERSION_DIR}"
        )
        if(NOT EXISTS "${FFMPEG_PREPARED_BINARIES}/lib/libavcodec.a")
            message(FATAL_ERROR "FFmpeg extraction failed or unexpected directory structure")
        endif()
    else()
        message(STATUS "Using FFmpeg ${FFMPEG_RELEASE_TAG} at ${FFMPEG_PREPARED_BINARIES}")
    endif()

    # Set FFmpeg libraries
    if(EXISTS "${FFMPEG_PREPARED_BINARIES}/lib/libhdr10plus.a")
        set(HDR10_PLUS_LIBRARY "${FFMPEG_PREPARED_BINARIES}/lib/libhdr10plus.a")
    endif()

    set(FFMPEG_LIBRARIES
        "${FFMPEG_PREPARED_BINARIES}/lib/libavcodec.a"
        "${FFMPEG_PREPARED_BINARIES}/lib/libswscale.a"
        "${FFMPEG_PREPARED_BINARIES}/lib/libavutil.a"
        "${FFMPEG_PREPARED_BINARIES}/lib/libcbs.a"
        "${FFMPEG_PREPARED_BINARIES}/lib/libSvtAv1Enc.a"
        "${FFMPEG_PREPARED_BINARIES}/lib/libx264.a"
        "${FFMPEG_PREPARED_BINARIES}/lib/libx265.a"
        ${HDR10_PLUS_LIBRARY}
        ${FFMPEG_PLATFORM_LIBRARIES}
    )
else()
    # User provided FFMPEG_PREPARED_BINARIES path
    message(STATUS "Using user-specified FFmpeg binaries at ${FFMPEG_PREPARED_BINARIES}")

    # Set platform-specific libraries
    if(NOT DEFINED FFMPEG_PLATFORM_LIBRARIES)
        set(FFMPEG_PLATFORM_LIBRARIES mfplat ole32 strmiids mfuuid vpl)
    endif()

    # Set base FFmpeg libraries (always required)
    set(FFMPEG_LIBRARIES
        "${FFMPEG_PREPARED_BINARIES}/lib/libavcodec.a"
        "${FFMPEG_PREPARED_BINARIES}/lib/libswscale.a"
        "${FFMPEG_PREPARED_BINARIES}/lib/libavutil.a"
        "${FFMPEG_PREPARED_BINARIES}/lib/libcbs.a"
    )

    # Add optional libraries if they exist (e.g., from prebuilt packages)
    if(EXISTS "${FFMPEG_PREPARED_BINARIES}/lib/libSvtAv1Enc.a")
        list(APPEND FFMPEG_LIBRARIES "${FFMPEG_PREPARED_BINARIES}/lib/libSvtAv1Enc.a")
    endif()
    if(EXISTS "${FFMPEG_PREPARED_BINARIES}/lib/libx264.a")
        list(APPEND FFMPEG_LIBRARIES "${FFMPEG_PREPARED_BINARIES}/lib/libx264.a")
    endif()
    if(EXISTS "${FFMPEG_PREPARED_BINARIES}/lib/libx265.a")
        list(APPEND FFMPEG_LIBRARIES "${FFMPEG_PREPARED_BINARIES}/lib/libx265.a")
    endif()
    if(EXISTS "${FFMPEG_PREPARED_BINARIES}/lib/libhdr10plus.a")
        list(APPEND FFMPEG_LIBRARIES "${FFMPEG_PREPARED_BINARIES}/lib/libhdr10plus.a")
    endif()

    # Add platform libraries
    list(APPEND FFMPEG_LIBRARIES ${FFMPEG_PLATFORM_LIBRARIES})
endif()

if(WIN32)
    # Static libavutil uses BCrypt for av_random_bytes(). Keep its system
    # dependency with FFmpeg so focused consumers do not rely on the host's
    # larger Windows link list, including with user-provided FFmpeg binaries.
    list(APPEND FFMPEG_LIBRARIES bcrypt)
endif()

set(FFMPEG_INCLUDE_DIRS "${FFMPEG_PREPARED_BINARIES}/include")
