/**
 * @file src/pyrowave/pyrowave_d3d11.cpp
 * @brief PyroWave encoder reading D3D11 textures through Vulkan external memory.
 */
// this include
#include "pyrowave_d3d11.h"

// platform includes
#define VK_USE_PLATFORM_WIN32_KHR
#include <vulkan/vulkan.h>

#include <pyrowave.h>

// standard includes
#include <array>
#include <cstring>
#include <utility>

// local includes
#include "pyrowave_container.h"
#include "pyrowave_protocol.h"
#include "src/logging.h"
#include "src/utility.h"

using namespace std::literals;

namespace pyrowave {
  namespace {
    std::string_view result_name(pyrowave_result result) {
      switch (result) {
        case PYROWAVE_SUCCESS:
          return "success"sv;
        case PYROWAVE_TIMEOUT:
          return "timeout"sv;
        case PYROWAVE_ERROR_GENERIC:
          return "generic error"sv;
        case PYROWAVE_ERROR_INVALID_ARGUMENT:
          return "invalid argument"sv;
        case PYROWAVE_ERROR_OUT_OF_HOST_MEMORY:
          return "out of host memory"sv;
        case PYROWAVE_ERROR_OUT_OF_DEVICE_MEMORY:
          return "out of device memory"sv;
        case PYROWAVE_ERROR_NO_VULKAN:
          return "no matching Vulkan device"sv;
        case PYROWAVE_ERROR_NOT_IMPLEMENTED:
          return "not implemented"sv;
        case PYROWAVE_ERROR_UNSUPPORTED_EXTERNAL_HANDLE:
          return "unsupported external handle"sv;
        case PYROWAVE_ERROR_FAILED_EXTERNAL_HANDLE:
          return "external handle import failed"sv;
        default:
          return "unknown error"sv;
      }
    }

    std::string_view priority_name(VkQueueGlobalPriority priority) {
      switch (priority) {
        case VK_QUEUE_GLOBAL_PRIORITY_LOW:
          return "low"sv;
        case VK_QUEUE_GLOBAL_PRIORITY_MEDIUM:
          return "medium"sv;
        case VK_QUEUE_GLOBAL_PRIORITY_HIGH:
          return "high"sv;
        case VK_QUEUE_GLOBAL_PRIORITY_REALTIME:
          return "realtime"sv;
        default:
          return "unknown"sv;
      }
    }

    // PyroWave takes ownership of an NT handle only when an import succeeds, so
    // it always receives a duplicate and the caller keeps its own handle.
    HANDLE duplicate_handle(HANDLE handle) {
      HANDLE duplicate = nullptr;
      if (!DuplicateHandle(GetCurrentProcess(), handle, GetCurrentProcess(), &duplicate, 0, FALSE, DUPLICATE_SAME_ACCESS)) {
        BOOST_LOG(error) << "PyroWave: DuplicateHandle failed: " << GetLastError();
        return nullptr;
      }
      return duplicate;
    }

    pyrowave_result create_device_with_priority(
      std::uint32_t vendor_id,
      std::uint32_t device_id,
      const pyrowave_luid *luid,
      pyrowave_device *device
    ) {
      // High global priority keeps encoding ahead of the game's own compute work.
      // Drivers may refuse it, in which case the default priority still works.
      auto result = pyrowave_create_device_by_compat2(vendor_id, device_id, nullptr, nullptr, luid, VK_QUEUE_GLOBAL_PRIORITY_HIGH, device);
      if (result != PYROWAVE_SUCCESS) {
        result = pyrowave_create_device_by_compat(vendor_id, device_id, nullptr, nullptr, luid, device);
      }
      return result;
    }

