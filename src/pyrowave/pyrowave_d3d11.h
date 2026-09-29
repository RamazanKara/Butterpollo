/**
 * @file src/pyrowave/pyrowave_d3d11.h
 * @brief PyroWave encoder reading D3D11 textures through Vulkan external memory.
 *
 * This is the only translation unit that sees the Vulkan and PyroWave headers.
 * The D3D11 side renders the frame into two shared textures (luma and chroma)
 * and signals a shared ID3D11Fence; the encoder imports all three as NT
 * handles, waits for the fence on its own Vulkan compute queue, encodes,
 * signals the fence again and packetizes the result into a PYRW container.
 */
#pragma once

// platform includes
#include <winsock2.h>
#include <d3d11.h>

// standard includes
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <vector>

namespace pyrowave {
  /// DXGI identity of the adapter that owns the shared resources.
  struct adapter_identity_t {
    LUID luid {};
    std::uint32_t vendor_id = 0;
    std::uint32_t device_id = 0;
  };

  /// Shared D3D11 resources the encoder reads. The NT handles stay owned by the
  /// caller; the encoder imports duplicates.
  struct d3d11_input_t {
    HANDLE luma = nullptr;  ///< R8_UNORM or R16_UNORM, width x height.
    HANDLE chroma = nullptr;  ///< R8G8_UNORM or R16G16_UNORM, luma extent (4:4:4) or half of it (4:2:0).
    HANDLE fence = nullptr;  ///< ID3D11Fence created with D3D11_FENCE_FLAG_SHARED.
    int width = 0;
    int height = 0;
    bool yuv444 = false;
    bool sixteen_bit = false;  ///< R16/R16G16 planes (HDR10) instead of R8/R8G8.
  };

  struct encode_timing_t {
    std::chrono::steady_clock::time_point submitted;  ///< GPU encode queued.
    std::chrono::steady_clock::time_point packetized;  ///< Bitstream read back and framed.
  };

  class d3d11_encoder_t {
  public:
    ~d3d11_encoder_t();

    d3d11_encoder_t(const d3d11_encoder_t &) = delete;
    d3d11_encoder_t &operator=(const d3d11_encoder_t &) = delete;

    /**
     * @brief Create a Vulkan device on the adapter and import the shared resources.
     * @return nullptr on failure (logged).
     */
    static std::unique_ptr<d3d11_encoder_t> create(const adapter_identity_t &adapter, const d3d11_input_t &input);

    /**
     * @brief Encode the textures as one intra frame.
     * @param acquire_value Fence value the D3D11 side signalled after rendering.
     * @param release_value Fence value signalled once the encoder no longer reads the textures.
     * @param budget_bytes Maximum bitstream size.
     * @param hdr10 Mark the container as HDR10.
     * @param container Receives the PYRW container.
     * @param timing Receives stage timestamps.
     * @return `false` on any encoder error (logged). Not thread-safe.
     */
    bool encode(
      std::uint64_t acquire_value,
      std::uint64_t release_value,
      std::size_t budget_bytes,
      bool hdr10,
      std::vector<std::uint8_t> &container,
      encode_timing_t &timing
    );

  private:
    struct impl_t;

    explicit d3d11_encoder_t(std::unique_ptr<impl_t> impl);

    std::unique_ptr<impl_t> impl;
  };
}  // namespace pyrowave
