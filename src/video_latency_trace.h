/**
 * @file src/video_latency_trace.h
 * @brief Per-stage breakdown of host processing latency for native encoders.
 *
 * Host processing latency (what Moonlight shows) runs from the capture
 * timestamp to the moment the video broadcast thread starts sending the
 * frame. This splits that interval into stages so a latency spike can be
 * attributed to the stage that caused it.
 */
#pragma once

#include <algorithm>
#include <array>
#include <chrono>
#include <cstddef>
#include <iomanip>
#include <sstream>
#include <string>
#include <vector>

namespace video::latency_trace {

  enum class stage_e : std::size_t {
    capture,  ///< Capture timestamp -> encode thread received the image (copy submit, mailbox, wakeup).
    convert,  ///< Colour-conversion submission on the encode thread.
    submit,  ///< Surface preparation and encoder SubmitInput.
    encode,  ///< SubmitInput -> bitstream returned (GPU conversion + hardware encode + output wait).
    deliver,  ///< Bitstream returned -> broadcast thread starts sending (copy, queue, wakeup).
    count,
  };

  inline constexpr std::size_t stage_count = static_cast<std::size_t>(stage_e::count);
  inline constexpr std::array<const char *, stage_count> stage_names {"capture", "convert", "submit", "encode", "deliver"};

  struct frame_sample_t {
    std::array<double, stage_count> stage_ms {};
    double total_ms = 0.0;
  };

  /**
   * @brief Build a sample from the stage timestamps of one frame.
   * @details Timestamps come from different threads. A stage that appears to
   *          run backwards is clamped to zero instead of producing a negative
   *          duration; the total is measured end to end, independently.
   */
  inline frame_sample_t make_sample(
    std::chrono::steady_clock::time_point captured,
    std::chrono::steady_clock::time_point popped,
    std::chrono::steady_clock::time_point converted,
    std::chrono::steady_clock::time_point submitted,
    std::chrono::steady_clock::time_point output,
    std::chrono::steady_clock::time_point sending
  ) {
    auto ms = [](std::chrono::steady_clock::time_point from, std::chrono::steady_clock::time_point to) {
      return std::max(0.0, std::chrono::duration<double, std::milli>(to - from).count());
    };
    frame_sample_t sample;
    sample.stage_ms = {ms(captured, popped), ms(popped, converted), ms(converted, submitted), ms(submitted, output), ms(output, sending)};
    sample.total_ms = ms(captured, sending);
    return sample;
  }

  struct stage_summary_t {
    double median_ms = 0.0;
    double p99_ms = 0.0;
    double max_ms = 0.0;
  };

  struct window_summary_t {
    std::size_t frames = 0;
    stage_summary_t total;
    std::array<stage_summary_t, stage_count> stages {};
    double spike_threshold_ms = 0.0;
    std::size_t spikes = 0;
    /// For each spike, the stage that exceeded its own median by the most.
    std::array<std::size_t, stage_count> spike_causes {};
    frame_sample_t worst;
  };

  /**
   * @brief Collects frame samples and summarizes them per reporting window.
   * @details A spike is a frame whose total exceeds the window's median total
   *          by at least spike_margin_ms. Each spike is attributed to the stage
   *          with the largest excess over that stage's median in the window.
   */
  class window_t {
  public:
    explicit window_t(double spike_margin_ms = 1.0, std::size_t max_samples = 8192):
        _spike_margin_ms(spike_margin_ms),
        _max_samples(max_samples) {
      _samples.reserve(std::min<std::size_t>(max_samples, 2048));
    }

    void add(const frame_sample_t &sample) {
      if (_samples.size() < _max_samples) {
        _samples.push_back(sample);
      }
    }

    std::size_t size() const {
      return _samples.size();
    }

    window_summary_t summarize() const {
      window_summary_t summary;
      summary.frames = _samples.size();
      if (_samples.empty()) {
        return summary;
      }

      std::vector<double> values(_samples.size());
      auto summarize_values = [&](auto extract) {
        for (std::size_t i = 0; i < _samples.size(); ++i) {
          values[i] = extract(_samples[i]);
        }
        std::sort(values.begin(), values.end());
        auto at = [&](double fraction) {
          return values[std::min(values.size() - 1, static_cast<std::size_t>(fraction * static_cast<double>(values.size() - 1) + 0.5))];
        };
        return stage_summary_t {at(0.5), at(0.99), values.back()};
      };

      summary.total = summarize_values([](const frame_sample_t &sample) {
        return sample.total_ms;
      });
      for (std::size_t stage = 0; stage < stage_count; ++stage) {
        summary.stages[stage] = summarize_values([stage](const frame_sample_t &sample) {
          return sample.stage_ms[stage];
        });
      }

      summary.spike_threshold_ms = summary.total.median_ms + _spike_margin_ms;
      const frame_sample_t *worst = &_samples.front();
      for (const auto &sample : _samples) {
        if (sample.total_ms > worst->total_ms) {
          worst = &sample;
        }
        if (sample.total_ms < summary.spike_threshold_ms) {
          continue;
        }
        ++summary.spikes;
        std::size_t cause = 0;
        double largest_excess = sample.stage_ms[0] - summary.stages[0].median_ms;
        for (std::size_t stage = 1; stage < stage_count; ++stage) {
          const double excess = sample.stage_ms[stage] - summary.stages[stage].median_ms;
          if (excess > largest_excess) {
            largest_excess = excess;
            cause = stage;
          }
        }
        ++summary.spike_causes[cause];
      }
      summary.worst = *worst;
      return summary;
    }

    void reset() {
      _samples.clear();
    }

  private:
    double _spike_margin_ms;
    std::size_t _max_samples;
    std::vector<frame_sample_t> _samples;
  };

  inline std::string format_summary(const window_summary_t &summary, double window_seconds) {
    std::ostringstream out;
    out << std::fixed << std::setprecision(2);
    out << "Host latency stages (" << std::setprecision(1) << window_seconds << "s, " << summary.frames << " frames)"
        << std::setprecision(2) << ": total med " << summary.total.median_ms << " p99 " << summary.total.p99_ms
        << " max " << summary.total.max_ms << " ms";
    for (std::size_t stage = 0; stage < stage_count; ++stage) {
      const auto &value = summary.stages[stage];
      out << " | " << stage_names[stage] << ' ' << value.median_ms << '/' << value.p99_ms << '/' << value.max_ms;
    }
    out << " (med/p99/max) | spikes >= " << summary.spike_threshold_ms << " ms: " << summary.spikes;
    if (summary.spikes) {
      out << " (";
      bool first = true;
      for (std::size_t stage = 0; stage < stage_count; ++stage) {
        if (!summary.spike_causes[stage]) {
          continue;
        }
        out << (first ? "" : ", ") << stage_names[stage] << ' ' << summary.spike_causes[stage];
        first = false;
      }
      out << ')';
    }
    out << " | worst " << summary.worst.total_ms << " ms =";
    for (std::size_t stage = 0; stage < stage_count; ++stage) {
      out << ' ' << stage_names[stage] << ' ' << summary.worst.stage_ms[stage];
    }
    return out.str();
  }

}  // namespace video::latency_trace
