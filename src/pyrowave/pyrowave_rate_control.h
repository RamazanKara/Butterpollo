/**
 * @file src/pyrowave/pyrowave_rate_control.h
 * @brief Per-frame bitstream budget for PyroWave's intra-only rate control.
 *
 * PyroWave has no temporal prediction and no rate-control state: every frame
 * gets a hard byte budget. The budget follows the negotiated bitrate but must
 * also stay inside what the video transport can protect with FEC, because a
 * PyroWave frame at LAN bitrates is far larger than a typical H.264 frame.
 */
#pragma once

#include "pyrowave_protocol.h"

#include <algorithm>
#include <cstddef>
#include <cstdint>

namespace pyrowave::rate_control {
  /// Smallest budget handed to the encoder, whatever the bitrate.
  constexpr std::size_t MINIMUM_FRAME_BYTES = 4096;

  /// Size of the sequence header PyroWave's packetizer prepends to the first packet.
  constexpr std::size_t SEQUENCE_HEADER_BYTES = 8;

  /**
   * @brief Byte budget of one frame at a given bitrate.
   * @return max(bitrate / 8 / fps, MINIMUM_FRAME_BYTES), rounded down to a multiple of 4.
   */
  constexpr std::size_t frame_budget_bytes(int bitrate_kbps, int framerate) {
    const std::uint64_t fps = framerate > 0 ? static_cast<std::uint64_t>(framerate) : 60;
    const std::uint64_t kbps = bitrate_kbps > 0 ? static_cast<std::uint64_t>(bitrate_kbps) : 0;
    const std::uint64_t bytes = std::max<std::uint64_t>(kbps * 1000 / 8 / fps, MINIMUM_FRAME_BYTES);
    return static_cast<std::size_t>(bytes & ~std::uint64_t {3});
  }

  /**
   * @brief Largest frame payload the video transport can split into FEC blocks.
   * @param payload_bytes_per_shard Frame bytes carried by one video packet.
   * @param fec_percentage Parity shards as a percentage of data shards.
   * @param max_fec_blocks FEC blocks a frame may be split into.
   * @param max_total_shards Data + parity shards per FEC block.
   * @param frame_header_bytes Bytes the transport prepends to the frame payload.
   */
  constexpr std::size_t fec_frame_limit_bytes(
    std::size_t payload_bytes_per_shard,
    int fec_percentage,
    std::size_t max_fec_blocks,
    std::size_t max_total_shards,
    std::size_t frame_header_bytes
  ) {
    const std::size_t fec = fec_percentage > 0 ? static_cast<std::size_t>(fec_percentage) : 0;
    const std::size_t data_shards_per_block = (max_total_shards * 100) / (100 + fec);
    const std::size_t payload = max_fec_blocks * data_shards_per_block * payload_bytes_per_shard;
    return payload > frame_header_bytes ? payload - frame_header_bytes : 0;
  }

  /**
   * @brief Upper bound of what a container adds on top of the encoder's budget.
   * @details The PYRW header, PyroWave's sequence header and one length prefix
   *          per packet. Every packet but the last is filled past
   *          PACKET_BOUNDARY - MAX_BLOCK_BYTES bytes, which bounds the count.
   */
  constexpr std::size_t container_overhead_bytes(std::size_t frame_bytes) {
    const std::size_t packets = frame_bytes / (PACKET_BOUNDARY - MAX_BLOCK_BYTES) + 2;
    return CONTAINER_HEADER_SIZE + SEQUENCE_HEADER_BYTES + packets * CONTAINER_LENGTH_PREFIX_SIZE;
  }

  struct budget_t {
    std::size_t bytes;  ///< pyrowave_rate_control::maximum_bitstream_size
    bool fec_limited;  ///< The transport limit, not the bitrate, set the budget.

    constexpr bool operator==(const budget_t &) const = default;
  };

  /**
   * @brief Encoder budget for one frame.
   * @param max_frame_bytes Largest frame the transport can FEC-protect; 0 when unknown.
   */
  constexpr budget_t encoder_budget(int bitrate_kbps, int framerate, std::size_t max_frame_bytes) {
    budget_t budget {frame_budget_bytes(bitrate_kbps, framerate), false};
    if (max_frame_bytes == 0) {
      return budget;
    }

    const std::size_t overhead = container_overhead_bytes(max_frame_bytes);
    const std::size_t limit = max_frame_bytes > overhead + 4 ? (max_frame_bytes - overhead) & ~std::size_t {3} : 4;
    if (budget.bytes > limit) {
      budget.bytes = limit;
      budget.fec_limited = true;
    }
    return budget;
  }
}  // namespace pyrowave::rate_control
