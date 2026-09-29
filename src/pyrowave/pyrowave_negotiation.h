/**
 * @file src/pyrowave/pyrowave_negotiation.h
 * @brief RTSP ANNOUNCE gating for the negotiated video format.
 */
#pragma once

#include "pyrowave_protocol.h"

namespace pyrowave {
  /// What the host can encode as PyroWave (all false when PyroWave is off or failed its probe).
  struct capabilities_t {
    bool available = false;
    bool yuv444 = false;
    bool hdr = false;
  };

  enum class announce_status_e {
    accepted,
    unknown_video_format,  ///< bitStreamFormat outside H.264/HEVC/AV1/PyroWave.
    unavailable,  ///< PyroWave requested while it is disabled or unsupported.
    yuv444_unavailable,  ///< PyroWave 4:4:4 requested but not supported.
    hdr_yuv444,  ///< PyroWave HDR combined with 4:4:4, which PyroWave clients never request.
  };

  struct announce_decision_t {
    announce_status_e status;
    int dynamic_range;  ///< dynamicRange to encode with.
    bool hdr_downgraded;  ///< PyroWave HDR requested but unsupported; the stream is SDR.
  };

  /**
   * @brief Validate the client's format request against the host's PyroWave support.
   * @details Unknown formats are rejected instead of silently falling back to
   *          H.264, which a PyroWave client would fail to decode. H.264, HEVC and
   *          AV1 requests pass through unchanged.
   */
  constexpr announce_decision_t negotiate_announce(
    int video_format,
    int chroma_sampling_type,
    int dynamic_range,
    const capabilities_t &caps
  ) {
    if (video_format < 0 || video_format > VIDEO_FORMAT_PYROWAVE) {
      return {announce_status_e::unknown_video_format, dynamic_range, false};
    }
    if (video_format != VIDEO_FORMAT_PYROWAVE) {
      return {announce_status_e::accepted, dynamic_range, false};
    }

    if (!caps.available) {
      return {announce_status_e::unavailable, dynamic_range, false};
    }

    const bool yuv444 = chroma_sampling_type == 1;
    const bool hdr = dynamic_range > 0;
    if (yuv444 && hdr) {
      return {announce_status_e::hdr_yuv444, dynamic_range, false};
    }
    if (yuv444 && !caps.yuv444) {
      return {announce_status_e::yuv444_unavailable, dynamic_range, false};
    }
    if (hdr && !caps.hdr) {
      // Every container carries its own HDR flag, so an SDR stream is still
      // decodable by a client that asked for HDR.
      return {announce_status_e::accepted, 0, true};
    }
    return {announce_status_e::accepted, dynamic_range, false};
  }
}  // namespace pyrowave
