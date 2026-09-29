/**
 * @file tests/unit/test_wgc_monitor_selection.cpp
 * @brief Capture target selection across monitor topology changes.
 */
#include "../tests_common.h"

#include <src/platform/windows/wgc_capture_policy.h>

#include <algorithm>
#include <cstdint>
#include <optional>
#include <utility>
#include <vector>

namespace {
  struct monitor_topology {
    int primary = 1;
    std::vector<int> requested {16};
    std::size_t enumeration = 0;
    int primary_queries = 0;
    int waits = 0;
    int retry_budget = 3;

    int select(const bool explicit_target = true) {
      return platf::dxgi::wgc_policy::select_monitor(
        explicit_target,
        [&] {
          const auto index = std::min(enumeration++, requested.size() - 1);
          return requested[index];
        },
        [&] {
          ++primary_queries;
          return primary;
        },
        [&] {
          if (waits >= retry_budget) {
            return false;
          }
          ++waits;
          return true;
        }
      );
    }
  };
}  // namespace

TEST(WgcMonitorSelection, ExplicitTargetDoesNotQueryPrimary) {
  monitor_topology topology;
  EXPECT_EQ(topology.select(), 16);
  EXPECT_EQ(topology.primary_queries, 0);
  EXPECT_EQ(topology.waits, 0);
}

TEST(WgcMonitorSelection, MissingExplicitTargetDoesNotCaptureAvailablePrimary) {
  monitor_topology topology;
  topology.requested = {0};
  EXPECT_EQ(topology.select(), 0);
  EXPECT_EQ(topology.primary_queries, 0);
  EXPECT_EQ(topology.waits, topology.retry_budget);
}

TEST(WgcMonitorSelection, TargetCanReappearDuringTopologySettle) {
  monitor_topology topology;
  topology.requested = {0, 0, 16};
  EXPECT_EQ(topology.select(), 16);
  EXPECT_EQ(topology.primary_queries, 0);
  EXPECT_EQ(topology.waits, 2);
}

TEST(WgcMonitorSelection, ReselectionDoesNotKeepADisconnectedHandle) {
  monitor_topology topology;
  topology.requested = {16, 0};
  EXPECT_EQ(topology.select(), 16);
  EXPECT_EQ(topology.select(), 0);
  EXPECT_EQ(topology.primary_queries, 0);
}

TEST(WgcMonitorSelection, EmptyTargetUsesPrimaryWithoutWaitingOrEnumeration) {
  monitor_topology topology;
  EXPECT_EQ(topology.select(false), 1);
  EXPECT_EQ(topology.primary_queries, 1);
  EXPECT_EQ(topology.enumeration, 0u);
  EXPECT_EQ(topology.waits, 0);
}

TEST(WgcMonitorSelection, EmptyTargetReportsUnavailablePrimary) {
  monitor_topology topology;
  topology.primary = 0;
  EXPECT_EQ(topology.select(false), 0);
  EXPECT_EQ(topology.primary_queries, 1);
  EXPECT_EQ(topology.enumeration, 0u);
  EXPECT_EQ(topology.waits, 0);
}

TEST(WgcInputGeometry, RefreshOnlyChangeKeepsInputMapping) {
  using namespace platf::dxgi::wgc_policy;
  EXPECT_EQ(assess_input_geometry({1920, 0, 4480, 1600}, 1920, 0, {0, 0, 4480, 1600}), input_geometry_change_e::unchanged);
}

TEST(WgcInputGeometry, NegativeDesktopOriginNormalizesTargetExactlyOnce) {
  using namespace platf::dxgi::wgc_policy;
  EXPECT_EQ(assess_input_geometry({1920, 1080, 4480, 2680}, 0, 0, {-1920, -1080, 4480, 2680}), input_geometry_change_e::unchanged);
  EXPECT_EQ(assess_input_geometry({0, 0, 4480, 2680}, -1920, -1080, {-1920, -1080, 4480, 2680}), input_geometry_change_e::unchanged);
}

TEST(WgcInputGeometry, MovingAnotherMonitorChangesNormalization) {
  using namespace platf::dxgi::wgc_policy;
  // Target stays at (0, 0); another output moves farther right/below.
  EXPECT_EQ(assess_input_geometry({0, 0, 4480, 1600}, 0, 0, {0, 0, 6400, 1600}), input_geometry_change_e::changed);
  EXPECT_EQ(assess_input_geometry({0, 0, 2560, 2680}, 0, 0, {0, 0, 2560, 3200}), input_geometry_change_e::changed);
}

