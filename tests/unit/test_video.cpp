#include "../tests_common.h"

#include "src/video_policy.h"
#include "src/video_latency_trace.h"
#include "src/thread_safe.h"

#include <array>
#include <deque>
#include <map>
#include <vector>

namespace {
  class FakeEncoderProvider: public video::policy::encoder_capability_provider_t {
  public:
    std::map<std::string, video::policy::encoder_capabilities_t> values;
    video::policy::encoder_capabilities_t capabilities(std::string_view encoder) const override {
      const auto found = values.find(std::string(encoder));
      return found == values.end() ? video::policy::encoder_capabilities_t {} : found->second;
    }
  };
}

TEST(VideoControlPolicy, BusyProducerDefersUpdateWithoutSpinning) {
  struct mailbox_t {
    bool busy = true;
    int polls = 0;
    std::optional<int> pending = 12000;

    bool peek() = delete;  // Readiness must not drive a retry after a failed pop.
    std::optional<int> pop(std::chrono::milliseconds delay) {
      EXPECT_EQ(delay, std::chrono::milliseconds::zero());
      ++polls;
      return busy ? std::nullopt : std::exchange(pending, std::nullopt);
    }
  } mailbox;
  std::optional<int> latest;
  auto consume = [&](int bitrate) { latest = bitrate; };
  video::policy::drain_ready_control_events(mailbox, consume);
  EXPECT_EQ(mailbox.polls, 1);
  EXPECT_FALSE(latest);
  EXPECT_EQ(mailbox.pending, 12000);

  mailbox.busy = false;
  video::policy::drain_ready_control_events(mailbox, consume);
  EXPECT_EQ(mailbox.polls, 3);
  EXPECT_EQ(latest, 12000);
  EXPECT_FALSE(mailbox.pending);
}

TEST(VideoControlPolicy, PartialDrainKeepsNewestConsumedUpdateAndDefersTheRest) {
  struct mailbox_t {
    std::deque<std::optional<int>> results {8000, 10000, std::nullopt, 12000};
    int polls = 0;
    bool peek() = delete;
    std::optional<int> pop(std::chrono::milliseconds delay) {
      EXPECT_EQ(delay, std::chrono::milliseconds::zero());
      ++polls;
      if (results.empty()) return std::nullopt;
      auto result = results.front();
      results.pop_front();
      return result;
    }
  } mailbox;
  std::optional<int> latest;
  auto consume = [&](int bitrate) { latest = bitrate; };
  video::policy::drain_ready_control_events(mailbox, consume);
  EXPECT_EQ(mailbox.polls, 3);
  EXPECT_EQ(latest, 10000);
  ASSERT_EQ(mailbox.results.size(), 1);
  video::policy::drain_ready_control_events(mailbox, consume);
  EXPECT_EQ(latest, 12000);
  EXPECT_EQ(mailbox.polls, 5);
}

TEST(VideoControlPolicy, QueuedReferenceUpdatesKeepOrder) {
  safe::queue_t<int> queue;
  queue.raise(41);
  queue.raise(42);
  std::vector<int> delivered;
  video::policy::drain_ready_control_events(queue, [&](int index) { delivered.push_back(index); });
  EXPECT_EQ(delivered, (std::vector<int> {41, 42}));
  EXPECT_FALSE(queue.peek());
}

TEST(VideoControlPolicy, IdrSignalIsConsumedOnceByZeroWaitPoll) {
  using namespace std::chrono_literals;
  safe::event_t<bool> idr;
  EXPECT_FALSE(idr.pop(0ms));
  idr.raise(true);
  EXPECT_TRUE(idr.pop(0ms));
  EXPECT_FALSE(idr.pop(0ms));
}

namespace {
  video::latency_trace::frame_sample_t latency_sample(double capture, double convert, double submit, double encode, double deliver) {
    video::latency_trace::frame_sample_t sample;
    sample.stage_ms = {capture, convert, submit, encode, deliver};
    sample.total_ms = capture + convert + submit + encode + deliver;
    return sample;
  }
}  // namespace

TEST(VideoLatencyTrace, SampleSplitsStagesAndClampsCrossThreadSkew) {
  using namespace std::chrono_literals;
  const auto t0 = std::chrono::steady_clock::time_point {} + 1s;
  // Output stamped on the pump thread slightly before the encode thread's
  // submit stamp must not produce a negative stage.
  const auto sample = video::latency_trace::make_sample(t0, t0 + 200us, t0 + 500us, t0 + 700us, t0 + 650us, t0 + 3ms);
  EXPECT_NEAR(sample.stage_ms[0], 0.2, 1e-9);
  EXPECT_NEAR(sample.stage_ms[1], 0.3, 1e-9);
  EXPECT_NEAR(sample.stage_ms[2], 0.2, 1e-9);
  EXPECT_EQ(sample.stage_ms[3], 0.0);
  EXPECT_NEAR(sample.stage_ms[4], 2.35, 1e-9);
  EXPECT_NEAR(sample.total_ms, 3.0, 1e-9);
}

