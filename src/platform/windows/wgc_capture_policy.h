#pragma once

#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>

namespace platf::dxgi::wgc_policy {
  inline constexpr std::uint32_t low_latency_initial_buffer_size = 1;
  inline constexpr std::uint32_t adaptive_max_buffer_size = 2;
  inline constexpr std::uint32_t helper_stop_timeout_ms = 3000;

  // Absolute input uses the whole virtual desktop, not just the captured
  // monitor. A neighbouring monitor can change these values without moving
  // or resizing the capture target itself.
  struct input_geometry_t {
    int offset_x;
    int offset_y;
    int desktop_width;
    int desktop_height;

    constexpr bool operator==(const input_geometry_t &) const = default;
  };

  struct desktop_bounds_t {
    int origin_x;
    int origin_y;
    int width;
    int height;
  };

  enum class input_geometry_change_e {
    unchanged,
    changed,
    unavailable,
  };

  constexpr input_geometry_change_e assess_input_geometry(
    const input_geometry_t &captured,
    const int monitor_x,
    const int monitor_y,
    const desktop_bounds_t &current
  ) noexcept {
    // GetSystemMetrics returns zero on failure. Preserve capture while the
    // desktop is temporarily unavailable, as with other DXGI settle retries.
    if (current.width <= 0 || current.height <= 0) {
      return input_geometry_change_e::unavailable;
    }
    const bool unchanged =
      captured.offset_x == static_cast<std::int64_t>(monitor_x) - current.origin_x &&
      captured.offset_y == static_cast<std::int64_t>(monitor_y) - current.origin_y &&
      captured.desktop_width == current.width && captured.desktop_height == current.height;
    return unchanged ? input_geometry_change_e::unchanged : input_geometry_change_e::changed;
  }

  /**
   * Select only the requested monitor, allowing transient enumeration failures
   * to settle. A missing explicit target must never turn into primary capture.
   * The caller supplies the bounded wait and the platform monitor lookups.
   */
  template<class FindRequested, class FindPrimary, class WaitForRetry>
  auto select_monitor(
    const bool has_requested_monitor,
    FindRequested find_requested,
    FindPrimary find_primary,
    WaitForRetry wait_for_retry
  ) {
    if (!has_requested_monitor) {
      return find_primary();
    }

    auto monitor = find_requested();
    while (!monitor && wait_for_retry()) {
      monitor = find_requested();
    }
    return monitor;
  }

  enum class capture_surface_format : std::uint8_t {
    bgra8,
    rgba16_float,
  };

  constexpr capture_surface_format select_capture_surface_format(
    const bool config_received,
    const bool force_sdr_capture,
    const bool dynamic_range,
    const bool advanced_color_capture
  ) noexcept {
    return config_received &&
             !force_sdr_capture &&
             (dynamic_range || advanced_color_capture) ?
             capture_surface_format::rgba16_float :
             capture_surface_format::bgra8;
  }

  constexpr std::uint32_t maximum_buffer_size(const bool vrr_low_latency) noexcept {
    return vrr_low_latency ? low_latency_initial_buffer_size : adaptive_max_buffer_size;
  }

  /**
   * A new WGC frame may skip the scratch handoff and go straight into the
   * shared texture only when no older frame is queued or mid-delivery;
   * otherwise it would be published ahead of that older frame.
   */
  constexpr bool may_publish_directly(const bool stopping, const bool frame_pending, const bool frame_delivering) noexcept {
    return !stopping && !frame_pending && !frame_delivering;
  }

  constexpr bool buffer_pool_is_quiet(
    const bool allow_decrease,
    const bool has_recent_drop,
    const bool recent_pool_pressure,
    const int peak_outstanding,
    const std::uint32_t current_buffer_size
  ) noexcept {
    return allow_decrease &&
           !has_recent_drop &&
           !recent_pool_pressure &&
           peak_outstanding <= static_cast<int>(current_buffer_size) - 1;
  }

  // ---- Slot-aligned publication -------------------------------------------
  //
  // The host claims one helper frame per pacing slot: slot k = anchor + k * period.
  // A frame published earlier than the last composition before a slot is
  // overwritten unused, but its full-frame GPU copy still competes with the
  // encoder's input. Knowing the host's slots, the helper can hold such a frame
  // (uncopied) and publish it only if no newer composition arrives in time.

  /// Host pacing grid in QPC ticks. period <= 0 means the host has no grid.
  struct host_claim_grid_t {
    std::int64_t anchor_qpc = 0;
    double period_qpc = 0.0;
  };

