/**
 * @file src/pyrowave/pyrowave_protocol.h
 * @brief Wire constants shared with the PyroWave-enabled Moonlight clients.
 *
 * PyroWave is not part of moonlight-common-c. These values mirror the public
 * PyroWave client builds (Artemis Android fork, Moonlight-Qt PyroWave build)
 * and must stay byte-for-byte compatible with them.
 */
#pragma once

#include <array>
#include <cstddef>
#include <cstdint>
#include <string_view>

namespace pyrowave {
  /// `x-nv-vqos[0].bitStreamFormat` value (and video::config_t::videoFormat) for PyroWave.
  constexpr int VIDEO_FORMAT_PYROWAVE = 3;

  /// serverinfo ServerCodecModeSupport bits (next to the SCM_* bits in Limelight.h).
  constexpr std::uint32_t SCM_PYROWAVE = 0x00800000;
  constexpr std::uint32_t SCM_PYROWAVE_444 = 0x01000000;

  /// RTSP DESCRIBE attribute announcing PyroWave support.
  constexpr std::string_view DESCRIBE_RTPMAP = "a=rtpmap:99 PYROWAVE/90000";

  /// Every video frame is one container: "PYRW", u8 version, u16 BE packet count,
  /// u8 flags, then packet_count x (u32 BE length, PyroWave packet bytes).
  constexpr std::array<std::uint8_t, 4> CONTAINER_MAGIC {'P', 'Y', 'R', 'W'};
  constexpr std::uint8_t CONTAINER_VERSION = 1;
  constexpr std::size_t CONTAINER_HEADER_SIZE = 8;
  constexpr std::size_t CONTAINER_LENGTH_PREFIX_SIZE = 4;
  constexpr std::size_t CONTAINER_MAX_PACKETS = 0xFFFF;

  /// Container flags. Bit 0 marks HDR10 (BT.2020 PQ, 10-bit values in 16-bit
  /// planes); every other bit must be zero.
  constexpr std::uint8_t CONTAINER_FLAG_HDR10 = 0x01;

  /// Packet boundary handed to pyrowave_encoder_compute_num_packets/packetize.
  /// A single coded block can be up to 16380 bytes, so the boundary must stay
  /// well above that.
  constexpr std::size_t PACKET_BOUNDARY = 0x8000;
  constexpr std::size_t MAX_BLOCK_BYTES = 16380;
}  // namespace pyrowave