    pyrowave_device create_device(const adapter_identity_t &adapter) {
      pyrowave_luid luid {};
      static_assert(sizeof(luid.luid) == sizeof(LUID));
      std::memcpy(luid.luid, &adapter.luid, sizeof(LUID));

      pyrowave_device device = nullptr;
      auto result = create_device_with_priority(0, 0, &luid, &device);
      if (result != PYROWAVE_SUCCESS) {
        // Virtual-display adapters (such as the Vibepollo display driver) have
        // their own DXGI LUID but no Vulkan device. Their surfaces live on the
        // physical GPU, which is found by its PCI ids instead.
        BOOST_LOG(info) << "PyroWave: no Vulkan device for adapter LUID "
                        << adapter.luid.HighPart << ':' << adapter.luid.LowPart << " (" << result_name(result)
                        << "); selecting by PCI id " << util::hex(adapter.vendor_id).to_string_view()
                        << ':' << util::hex(adapter.device_id).to_string_view();
        result = create_device_with_priority(adapter.vendor_id, adapter.device_id, nullptr, &device);
      }
      if (result != PYROWAVE_SUCCESS) {
        BOOST_LOG(error) << "PyroWave: could not create a Vulkan device: " << result_name(result);
        return nullptr;
      }

      if (!pyrowave_device_confirm_interop_support(device)) {
        BOOST_LOG(error) << "PyroWave: the Vulkan device does not support D3D11 texture and fence import";
        pyrowave_device_destroy(device);
        return nullptr;
      }

      // Encode on the async compute queue: the game keeps the graphics queue busy.
      if (pyrowave_device_set_queue_type(device, VK_QUEUE_COMPUTE_BIT) != PYROWAVE_SUCCESS) {
        BOOST_LOG(warning) << "PyroWave: async compute queue unavailable; encoding on the graphics queue";
      }
      BOOST_LOG(info) << "PyroWave: Vulkan device ready (queue priority "
                      << priority_name(pyrowave_device_get_global_priority(device)) << ')';
      return device;
    }

    pyrowave_image import_texture(pyrowave_device device, HANDLE handle, VkFormat format, int width, int height) {
      auto duplicate = duplicate_handle(handle);
      if (!duplicate) {
        return nullptr;
      }

      VkImageCreateInfo image_info {VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
      image_info.imageType = VK_IMAGE_TYPE_2D;
      image_info.format = format;
      image_info.extent = {static_cast<std::uint32_t>(width), static_cast<std::uint32_t>(height), 1};
      image_info.mipLevels = 1;
      image_info.arrayLayers = 1;
      image_info.samples = VK_SAMPLE_COUNT_1_BIT;
      image_info.tiling = VK_IMAGE_TILING_OPTIMAL;
      image_info.usage = VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT;
      image_info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
      image_info.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;

      pyrowave_image_create_info info {};
      info.device = device;
      info.external_handle = reinterpret_cast<pyrowave_os_handle>(duplicate);
      info.handle_type = VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT;
      info.image_create_info = &image_info;

      pyrowave_image image = nullptr;
      const auto result = pyrowave_image_create(&info, &image);
      if (result != PYROWAVE_SUCCESS) {
        CloseHandle(duplicate);
        BOOST_LOG(error) << "PyroWave: D3D11 texture import failed: " << result_name(result);
        return nullptr;
      }
      return image;
    }

    pyrowave_sync_object import_fence(pyrowave_device device, HANDLE handle) {
      auto duplicate = duplicate_handle(handle);
      if (!duplicate) {
        return nullptr;
      }

      pyrowave_sync_object_create_info info {};
      info.device = device;
      info.external_handle = reinterpret_cast<pyrowave_os_handle>(duplicate);
      info.handle_type = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT;
      info.semaphore_type = VK_SEMAPHORE_TYPE_TIMELINE;

      pyrowave_sync_object sync = nullptr;
      const auto result = pyrowave_sync_object_create(&info, &sync);
      if (result != PYROWAVE_SUCCESS) {
        CloseHandle(duplicate);
        BOOST_LOG(error) << "PyroWave: D3D11 fence import failed: " << result_name(result);
        return nullptr;
      }
      return sync;
    }
  }  // namespace

