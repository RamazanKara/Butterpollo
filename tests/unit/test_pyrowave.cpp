/**
 * @file tests/unit/test_pyrowave.cpp
 * @brief PyroWave wire framing, per-frame budget and ANNOUNCE gating.
 */
#include "../tests_common.h"

#include "src/pyrowave/pyrowave_container.h"
#include "src/pyrowave/pyrowave_negotiation.h"
#include "src/pyrowave/pyrowave_protocol.h"
#include "src/pyrowave/pyrowave_rate_control.h"

#include <algorithm>
#include <cstdint>
#include <vector>

namespace {
  using pyrowave::packet_slice_t;

  std::vector<std::uint8_t> patterned_bytes(std::size_t size) {
    std::vector<std::uint8_t> bytes(size);
    for (std::size_t i = 0; i < size; ++i) {
      bytes[i] = static_cast<std::uint8_t>(i * 7 + 3);
    }
    return bytes;
  }

  std::uint32_t read_be32(const std::vector<std::uint8_t> &bytes, std::size_t offset) {
    return (std::uint32_t {bytes[offset]} << 24) | (std::uint32_t {bytes[offset + 1]} << 16) |
           (std::uint32_t {bytes[offset + 2]} << 8) | std::uint32_t {bytes[offset + 3]};
  }
}  // namespace

TEST(PyroWaveContainer, SinglePacketHasExactHeaderAndPayload) {
  const std::vector<std::uint8_t> bitstream {0xAA, 0xBB, 0xCC};
  const std::vector<packet_slice_t> packets {{0, 3}};

  std::vector<std::uint8_t> out;
  ASSERT_TRUE(pyrowave::write_container(bitstream, packets, false, out));

  const std::vector<std::uint8_t> expected {
    'P', 'Y', 'R', 'W', 0x01, 0x00, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x03, 0xAA, 0xBB, 0xCC
  };
  EXPECT_EQ(out, expected);
  EXPECT_EQ(pyrowave::container_size(packets), expected.size());
}

TEST(PyroWaveContainer, LengthsAndCountAreBigEndian) {
  const auto bitstream = patterned_bytes(0x10000 + 0x0102 + 16);
  // Non-contiguous slices: the packetizer's offsets must be honoured, not assumed.
  const std::vector<packet_slice_t> packets {
    {8, 0x0102},
    {0x0102 + 12, 0x10000},
  };

  std::vector<std::uint8_t> out;
  ASSERT_TRUE(pyrowave::write_container(bitstream, packets, false, out));
  ASSERT_EQ(out.size(), 8u + 4u + 0x0102u + 4u + 0x10000u);

  EXPECT_EQ(out[5], 0x00);
  EXPECT_EQ(out[6], 0x02);
  EXPECT_EQ(read_be32(out, 8), 0x0102u);
  EXPECT_TRUE(std::equal(out.begin() + 12, out.begin() + 12 + 0x0102, bitstream.begin() + 8));

  const std::size_t second = 12 + 0x0102;
  EXPECT_EQ(read_be32(out, second), 0x10000u);
  EXPECT_TRUE(std::equal(out.begin() + second + 4, out.end(), bitstream.begin() + 0x0102 + 12));
}

TEST(PyroWaveContainer, PacketCountAboveOneByte) {
  const auto bitstream = patterned_bytes(300);
  std::vector<packet_slice_t> packets;
  for (std::size_t i = 0; i < 300; ++i) {
    packets.push_back({i, 1});
  }

  std::vector<std::uint8_t> out;
  ASSERT_TRUE(pyrowave::write_container(bitstream, packets, false, out));
  EXPECT_EQ(out[5], 0x01);
  EXPECT_EQ(out[6], 0x2C);
  EXPECT_EQ(out.size(), 8u + 300u * 5u);
}

TEST(PyroWaveContainer, FlagsMarkOnlyHdr10) {
  const std::vector<std::uint8_t> bitstream {1, 2, 3, 4};
  const std::vector<packet_slice_t> packets {{0, 4}};

  std::vector<std::uint8_t> out;
  ASSERT_TRUE(pyrowave::write_container(bitstream, packets, true, out));
  EXPECT_EQ(out[7], 0x01);

  ASSERT_TRUE(pyrowave::write_container(bitstream, packets, false, out));
  EXPECT_EQ(out[7], 0x00);
}