TEST(VideoLatencyTrace, AttributesSpikesToTheStageThatGrew) {
  video::latency_trace::window_t window(1.0);
  for (int i = 0; i < 100; ++i) {
    window.add(latency_sample(0.1, 0.1, 0.1, 2.5, 0.2));
  }
  window.add(latency_sample(0.1, 0.1, 0.1, 4.6, 0.2));
  window.add(latency_sample(0.1, 0.1, 0.1, 4.2, 0.2));
  window.add(latency_sample(1.7, 0.1, 0.1, 2.5, 0.2));
  // Below the median + 1 ms threshold: not a spike.
  window.add(latency_sample(0.1, 0.1, 0.1, 3.0, 0.2));

  const auto summary = window.summarize();
  EXPECT_EQ(summary.frames, 104u);
  EXPECT_NEAR(summary.total.median_ms, 3.0, 1e-9);
  EXPECT_NEAR(summary.spike_threshold_ms, 4.0, 1e-9);
  EXPECT_EQ(summary.spikes, 3u);
  EXPECT_EQ(summary.spike_causes[static_cast<std::size_t>(video::latency_trace::stage_e::encode)], 2u);
  EXPECT_EQ(summary.spike_causes[static_cast<std::size_t>(video::latency_trace::stage_e::capture)], 1u);
  EXPECT_NEAR(summary.worst.total_ms, 5.1, 1e-9);
  EXPECT_NEAR(summary.stages[static_cast<std::size_t>(video::latency_trace::stage_e::encode)].max_ms, 4.6, 1e-9);

  const auto line = video::latency_trace::format_summary(summary, 10.0);
  EXPECT_NE(line.find("104 frames"), std::string::npos);
  EXPECT_NE(line.find("spikes >= 4.00 ms: 3 (capture 1, encode 2)"), std::string::npos);

  window.reset();
  EXPECT_EQ(window.size(), 0u);
  EXPECT_EQ(window.summarize().frames, 0u);
}

TEST(VideoLatencyTrace, SummarizesSourceAgeOnlyWhenKnown) {
  video::latency_trace::window_t window(1.0);
  auto aged = latency_sample(0.1, 0.1, 0.1, 2.5, 0.2);
  aged.source_age_ms = 1.5;
  window.add(aged);
  aged.source_age_ms = 3.5;
  window.add(aged);
  window.add(latency_sample(0.1, 0.1, 0.1, 2.5, 0.2));  // Unknown age.

  const auto summary = window.summarize();
  EXPECT_EQ(summary.source_age_frames, 2u);
  EXPECT_NEAR(summary.source_age.max_ms, 3.5, 1e-9);
  EXPECT_NE(video::latency_trace::format_summary(summary, 10.0).find("source age"), std::string::npos);

  video::latency_trace::window_t unknown(1.0);
  unknown.add(latency_sample(0.1, 0.1, 0.1, 2.5, 0.2));
  EXPECT_EQ(video::latency_trace::format_summary(unknown.summarize(), 10.0).find("source age"), std::string::npos);
}

TEST(CapturePolicy, ExactAndSyntheticSourcesRejectProcessDisplayOverride) {
  EXPECT_TRUE(video::policy::may_apply_process_display_preference(video::policy::capture_selection_e::process_preferred));
  EXPECT_FALSE(video::policy::may_apply_process_display_preference(video::policy::capture_selection_e::exact_output));
  EXPECT_FALSE(video::policy::may_apply_process_display_preference(video::policy::capture_selection_e::synthetic_black));
}

TEST(CapturePolicy, PersistentDisplayFailureBacksOffWithoutDelayingInitialRecovery) {
  using namespace std::chrono_literals;

  EXPECT_EQ(video::policy::display_retry_delay(0), 50ms);
  EXPECT_EQ(video::policy::display_retry_delay(1), 100ms);
  EXPECT_EQ(video::policy::display_retry_delay(5), 1600ms);
  EXPECT_EQ(video::policy::display_retry_delay(6), 2s);
  EXPECT_EQ(video::policy::display_retry_delay(1000), 2s);

  EXPECT_TRUE(video::policy::should_log_display_retry(0));
  EXPECT_TRUE(video::policy::should_log_display_retry(1));
  EXPECT_TRUE(video::policy::should_log_display_retry(2));
  EXPECT_FALSE(video::policy::should_log_display_retry(3));
  EXPECT_TRUE(video::policy::should_log_display_retry(16));
  EXPECT_FALSE(video::policy::should_log_display_retry(17));
}

