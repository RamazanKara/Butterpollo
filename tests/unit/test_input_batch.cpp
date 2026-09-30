#include "../tests_common.h"
#include "src/input_batch.h"

#include <boost/endian/conversion.hpp>

namespace {
  template<typename Packet>
  input::batch_result_e batch_packets(Packet &dest, Packet &src) {
    return input::batch(reinterpret_cast<PNV_INPUT_HEADER>(&dest), reinterpret_cast<PNV_INPUT_HEADER>(&src));
  }

  NV_REL_MOUSE_MOVE_PACKET relative_packet(short x, short y) {
    NV_REL_MOUSE_MOVE_PACKET packet {};
    packet.header.magic = boost::endian::native_to_little<std::uint32_t>(MOUSE_MOVE_REL_MAGIC_GEN5);
    packet.deltaX = boost::endian::native_to_big(x);
    packet.deltaY = boost::endian::native_to_big(y);
    return packet;
  }

  NV_SCROLL_PACKET scroll_packet(short amount) {
    NV_SCROLL_PACKET packet {};
    packet.header.magic = boost::endian::native_to_little<std::uint32_t>(SCROLL_MAGIC_GEN5);
    packet.scrollAmt1 = packet.scrollAmt2 = boost::endian::native_to_big(amount);
    return packet;
  }
}

TEST(InputBatch, AddsSafeRelativeMovementWithoutChangingSource) {
  auto dest = relative_packet(120, -400);
  auto src = relative_packet(-20, 100);
  EXPECT_EQ(batch_packets(dest, src), input::batch_result_e::batched);
  EXPECT_EQ(boost::endian::big_to_native(dest.deltaX), 100);
  EXPECT_EQ(boost::endian::big_to_native(dest.deltaY), -300);
  EXPECT_EQ(boost::endian::big_to_native(src.deltaX), -20);
  EXPECT_EQ(boost::endian::big_to_native(src.deltaY), 100);
}

TEST(InputBatch, AllowsExactSignedBounds) {
  auto dest = relative_packet(32766, -32767);
  auto src = relative_packet(1, -1);
  EXPECT_EQ(batch_packets(dest, src), input::batch_result_e::batched);
  EXPECT_EQ(boost::endian::big_to_native(dest.deltaX), 32767);
  EXPECT_EQ(boost::endian::big_to_native(dest.deltaY), -32768);
}

TEST(InputBatch, RejectsPositiveRelativeOverflowWithoutPartialMutation) {
  auto dest = relative_packet(32767, 10);
  auto src = relative_packet(1, 20);
  EXPECT_EQ(batch_packets(dest, src), input::batch_result_e::terminate_batch);
  EXPECT_EQ(boost::endian::big_to_native(dest.deltaX), 32767);
  EXPECT_EQ(boost::endian::big_to_native(dest.deltaY), 10);
}

TEST(InputBatch, RejectsNegativeRelativeOverflowWithoutPartialMutation) {
  auto dest = relative_packet(10, -32768);
  auto src = relative_packet(20, -1);
  EXPECT_EQ(batch_packets(dest, src), input::batch_result_e::terminate_batch);
  EXPECT_EQ(boost::endian::big_to_native(dest.deltaX), 10);
  EXPECT_EQ(boost::endian::big_to_native(dest.deltaY), -32768);
}

TEST(InputBatch, CombinesVerticalScrollAndKeepsRepeatedAmountsEqual) {
  auto dest = scroll_packet(-120);
  auto src = scroll_packet(240);
  EXPECT_EQ(batch_packets(dest, src), input::batch_result_e::batched);
  EXPECT_EQ(boost::endian::big_to_native(dest.scrollAmt1), 120);
  EXPECT_EQ(dest.scrollAmt1, dest.scrollAmt2);
}

TEST(InputBatch, RejectsScrollOverflowInBothDirections) {
  for (auto [amount, delta] : {std::pair<short, short>{32767, 1}, {-32768, -1}}) {
    auto dest = scroll_packet(amount);
    auto src = scroll_packet(delta);
    EXPECT_EQ(batch_packets(dest, src), input::batch_result_e::terminate_batch);
    EXPECT_EQ(boost::endian::big_to_native(dest.scrollAmt1), amount);
    EXPECT_EQ(dest.scrollAmt1, dest.scrollAmt2);
  }
}

TEST(InputBatch, CombinesHorizontalScrollAndRejectsOverflow) {
  SS_HSCROLL_PACKET dest {}, src {};
  dest.header.magic = src.header.magic = boost::endian::native_to_little<std::uint32_t>(SS_HSCROLL_MAGIC);
  dest.scrollAmount = boost::endian::native_to_big<short>(32760);
  src.scrollAmount = boost::endian::native_to_big<short>(7);
  EXPECT_EQ(batch_packets(dest, src), input::batch_result_e::batched);
  EXPECT_EQ(boost::endian::big_to_native(dest.scrollAmount), 32767);
  src.scrollAmount = boost::endian::native_to_big<short>(1);
  EXPECT_EQ(batch_packets(dest, src), input::batch_result_e::terminate_batch);
  EXPECT_EQ(boost::endian::big_to_native(dest.scrollAmount), 32767);
}

TEST(InputBatch, StopsAtPacketTypeBoundary) {
  auto dest = relative_packet(1, 2);
  auto src = scroll_packet(120);
  EXPECT_EQ(input::batch(reinterpret_cast<PNV_INPUT_HEADER>(&dest), reinterpret_cast<PNV_INPUT_HEADER>(&src)),
    input::batch_result_e::terminate_batch);
  EXPECT_EQ(boost::endian::big_to_native(dest.deltaX), 1);
  EXPECT_EQ(boost::endian::big_to_native(dest.deltaY), 2);
}