  struct d3d11_encoder_t::impl_t {
    pyrowave_device device = nullptr;
    pyrowave_image luma = nullptr;
    pyrowave_image chroma = nullptr;
    pyrowave_sync_object fence = nullptr;
    pyrowave_encoder encoder = nullptr;
    pyrowave_gpu_buffers buffers {};

    std::vector<pyrowave_packet> packets;
    std::vector<packet_slice_t> slices;
    std::vector<std::uint8_t> bitstream;

    impl_t() = default;
    impl_t(const impl_t &) = delete;
    impl_t &operator=(const impl_t &) = delete;

    ~impl_t() {
      // The encoder waits for the GPU before it is destroyed; everything it
      // references must outlive it, and the device must outlive everything.
      if (encoder) {
        pyrowave_encoder_destroy(encoder);
      }
      if (fence) {
        pyrowave_sync_object_destroy(fence);
      }
      if (chroma) {
        pyrowave_image_destroy(chroma);
      }
      if (luma) {
        pyrowave_image_destroy(luma);
      }
      if (device) {
        pyrowave_device_destroy(device);
      }
    }
  };

  d3d11_encoder_t::d3d11_encoder_t(std::unique_ptr<impl_t> impl):
      impl(std::move(impl)) {
  }

  d3d11_encoder_t::~d3d11_encoder_t() = default;

  std::unique_ptr<d3d11_encoder_t> d3d11_encoder_t::create(const adapter_identity_t &adapter, const d3d11_input_t &input) {
    if (!input.luma || !input.chroma || !input.fence || input.width <= 0 || input.height <= 0) {
      BOOST_LOG(error) << "PyroWave: incomplete D3D11 input";
      return nullptr;
    }

    auto impl = std::make_unique<impl_t>();
    impl->device = create_device(adapter);
    if (!impl->device) {
      return nullptr;
    }

    const auto chroma_width = input.yuv444 ? input.width : input.width / 2;
    const auto chroma_height = input.yuv444 ? input.height : input.height / 2;
    impl->luma = import_texture(impl->device, input.luma, input.sixteen_bit ? VK_FORMAT_R16_UNORM : VK_FORMAT_R8_UNORM, input.width, input.height);
    impl->chroma = import_texture(impl->device, input.chroma, input.sixteen_bit ? VK_FORMAT_R16G16_UNORM : VK_FORMAT_R8G8_UNORM, chroma_width, chroma_height);
    impl->fence = import_fence(impl->device, input.fence);
    if (!impl->luma || !impl->chroma || !impl->fence) {
      return nullptr;
    }

    // Cb and Cr are the R and G channels of the chroma texture.
    auto &planes = impl->buffers.planes;
    if (pyrowave_image_get_image_view(impl->luma, VK_IMAGE_ASPECT_PLANE_0_BIT, VK_IMAGE_USAGE_SAMPLED_BIT, &planes[0]) != PYROWAVE_SUCCESS ||
        pyrowave_image_get_image_view(impl->chroma, VK_IMAGE_ASPECT_PLANE_1_BIT, VK_IMAGE_USAGE_SAMPLED_BIT, &planes[1]) != PYROWAVE_SUCCESS ||
        pyrowave_image_get_image_view(impl->chroma, VK_IMAGE_ASPECT_PLANE_2_BIT, VK_IMAGE_USAGE_SAMPLED_BIT, &planes[2]) != PYROWAVE_SUCCESS) {
      BOOST_LOG(error) << "PyroWave: could not create plane views";
      return nullptr;
    }

    pyrowave_encoder_create_info encoder_info {};
    encoder_info.device = impl->device;
    encoder_info.width = input.width;
    encoder_info.height = input.height;
    encoder_info.chroma = input.yuv444 ? PYROWAVE_CHROMA_SUBSAMPLING_444 : PYROWAVE_CHROMA_SUBSAMPLING_420;
    const auto result = pyrowave_encoder_create(&encoder_info, &impl->encoder);
    if (result != PYROWAVE_SUCCESS) {
      BOOST_LOG(error) << "PyroWave: encoder creation failed: " << result_name(result);
      return nullptr;
    }

    return std::unique_ptr<d3d11_encoder_t>(new d3d11_encoder_t(std::move(impl)));
  }

