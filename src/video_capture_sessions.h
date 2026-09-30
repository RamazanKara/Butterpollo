#pragma once

#include <algorithm>
#include <chrono>
#include <memory>
#include <utility>
#include <vector>

namespace video::capture_sessions {
  // Drain without waiting, then acknowledge every stopped peer even when the
  // first client is still waiting for an unavailable capture display.
  template<typename Session, typename Queue>
  bool admit_and_prune_sessions(std::vector<std::unique_ptr<Session>> &sessions, Queue &queue) {
    while (auto pending = queue.pop(std::chrono::milliseconds {0})) {
      sessions.emplace_back(std::make_unique<Session>(std::move(*pending)));
    }
    std::erase_if(sessions, [](const auto &session) {
      if (!session->shutdown_event->peek()) {
        return false;
      }
      session->join_event->raise(true);
      return true;
    });
    return !sessions.empty();
  }
}  // namespace video::capture_sessions
