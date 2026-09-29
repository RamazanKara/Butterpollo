/**
 * @file src/pyrowave/pyrowave_container.cpp
 * @brief Framing of PyroWave packets into the per-frame "PYRW" container.
 */
#include "pyrowave_container.h"

#include "pyrowave_protocol.h"

#include <algorithm>
#include <cstring>
#include <limits>

namespace pyrowave {
  namespace {
    std::uint8_t *put_be16(std::uint8_t *p, std::uint16_t value) {
      p[0] = static_cast<std::uint8_t>(value >> 8);
      p[1] = static_cast<std::uint8_t>(value);
      return p + 2;
    }

    std::uint8_t *put_be32(std::uint8_t *p, std::uint32_t value) {
      p[0] = static_cast<std::uint8_t>(value >> 24);
      p[1] = static_cast<std::uint8_t>(value >> 16);
      p[2] = static_cast<std::uint8_t>(value >> 8);
      p[3] = static_cast<std::uint8_t>(value);
      return p + 4;
    }

    bool representable(std::span<const std::uint8_t> bitstream, std::span<const packet_slice_t> packets) {
      if (packets.empty() || packets.size() > CONTAINER_MAX_PACKETS) {
        return false;
      }
      return std::ranges::all_of(packets, [&](const packet_slice_t &packet) {
        return packet.size != 0 &&
               packet.size <= std::numeric_limits<std::uint32_t>::max() &&
               packet.offset <= bitstream.size() &&
               packet.size <= bitstream.size() - packet.offset;
      });
    }
  }  // namespace

  std::size_t container_size(std::span<const packet_slice_t> packets) {
    std::size_t size = CONTAINER_HEADER_SIZE;
    for (const auto &packet : packets) {
      size += CONTAINER_LENGTH_PREFIX_SIZE + packet.size;
    }
    return size;
  }

  bool write_container(
    std::span<const std::uint8_t> bitstream,
    std::span<const packet_slice_t> packets,
    bool hdr10,
    std::vector<std::uint8_t> &out
  ) {
    out.clear();
    if (!representable(bitstream, packets)) {
      return false;
    }

    out.resize(container_size(packets));
    auto *p = out.data();
    p = std::copy(CONTAINER_MAGIC.begin(), CONTAINER_MAGIC.end(), p);
    *p++ = CONTAINER_VERSION;
    p = put_be16(p, static_cast<std::uint16_t>(packets.size()));
    *p++ = hdr10 ? CONTAINER_FLAG_HDR10 : 0;

    for (const auto &packet : packets) {
      p = put_be32(p, static_cast<std::uint32_t>(packet.size));
      std::memcpy(p, bitstream.data() + packet.offset, packet.size);
      p += packet.size;
    }
    return true;
  }
}  // namespace pyrowave