TEST(PyroWaveContainer, RejectsUnrepresentablePacketLists) {
  const auto bitstream = patterned_bytes(64);
  std::vector<std::uint8_t> out {0xFF};

  EXPECT_FALSE(pyrowave::write_container(bitstream, {}, false, out));
  EXPECT_TRUE(out.empty());

  out = {0xFF};
  const std::vector<packet_slice_t> zero_length {{0, 8}, {8, 0}};
  EXPECT_FALSE(pyrowave::write_container(bitstream, zero_length, false, out));
  EXPECT_TRUE(out.empty());

  const std::vector<packet_slice_t> past_end {{60, 8}};
  EXPECT_FALSE(pyrowave::write_container(bitstream, past_end, false, out));
  const std::vector<packet_slice_t> offset_past_end {{65, 1}};
  EXPECT_FALSE(pyrowave::write_container(bitstream, offset_past_end, false, out));

  std::vector<packet_slice_t> too_many(pyrowave::CONTAINER_MAX_PACKETS + 1, packet_slice_t {0, 1});
  EXPECT_FALSE(pyrowave::write_container(bitstream, too_many, false, out));
  too_many.pop_back();
  EXPECT_TRUE(pyrowave::write_container(bitstream, too_many, false, out));
  EXPECT_EQ(out[5], 0xFF);
  EXPECT_EQ(out[6], 0xFF);
}

TEST(PyroWaveRateControl, FrameBudgetFollowsBitrateAndRoundsDown) {
  using pyrowave::rate_control::frame_budget_bytes;

  // 100 Mbit/s at 60 fps = 208333.3 bytes -> 208332.
  EXPECT_EQ(frame_budget_bytes(100000, 60), 208332u);
  // 500 Mbit/s at 120 fps = 520833.3 bytes -> 520832.
  EXPECT_EQ(frame_budget_bytes(500000, 120), 520832u);
  // Exact multiples stay put: 96 kbit/s / 8 / 1 fps = 12000 bytes.
  EXPECT_EQ(frame_budget_bytes(96, 1), 12000u);
  EXPECT_EQ(frame_budget_bytes(100000, 60) % 4, 0u);
  // Large bitrates must not overflow 32-bit intermediates.
  EXPECT_EQ(frame_budget_bytes(4000000, 60), 8333332u);
}

TEST(PyroWaveRateControl, FrameBudgetHasFloor) {
  using pyrowave::rate_control::frame_budget_bytes;
  using pyrowave::rate_control::MINIMUM_FRAME_BYTES;

  EXPECT_EQ(frame_budget_bytes(1000, 60), MINIMUM_FRAME_BYTES);
  EXPECT_EQ(frame_budget_bytes(0, 60), MINIMUM_FRAME_BYTES);
  EXPECT_EQ(frame_budget_bytes(-5, 60), MINIMUM_FRAME_BYTES);
  // An unknown frame rate is budgeted as 60 fps rather than dividing by zero.
  EXPECT_EQ(frame_budget_bytes(100000, 0), frame_budget_bytes(100000, 60));
}

TEST(PyroWaveRateControl, FecLimitMatchesTransportFraming) {
  using pyrowave::rate_control::fec_frame_limit_bytes;

  // Moonlight's 1392-byte packets carry 1376 frame bytes each; 20% FEC allows
  // 212 data shards per block and 4 blocks per frame, minus the 8-byte frame header.
  EXPECT_EQ(fec_frame_limit_bytes(1376, 20, 4, 255, 8), 4u * 212u * 1376u - 8u);
  EXPECT_EQ(fec_frame_limit_bytes(1376, 0, 4, 255, 8), 4u * 255u * 1376u - 8u);
  EXPECT_EQ(fec_frame_limit_bytes(1376, 255, 4, 255, 8), 4u * 71u * 1376u - 8u);
  EXPECT_EQ(fec_frame_limit_bytes(0, 20, 4, 255, 8), 0u);
}

TEST(PyroWaveRateControl, BudgetIsCappedToTheFecLimit) {
  using pyrowave::rate_control::container_overhead_bytes;
  using pyrowave::rate_control::encoder_budget;
  using pyrowave::rate_control::fec_frame_limit_bytes;

  const auto limit = fec_frame_limit_bytes(1376, 20, 4, 255, 8);

  // 500 Mbit/s at 60 fps fits.
  const auto fits = encoder_budget(500000, 60, limit);
  EXPECT_FALSE(fits.fec_limited);
  EXPECT_EQ(fits.bytes, pyrowave::rate_control::frame_budget_bytes(500000, 60));

  // 1 Gbit/s at 60 fps does not: the budget plus every byte the container can
  // add must still fit the transport.
  const auto capped = encoder_budget(1000000, 60, limit);
  EXPECT_TRUE(capped.fec_limited);
  EXPECT_EQ(capped.bytes % 4, 0u);
  EXPECT_LE(capped.bytes + container_overhead_bytes(capped.bytes), limit);
  EXPECT_GT(capped.bytes, limit - 1024);

  // Unknown transport limit: bitrate only.
  EXPECT_EQ(encoder_budget(1000000, 60, 0), (pyrowave::rate_control::budget_t {pyrowave::rate_control::frame_budget_bytes(1000000, 60), false}));
}