  bool d3d11_encoder_t::encode(
    std::uint64_t acquire_value,
    std::uint64_t release_value,
    std::size_t budget_bytes,
    bool hdr10,
    std::vector<std::uint8_t> &container,
    encode_timing_t &timing
  ) {
    auto &state = *impl;

    std::array<pyrowave_gpu_external_reference, 2> images {{
      {state.luma, VK_QUEUE_FAMILY_EXTERNAL},
      {state.chroma, VK_QUEUE_FAMILY_EXTERNAL},
    }};
    const auto semaphore = pyrowave_sync_object_get_semaphore(state.fence);

    pyrowave_gpu_sync_operation acquire {};
    acquire.images = images.data();
    acquire.num_images = images.size();
    acquire.sync = {semaphore, acquire_value};

    pyrowave_gpu_sync_operation release = acquire;
    release.sync.value = release_value;

    const pyrowave_rate_control rate_control {budget_bytes};
    auto result = pyrowave_encoder_encode_gpu_synchronous(state.encoder, &acquire, &release, &state.buffers, &rate_control);
    timing.submitted = std::chrono::steady_clock::now();
    if (result != PYROWAVE_SUCCESS) {
      BOOST_LOG(error) << "PyroWave: encode failed: " << result_name(result);
      return false;
    }

    // Everything below waits for the GPU to finish this frame.
    std::size_t packet_count = 0;
    result = pyrowave_encoder_compute_num_packets(state.encoder, PACKET_BOUNDARY, &packet_count);
    if (result != PYROWAVE_SUCCESS || packet_count == 0) {
      BOOST_LOG(error) << "PyroWave: packet count query failed: " << result_name(result);
      return false;
    }

    // Packetizing copies coded blocks out of the encoder's bitstream buffer and
    // prepends an 8-byte sequence header, so that buffer's size bounds the output.
    const void *raw_bitstream = nullptr;
    const void *raw_metadata = nullptr;
    std::size_t raw_bitstream_size = 0;
    std::size_t raw_metadata_size = 0;
    result = pyrowave_encoder_get_mapped_raw_bitstream(state.encoder, &raw_bitstream, &raw_bitstream_size, &raw_metadata, &raw_metadata_size);
    if (result != PYROWAVE_SUCCESS) {
      BOOST_LOG(error) << "PyroWave: bitstream query failed: " << result_name(result);
      return false;
    }
    const auto output_size = raw_bitstream_size + 64;
    if (state.bitstream.size() < output_size) {
      state.bitstream.resize(output_size);
    }

    state.packets.resize(packet_count);
    std::size_t written = 0;
    result = pyrowave_encoder_packetize(state.encoder, state.packets.data(), PACKET_BOUNDARY, &written, state.bitstream.data(), state.bitstream.size());
    if (result != PYROWAVE_SUCCESS || written == 0 || written > packet_count) {
      BOOST_LOG(error) << "PyroWave: packetization failed: " << result_name(result);
      return false;
    }

    state.slices.resize(written);
    for (std::size_t i = 0; i < written; ++i) {
      state.slices[i] = {state.packets[i].offset, state.packets[i].size};
    }
    const bool framed = write_container(state.bitstream, state.slices, hdr10, container);
    timing.packetized = std::chrono::steady_clock::now();
    if (!framed) {
      BOOST_LOG(error) << "PyroWave: produced packets that cannot be framed (" << written << " packets)";
      return false;
    }
    return true;
  }
}  // namespace pyrowave