TEST(WgcInputGeometry, ReinitializationRebuildsOffsetsAfterNeighbourCrossesOrigin) {
  using namespace platf::dxgi::wgc_policy;
  const desktop_bounds_t moved {-1920, -1080, 4480, 2680};
  EXPECT_EQ(assess_input_geometry({0, 0, 4480, 2680}, 0, 0, moved), input_geometry_change_e::changed);
  EXPECT_EQ(assess_input_geometry({1920, 1080, 4480, 2680}, 0, 0, moved), input_geometry_change_e::unchanged);
}

TEST(WgcInputGeometry, UnavailableMetricsDoNotForceCaptureReinitialization) {
  using namespace platf::dxgi::wgc_policy;
  const input_geometry_t captured {1920, 0, 4480, 1600};
  for (const auto bounds : {desktop_bounds_t {0, 0, 0, 1600}, {0, 0, 4480, 0}, {0, 0, -1, 1600}, {0, 0, 4480, -1}}) {
    EXPECT_EQ(assess_input_geometry(captured, 1920, 0, bounds), input_geometry_change_e::unavailable);
  }
  EXPECT_EQ(assess_input_geometry(captured, 1920, 0, {0, 0, 4480, 1600}), input_geometry_change_e::unchanged);
}

TEST(WgcHelperPublication, DirectPublishOnlyWhenNothingOlderIsInFlight) {
  using platf::dxgi::wgc_policy::may_publish_directly;
  EXPECT_TRUE(may_publish_directly(false, false, false));
  // A queued or mid-delivery scratch frame is older and must be published first.
  EXPECT_FALSE(may_publish_directly(false, true, false));
  EXPECT_FALSE(may_publish_directly(false, false, true));
  EXPECT_FALSE(may_publish_directly(true, false, false));
}

namespace {
  namespace wgc_policy = platf::dxgi::wgc_policy;

  constexpr std::int64_t qpc_per_ms = 10000;  // 10 MHz QPC

  wgc_policy::host_claim_grid_t grid_120fps(std::int64_t anchor) {
    return {anchor, 1000.0 / 120.0 * qpc_per_ms};
  }

  constexpr wgc_policy::publish_timing_t test_timing {
    .next_publish_lead_qpc = 1500,  // 0.15 ms
    .deadline_lead_qpc = 2500,  // 0.25 ms
    .arrival_tolerance_qpc = 500,  // 0.05 ms
    .host_grace_qpc = 60000,  // 6 ms
  };

  struct publication_run_t {
    int published = 0;
    int claims = 0;
    int claims_of_newest = 0;
  };

  // Deterministic compositions every `composition_ms` (callback 0.3 ms after
  // vblank) against 120 Hz host claims `delta_ms` after an arrival. The host
  // claims the newest frame published before each slot.
  publication_run_t run_publication(double composition_ms, double delta_ms, bool slot_aligned) {
    const auto grid = grid_120fps(static_cast<std::int64_t>((0.3 + delta_ms) * qpc_per_ms));
    wgc_policy::composition_interval_t intervals;
    std::vector<std::pair<std::int64_t, std::int64_t>> publications;  // (visible_at, arrival)
    std::optional<std::pair<std::int64_t, std::int64_t>> held;  // (deadline, arrival)
    std::vector<std::int64_t> arrivals;
    std::int64_t last_publish = 0;
    for (double t = 0.0; t < 1000.0; t += composition_ms) {
      arrivals.push_back(static_cast<std::int64_t>((t + 0.3) * qpc_per_ms));
    }
    for (const auto arrival : arrivals) {
      if (held && held->first <= arrival) {
        publications.push_back({held->first + 1000, held->second});
        last_publish = held->first + 1000;
        held.reset();
      }
      intervals.add(arrival, 50 * qpc_per_ms);
      held.reset();
      const auto plan = slot_aligned ?
                          wgc_policy::plan_publication(grid, arrival, intervals.estimate(), last_publish, test_timing) :
                          wgc_policy::publish_plan_t {};
      if (plan.defer) {
        held = std::pair {plan.deadline_qpc, arrival};
      } else {
        publications.push_back({arrival + 1000, arrival});
        last_publish = arrival + 1000;
      }
    }
    publication_run_t run;
    run.published = static_cast<int>(publications.size());
    std::size_t next = 0;
    std::int64_t claimed = -1;
    for (std::int64_t slot = grid.anchor_qpc; slot < 1000 * qpc_per_ms; slot += static_cast<std::int64_t>(grid.period_qpc)) {
      while (next < publications.size() && publications[next].first <= slot) {
        ++next;
      }
      if (next == 0 || publications[next - 1].second <= claimed) {
        continue;
      }
      claimed = publications[next - 1].second;
      ++run.claims;
      const auto newest = std::upper_bound(arrivals.begin(), arrivals.end(), slot - 1500) - 1;
      run.claims_of_newest += *newest == claimed ? 1 : 0;
    }
    return run;
  }
}  // namespace