TEST(CapturePolicy, ExactOutputRejectsManualSwitchAndActiveOutputKeepsStableIdentity) {
  const std::array<std::string, 2> original_order {"Display-A", "Display-B"};
  const std::array<std::string, 2> reordered {"Display-B", "Display-A"};

  EXPECT_FALSE(video::policy::select_manual_display_output(
    video::policy::capture_selection_e::exact_output,
    1,
    original_order
  ));

  const auto selected = video::policy::select_manual_display_output(
    video::policy::capture_selection_e::process_preferred,
    1,
    original_order
  );
  ASSERT_EQ(selected, "Display-B");
  EXPECT_EQ(video::policy::resolve_display_output(*selected, reordered), 0);
  EXPECT_FALSE(video::policy::resolve_display_output(*selected, std::array<std::string, 1> {"Display-A"}));
}

TEST(CapturePolicy, QueueOverflowRejectsAndSignalsWithoutDiscardingPriorSession) {
  using namespace std::chrono_literals;

  safe::queue_t<int> queue {1};
  safe::signal_t shutdown_signal;
  safe::signal_t join_signal;
  ASSERT_TRUE(queue.try_raise(7));

  EXPECT_FALSE(video::policy::try_admit_capture_session(
    queue,
    9,
    shutdown_signal,
    join_signal
  ));
  EXPECT_TRUE(shutdown_signal.peek());
  EXPECT_TRUE(join_signal.peek());
  EXPECT_EQ(queue.pop(0ms), 7);
  EXPECT_FALSE(queue.pop(0ms));
}

TEST(CapturePolicy, AcceptedSessionAdmissionLeavesSignalsForTheWorker) {
  using namespace std::chrono_literals;

  safe::queue_t<int> queue {1};
  safe::signal_t shutdown_signal;
  safe::signal_t join_signal;

  EXPECT_TRUE(video::policy::try_admit_capture_session(
    queue,
    7,
    shutdown_signal,
    join_signal
  ));
  EXPECT_FALSE(shutdown_signal.peek());
  EXPECT_FALSE(join_signal.peek());
  EXPECT_EQ(queue.pop(0ms), 7);
}

TEST(EncoderPolicy, SelectsFirstAvailableCapableEncoderWithoutHardwareProbe) {
  FakeEncoderProvider provider;
  provider.values["nvenc"] = {false, true, true};
  provider.values["software"] = {true, true, false};
  const std::array<std::string_view, 2> preference {"nvenc", "software"};
  EXPECT_EQ(video::policy::select_encoder(preference, {.hdr = true}, provider), "software");
}

TEST(EncoderPolicy, RejectsEncoderThatCannotMeetRequestedFormat) {
  FakeEncoderProvider provider;
  provider.values["software"] = {true, true, false};
  const std::array<std::string_view, 1> preference {"software"};
  EXPECT_FALSE(video::policy::select_encoder(preference, {.hdr = true, .yuv444 = true}, provider));
}

struct FramerateX100Test: testing::TestWithParam<std::tuple<std::int32_t, video::policy::rational_t>> {};
TEST_P(FramerateX100Test, Run) {
  const auto &[value, expected] = GetParam();
  EXPECT_EQ(video::policy::framerate_x100_to_rational(value), expected);
}
INSTANTIATE_TEST_SUITE_P(
  FramerateX100Tests,
  FramerateX100Test,
  testing::Values(
    std::make_tuple(2397, video::policy::rational_t {24000, 1001}),
    std::make_tuple(2398, video::policy::rational_t {24000, 1001}),
    std::make_tuple(2500, video::policy::rational_t {25, 1}),
    std::make_tuple(2997, video::policy::rational_t {30000, 1001}),
    std::make_tuple(6000, video::policy::rational_t {60, 1}),
    std::make_tuple(11988, video::policy::rational_t {120000, 1001}),
    std::make_tuple(23976, video::policy::rational_t {240000, 1001}),
    std::make_tuple(9498, video::policy::rational_t {4749, 50})
  )
);

TEST(VideoOutputPolicy, KeepsConfiguredVirtualOutputWhenAnotherVirtualDisplayEnumeratesFirst) {
  const std::array<std::string, 2> active_outputs {
    "\\\\.\\DISPLAY54",
    "\\\\.\\DISPLAY53",
  };

  EXPECT_EQ(
    video::policy::select_preferred_virtual_output(
      "\\\\.\\display53",
      active_outputs,
      active_outputs
    ),
    "\\\\.\\DISPLAY53"
  );
}

TEST(VideoOutputPolicy, FallsBackToFirstActiveVirtualOutputWithoutConfiguredAffinity) {
  const std::array<std::string, 2> active_outputs {
    "\\\\.\\DISPLAY54",
    "\\\\.\\DISPLAY53",
  };

  EXPECT_EQ(
    video::policy::select_preferred_virtual_output("", active_outputs, active_outputs),
    "\\\\.\\DISPLAY54"
  );
}
