/**
 * @file src/display_helper_integration.h
 * @brief Cross-platform wrapper for display helper integration. On Windows, routes to the IPC helper; on other platforms, no-ops.
 */
#pragma once

#include "src/config.h"
#include "src/display_helper_builder.h"
#include "src/rtsp.h"

#include <display_device/types.h>
#include <optional>

  // Bring in the Windows implementation in the correct namespace
  #include "src/platform/windows/display_helper_integration.h"

namespace display_helper_integration {
  // On Windows, we exclusively use the helper and suppress in-process fallback.
  inline bool suppress_fallback() {
    return true;
  }

  // Enumerate display devices as a JSON string suitable for API responses.
  // Implemented in the Windows backend.
  std::string enumerate_devices_json(display_device::DeviceEnumerationDetail detail);
}  // namespace display_helper_integration
