#include "src/platform/windows/display_helper_v2/async_dispatcher.h"

#include "src/platform/windows/display_helper_v2/diagnostics.h"

namespace display_helper::v2 {
  AsyncDispatcher::AsyncDispatcher(
    ApplyOperation &apply_operation,
    VerificationOperation &verification_operation,
    RecoveryOperation &recovery_operation,
    RecoveryValidationOperation &recovery_validation_operation,
    IClock &clock)
    : apply_operation_(apply_operation),
      verification_operation_(verification_operation),
      recovery_operation_(recovery_operation),
      recovery_validation_operation_(recovery_validation_operation),
      clock_(clock),
      worker_(&AsyncDispatcher::worker_loop, this),
      timer_worker_(&AsyncDispatcher::timer_loop, this) {}

  AsyncDispatcher::~AsyncDispatcher() {
    if (timer_worker_.joinable()) {
      timer_worker_.request_stop();
      timer_cv_.notify_all();
      timer_worker_.join();
    }
    if (worker_.joinable()) {
      worker_.request_stop();
      cv_.notify_all();
      worker_.join();
    }
  }

  void AsyncDispatcher::dispatch_apply(
    const ApplyRequest &request,
    const CancellationToken &token,
    std::chrono::milliseconds delay,
    std::function<void(const ApplyOutcome &)> completion) {
    enqueue_task([
      this,
      request,
      token,
      delay,
      completion = std::move(completion)
    ]() mutable {
      auto remaining_delay = delay;
      constexpr auto kCancellationSlice = std::chrono::milliseconds(100);
      while (remaining_delay > std::chrono::milliseconds::zero()) {
        if (token.is_cancelled()) {
          ApplyOutcome outcome;
          outcome.status = ApplyStatus::Fatal;
          completion(outcome);
          return;
        }
        const auto slice = remaining_delay > kCancellationSlice ? kCancellationSlice : remaining_delay;
        clock_.sleep_for(slice);
        remaining_delay -= slice;
      }

      completion(apply_operation_.run(request, token));
    });
  }

  void AsyncDispatcher::dispatch_verification(
    const ApplyRequest &request,
    const std::optional<ActiveTopology> &expected_topology,
    const std::optional<ResolvedConfigurationTarget> &resolved_target,
    const CancellationToken &token,
    std::function<void(bool)> completion) {
    enqueue_task([
      this,
      request,
      expected_topology,
      resolved_target,
      token,
      completion = std::move(completion)
    ]() mutable {
      completion(verification_operation_.run(request, expected_topology, resolved_target, token));
    });
  }

  void AsyncDispatcher::dispatch_verification_after(
    const ApplyRequest &request,
    const std::optional<ActiveTopology> &expected_topology,
    const std::optional<ResolvedConfigurationTarget> &resolved_target,
    const CancellationToken &token,
    std::chrono::milliseconds delay,
    std::function<void(bool)> completion) {
    enqueue_delayed_task([
      this,
      request,
      expected_topology,
      resolved_target,
      token,
      delay,
      completion = std::move(completion)
    ](std::stop_token stop_token) mutable {
      auto remaining_delay = delay;
      constexpr auto kCancellationSlice = std::chrono::milliseconds(100);
      while (remaining_delay > std::chrono::milliseconds::zero()) {
        if (stop_token.stop_requested() || token.is_cancelled()) {
          return;
        }
        const auto slice = remaining_delay > kCancellationSlice ? kCancellationSlice : remaining_delay;
        clock_.sleep_for(slice);
        remaining_delay -= slice;
      }
      if (stop_token.stop_requested() || token.is_cancelled()) {
        return;
      }
      enqueue_task([
        this,
        request,
        expected_topology,
        resolved_target,
        token,
        completion = std::move(completion)
      ]() mutable {
        completion(verification_operation_.run(request, expected_topology, resolved_target, token));
      });
    });
  }

  void AsyncDispatcher::dispatch_reset_staged_apply_state(
    std::function<void(bool)> completion) {
    enqueue_task([
      this,
      completion = std::move(completion)
    ]() mutable {
      // RESET is deliberately non-cancellable once ordered behind prior work.
      // A subsequent APPLY is queued after this task and observes the reset.
      completion(apply_operation_.reset_staged_apply_state());
    });
  }

  void AsyncDispatcher::dispatch_recovery(
    const CancellationToken &token,
    std::chrono::milliseconds delay,
    std::function<void(const RecoveryOutcome &)> completion) {
    enqueue_task([
      this,
      token,
      delay,
      completion = std::move(completion)
    ]() mutable {
      // Sleep in slices so a DISARM/APPLY during the revert grace window can
      // cancel the pending restore before it touches the display stack.
      auto remaining = delay;
      constexpr auto kSlice = std::chrono::milliseconds(50);
      while (remaining > std::chrono::milliseconds::zero()) {
        if (token.is_cancelled()) {
          completion(RecoveryOutcome {});
          return;
        }
        const auto slice = remaining > kSlice ? kSlice : remaining;
        clock_.sleep_for(slice);
        remaining -= slice;
      }

      completion(recovery_operation_.run(token));
    });
  }

  void AsyncDispatcher::dispatch_recovery_validation(
    const Snapshot &snapshot,
    const CancellationToken &token,
    std::function<void(bool)> completion) {
    enqueue_task([
      this,
      snapshot,
      token,
      completion = std::move(completion)
    ]() mutable {
      completion(recovery_validation_operation_.run(snapshot, token));
    });
  }

  void AsyncDispatcher::dispatch_refresh_rate(
    std::string device_id,
    unsigned int numerator,
    unsigned int denominator,
    const CancellationToken &token,
    std::function<void(bool)> completion
  ) {
    enqueue_task([
      this,
      device_id = std::move(device_id),
      numerator,
      denominator,
      token,
      completion = std::move(completion)
    ]() mutable {
      if (token.is_cancelled()) {
        completion(false);
        return;
      }
      completion(apply_operation_.set_refresh_rate(device_id, numerator, denominator));
    });
  }

  void AsyncDispatcher::enqueue_task(std::function<void()> task) {
    {
      std::lock_guard<std::mutex> lock(mutex_);
      tasks_.push_back(std::move(task));
    }
    cv_.notify_one();
  }

  void AsyncDispatcher::enqueue_delayed_task(std::function<void(std::stop_token)> task) {
    {
      std::lock_guard<std::mutex> lock(timer_mutex_);
      timer_tasks_.push_back(std::move(task));
    }
    timer_cv_.notify_one();
  }

  void AsyncDispatcher::worker_loop(std::stop_token st) {
    while (!st.stop_requested()) {
      std::function<void()> task;
      {
        std::unique_lock<std::mutex> lock(mutex_);
        cv_.wait(lock, [&]() { return st.stop_requested() || !tasks_.empty(); });
        if (st.stop_requested()) {
          break;
        }
        task = std::move(tasks_.front());
        tasks_.pop_front();
      }

      if (task) {
        task();
      }
    }
  }

  void AsyncDispatcher::timer_loop(std::stop_token st) {
    while (!st.stop_requested()) {
      std::function<void(std::stop_token)> task;
      {
        std::unique_lock<std::mutex> lock(timer_mutex_);
        timer_cv_.wait(lock, [&]() { return st.stop_requested() || !timer_tasks_.empty(); });
        if (st.stop_requested()) {
          break;
        }
        task = std::move(timer_tasks_.front());
        timer_tasks_.pop_front();
      }
      if (task) {
        task(st);
      }
    }
  }
}  // namespace display_helper::v2
