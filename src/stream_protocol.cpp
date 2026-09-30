#include "stream_protocol.h"

#include <algorithm>
#include <stdexcept>

namespace stream {
  std::optional<control_packet_view_t> decode_control_packet(std::string_view packet_bytes) {
    if (packet_bytes.size() < sizeof(std::uint16_t)) {
      return std::nullopt;
    }
    const auto lo = static_cast<std::uint8_t>(packet_bytes[0]);
    const auto hi = static_cast<std::uint8_t>(packet_bytes[1]);
    const auto type = static_cast<std::uint16_t>(lo | (static_cast<std::uint16_t>(hi) << 8));
    return control_packet_view_t {type, packet_bytes.substr(sizeof(type))};
  }

  std::vector<std::uint8_t> concat_and_insert(
    std::uint64_t insert_size,
    std::uint64_t slice_size,
    std::string_view data1,
    std::string_view data2
  ) {
    if (slice_size == 0) {
      return {};
    }
    std::vector<std::uint8_t> result;
    const auto max_size = result.max_size();
    if (data1.size() > max_size || data2.size() > max_size - data1.size()) {
      throw std::length_error("Video packet payload exceeds buffer capacity");
    }
    const auto payload_size = data1.size() + data2.size();
    if (payload_size == 0) {
      return result;
    }
    const auto slices = 1 + (payload_size - 1) / slice_size;
    if (insert_size > (max_size - payload_size) / slices) {
      throw std::length_error("Video packet headers exceed buffer capacity");
    }
    result.reserve(payload_size + slices * insert_size);

    // Copy directly into the packet buffer, including a slice that spans the
    // frame-header/payload boundary. Avoid a second whole-frame allocation.
    for (std::size_t offset = 0; offset < payload_size;) {
      result.insert(result.end(), insert_size, 0);
      const auto count = std::min<std::uint64_t>(slice_size, payload_size - offset);
      const auto from_first = offset < data1.size() ? std::min<std::size_t>(count, data1.size() - offset) : 0;
      if (from_first != 0) {
        result.insert(result.end(), data1.begin() + offset, data1.begin() + offset + from_first);
      }
      if (from_first < count) {
        const auto second_offset = offset + from_first - data1.size();
        result.insert(result.end(), data2.begin() + second_offset, data2.begin() + second_offset + count - from_first);
      }
      offset += count;
    }
    return result;
  }
}  // namespace stream
