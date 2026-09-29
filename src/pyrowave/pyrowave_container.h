/**
 * @file src/pyrowave/pyrowave_container.h
 * @brief Framing of PyroWave packets into the per-frame "PYRW" container.
 */
#pragma once

#include <cstddef>
#include <cstdint>
#include <span>
#include <vector>

namespace pyrowave {
  /// One packet produced by pyrowave_encoder_packetize(), as a range of its output buffer.
  struct packet_slice_t {
    std::size_t offset;
    std::size_t size;
  };

  /**
   * @brief Size of the container that write_container() produces for these packets.
   */
  std::size_t container_size(std::span<const packet_slice_t> packets);

  /**
   * @brief Frame PyroWave packets into one container.
   * @param bitstream Buffer the packets were written into by packetize.
   * @param packets Packets in transmission order.
   * @param hdr10 Marks the frame as HDR10 (BT.2020 PQ).
   * @param out Receives the container; cleared first.
   * @return `false` (with `out` empty) if the packets cannot be represented:
   *         no packets, more than 65535 packets, an empty packet, a packet
   *         longer than 2^32-1 bytes or one outside of `bitstream`.
   */
  bool write_container(
    std::span<const std::uint8_t> bitstream,
    std::span<const packet_slice_t> packets,
    bool hdr10,
    std::vector<std::uint8_t> &out
  );
}  // namespace pyrowave