TEST(WgcSlotAlignedPublication, NextClaimFollowsTheHostGrid) {
  const auto grid = grid_120fps(1000);
  EXPECT_EQ(wgc_policy::next_host_claim(grid, 0), 1000);
  EXPECT_EQ(wgc_policy::next_host_claim(grid, 1000), 1000 + 83333);
  EXPECT_EQ(wgc_policy::next_host_claim(grid, 1000 + 83334), 1000 + 166667);
  EXPECT_EQ(wgc_policy::next_host_claim({}, 5000), 0);
}

TEST(WgcSlotAlignedPublication, HoldsOnlyFramesANewerCompositionWillReplace) {
  const auto grid = grid_120fps(0);
  const std::int64_t slot = 83333;
  const std::int64_t composition_240hz = 41667;
  // 5 ms before the claim: the next 240 Hz composition lands 0.8 ms before it.
  auto plan = wgc_policy::plan_publication(grid, slot - 50000, composition_240hz, slot - 90000, test_timing);
  EXPECT_TRUE(plan.defer);
  EXPECT_EQ(plan.deadline_qpc, slot - test_timing.deadline_lead_qpc);
  // 3 ms before the claim: no newer frame can make it, publish now.
  plan = wgc_policy::plan_publication(grid, slot - 30000, composition_240hz, slot - 90000, test_timing);
  EXPECT_FALSE(plan.defer);
  // A source at the stream rate is never held.
  plan = wgc_policy::plan_publication(grid, slot - 80000, 83333, slot - 90000, test_timing);
  EXPECT_FALSE(plan.defer);
  // Without a grid or an interval estimate everything is published.
  EXPECT_FALSE(wgc_policy::plan_publication({}, slot - 50000, composition_240hz, 0, test_timing).defer);
  EXPECT_FALSE(wgc_policy::plan_publication(grid, slot - 50000, 0, 0, test_timing).defer);
}

TEST(WgcSlotAlignedPublication, PublishesAtOnceWhileTheHostIsWaiting) {
  const auto grid = grid_120fps(0);
  // Nothing was published during the whole previous slot, so the host missed
  // its claim at 83333 and is waiting in its grace window.
  const auto plan = wgc_policy::plan_publication(grid, 83333 + 1000, 41667, 0, test_timing);
  EXPECT_FALSE(plan.defer);
}

TEST(WgcSlotAlignedPublication, IntervalEstimateIgnoresALateCallback) {
  wgc_policy::composition_interval_t intervals;
  std::int64_t t = 0;
  intervals.add(t, 500000);
  for (int i = 0; i < 6; ++i) {
    intervals.add(t += 41667, 500000);
  }
  EXPECT_EQ(intervals.estimate(), 0);  // Needs 8 intervals.
  intervals.add(t += 41667 + 15000, 500000);  // One late callback...
  intervals.add(t += 41667 - 15000, 500000);  // ...and the short interval after it.
  EXPECT_EQ(intervals.estimate(), 41667);
}

TEST(WgcSlotAlignedPublication, PublishesOneFramePerClaimWithoutLosingFreshness) {
  // 240 Hz content against 120 Hz claims: half the frames are never claimed.
  const auto every = run_publication(1000.0 / 240.0, 0.5, false);
  const auto aligned = run_publication(1000.0 / 240.0, 0.5, true);
  EXPECT_GT(every.published, 230);
  EXPECT_LE(aligned.published, 128);
  EXPECT_EQ(aligned.claims, every.claims);
  EXPECT_EQ(aligned.claims_of_newest, every.claims_of_newest);

  // 480 Hz: three of four frames are skipped.
  const auto aligned_480 = run_publication(1000.0 / 480.0, 0.5, true);
  EXPECT_LE(aligned_480.published, 128);
  EXPECT_EQ(aligned_480.claims_of_newest, run_publication(1000.0 / 480.0, 0.5, false).claims_of_newest);

  // A game capped to the stream rate: every frame is still published.
  const auto game = run_publication(1000.0 / 120.0, 0.5, true);
  EXPECT_EQ(game.published, run_publication(1000.0 / 120.0, 0.5, false).published);
}