TEST(PyroWaveRateControl, ContainerOverheadBoundsWorstCasePacketization) {
  using pyrowave::rate_control::container_overhead_bytes;

  // Worst case: every packet closes right after the boundary minus one full block.
  const std::size_t bitstream = 1'000'000;
  const std::size_t min_fill = pyrowave::PACKET_BOUNDARY - pyrowave::MAX_BLOCK_BYTES;
  const std::size_t worst_packets = (bitstream + min_fill - 1) / min_fill + 1;
  EXPECT_GE(container_overhead_bytes(bitstream), 8u + 8u + worst_packets * 4u);
}

namespace {
  pyrowave::announce_decision_t negotiate(int format, int chroma, int dynamic_range, const pyrowave::capabilities_t &caps, int width = 1920, int height = 1080) {
    return pyrowave::negotiate_announce(format, width, height, chroma, dynamic_range, caps);
  }
}  // namespace

TEST(PyroWaveNegotiation, ExistingCodecsPassThrough) {
  using pyrowave::announce_status_e;
  const pyrowave::capabilities_t none {};

  for (int format = 0; format <= 2; ++format) {
    // Odd sizes are fine for H.264/HEVC/AV1; their encoders pad.
    const auto decision = negotiate(format, 1, 1, none, 1366, 767);
    EXPECT_EQ(decision.status, announce_status_e::accepted);
    EXPECT_EQ(decision.dynamic_range, 1);
    EXPECT_FALSE(decision.hdr_downgraded);
  }
}

TEST(PyroWaveNegotiation, UnknownFormatsAreRejected) {
  using pyrowave::announce_status_e;
  const pyrowave::capabilities_t all {true, true, true};

  EXPECT_EQ(negotiate(4, 0, 0, all).status, announce_status_e::unknown_video_format);
  EXPECT_EQ(negotiate(-1, 0, 0, all).status, announce_status_e::unknown_video_format);
}

TEST(PyroWaveNegotiation, PyroWaveRequiresAvailability) {
  using pyrowave::announce_status_e;

  EXPECT_EQ(negotiate(3, 0, 0, {}).status, announce_status_e::unavailable);
  const auto sdr = negotiate(3, 0, 0, {true, false, false});
  EXPECT_EQ(sdr.status, announce_status_e::accepted);
  EXPECT_EQ(sdr.dynamic_range, 0);
}

TEST(PyroWaveNegotiation, Yuv444NeedsSupportAndExcludesHdr) {
  using pyrowave::announce_status_e;

  EXPECT_EQ(negotiate(3, 1, 0, {true, false, true}).status, announce_status_e::yuv444_unavailable);
  EXPECT_EQ(negotiate(3, 1, 0, {true, true, false}).status, announce_status_e::accepted);
  EXPECT_EQ(negotiate(3, 1, 1, {true, true, true}).status, announce_status_e::hdr_yuv444);
}

TEST(PyroWaveNegotiation, HdrFallsBackToSdrWhenUnsupported) {
  using pyrowave::announce_status_e;

  const auto downgraded = negotiate(3, 0, 1, {true, true, false});
  EXPECT_EQ(downgraded.status, announce_status_e::accepted);
  EXPECT_EQ(downgraded.dynamic_range, 0);
  EXPECT_TRUE(downgraded.hdr_downgraded);

  const auto hdr = negotiate(3, 0, 1, {true, false, true});
  EXPECT_EQ(hdr.status, announce_status_e::accepted);
  EXPECT_EQ(hdr.dynamic_range, 1);
  EXPECT_FALSE(hdr.hdr_downgraded);
}

TEST(PyroWaveNegotiation, StreamSizeMustBeCodable) {
  using pyrowave::announce_status_e;
  const pyrowave::capabilities_t all {true, true, true};

  // 4:2:0 subsamples by two in both directions.
  EXPECT_EQ(negotiate(3, 0, 0, all, 1921, 1080).status, announce_status_e::unsupported_size);
  EXPECT_EQ(negotiate(3, 0, 0, all, 1920, 1081).status, announce_status_e::unsupported_size);
  EXPECT_EQ(negotiate(3, 1, 0, all, 1921, 1081).status, announce_status_e::accepted);

  EXPECT_EQ(negotiate(3, 0, 0, all, 16384, 16384).status, announce_status_e::accepted);
  EXPECT_EQ(negotiate(3, 0, 0, all, 16386, 1080).status, announce_status_e::unsupported_size);
  EXPECT_EQ(negotiate(3, 0, 0, all, 0, 1080).status, announce_status_e::unsupported_size);
  EXPECT_EQ(negotiate(3, 1, 0, all, 1920, -2).status, announce_status_e::unsupported_size);
}

static_assert(pyrowave::rate_control::frame_budget_bytes(100000, 60) == 208332);
static_assert(pyrowave::CONTAINER_MAGIC[0] == 0x50 && pyrowave::CONTAINER_MAGIC[3] == 0x57);