  /// First host claim strictly after now, or 0 without a grid.
  inline std::int64_t next_host_claim(const host_claim_grid_t &grid, const std::int64_t now_qpc) noexcept {
    if (grid.period_qpc <= 0.0) {
      return 0;
    }
    const double elapsed = static_cast<double>(now_qpc - grid.anchor_qpc);
    const double slots = elapsed < 0.0 ? 0.0 : static_cast<double>(static_cast<std::int64_t>(elapsed / grid.period_qpc)) + 1.0;
    return grid.anchor_qpc + static_cast<std::int64_t>(slots * grid.period_qpc + 0.5);
  }

  struct publish_timing_t {
    std::int64_t next_publish_lead_qpc;  ///< A frame published on arrival must land this long before the claim.
    std::int64_t deadline_lead_qpc;  ///< A held frame is published this long before the claim (covers timer wake-up).
    std::int64_t arrival_tolerance_qpc;  ///< Allowed lateness of the next composition.
    std::int64_t host_grace_qpc;  ///< How long the host waits after a claim that found no new frame.
  };

  struct publish_plan_t {
    bool defer = false;
    std::int64_t deadline_qpc = 0;  ///< When a deferred frame must be published if nothing newer arrived.
  };

  /**
   * Decide whether a newly arrived frame is published now or held.
   * Hold it only when the next composition is expected early enough to be
   * published before the same host claim. The held frame is published at the
   * deadline if that composition never comes (static content, slower source),
   * so no update is lost. A host whose last claim found nothing new is still
   * waiting for a frame, so anything arriving during that wait goes out at once.
   */
  inline publish_plan_t plan_publication(
    const host_claim_grid_t &grid,
    const std::int64_t now_qpc,
    const std::int64_t composition_period_qpc,
    const std::int64_t last_publish_qpc,
    const publish_timing_t &timing
  ) noexcept {
    const std::int64_t next_claim = next_host_claim(grid, now_qpc);
    if (next_claim <= 0 || composition_period_qpc <= 0) {
      return {};
    }
    const auto period = static_cast<std::int64_t>(grid.period_qpc + 0.5);
    // Only a source faster than the stream produces frames the host never claims.
    // At or near one composition per slot (a game capped to the stream rate),
    // holding a frame could only make it later.
    if (composition_period_qpc * 5 > period * 3) {
      return {};
    }
    const std::int64_t previous_claim = next_claim - period;
    const bool host_waiting = now_qpc - previous_claim < timing.host_grace_qpc &&
                              last_publish_qpc <= previous_claim - period;
    if (host_waiting) {
      return {};
    }
    // The newer composition must arrive, and be published on arrival, before the claim.
    const std::int64_t next_composition = now_qpc + composition_period_qpc + timing.arrival_tolerance_qpc;
    if (next_composition + timing.next_publish_lead_qpc > next_claim) {
      return {};
    }
    const std::int64_t deadline = next_claim - timing.deadline_lead_qpc;
    if (deadline <= now_qpc) {
      return {};
    }
    return {true, deadline};
  }

  /**
   * Typical recent interval between compositions (median of the last 16).
   * A median ignores the odd late callback that a minimum would latch onto.
   */
  class composition_interval_t {
  public:
    void add(const std::int64_t arrival_qpc, const std::int64_t max_interval_qpc) noexcept {
      if (_has_last) {
        const auto interval = arrival_qpc - _last_qpc;
        if (interval > 0 && interval <= max_interval_qpc) {
          _intervals[_next] = interval;
          _next = (_next + 1) % _intervals.size();
          if (_count < _intervals.size()) {
            ++_count;
          }
        }
      }
      _last_qpc = arrival_qpc;
      _has_last = true;
    }

    /// 0 until enough samples exist.
    std::int64_t estimate() const noexcept {
      if (_count < minimum_samples) {
        return 0;
      }
      auto sorted = _intervals;
      std::sort(sorted.begin(), sorted.begin() + static_cast<std::ptrdiff_t>(_count));
      return sorted[_count / 2];
    }

    void reset() noexcept {
      *this = {};
    }

  private:
    static constexpr std::size_t minimum_samples = 8;
    std::array<std::int64_t, 16> _intervals {};
    std::size_t _next = 0;
    std::size_t _count = 0;
    std::int64_t _last_qpc = 0;
    bool _has_last = false;
  };
}  // namespace platf::dxgi::wgc_policy
