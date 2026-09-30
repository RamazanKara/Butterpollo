#pragma once

#include <cstdint>

extern "C" {
#include <moonlight-common-c/src/Input.h>
}

namespace input {
  enum class batch_result_e {
    batched,  ///< This entry was batched with the source entry
    not_batchable,  ///< Not eligible to batch but continue attempts to batch
    terminate_batch,  ///< Stop trying to batch with this entry
  };

  // Both packets must have passed input packet validation. Only dest is modified.
  batch_result_e batch(PNV_INPUT_HEADER dest, PNV_INPUT_HEADER src);
}  // namespace input
