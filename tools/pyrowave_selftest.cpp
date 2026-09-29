/**
 * @file tools/pyrowave_selftest.cpp
 * @brief GPU self-test of the PyroWave encode path against a CPU reference.
 *
 * Renders a synthetic test pattern into a D3D11 capture surface and runs it
 * through the production d3d_pyrowave_encode_device_t: colour conversion into
 * the shared luma/chroma planes, the shared fence, Vulkan import, encode and
 * PYRW framing. The container is parsed with an independent parser, decoded
 * with PyroWave's own decoder and compared with a CPU reference conversion
 * (BT.709 limited range with left-cosited 4:2:0 chroma for SDR, BT.2020 PQ
 * limited range for HDR10). It also checks the FEC frame cap and runs the
 * host's own probe path (validate_config() on a synthetic probe surface).
 *
 * Not registered with ctest because it needs a Vulkan-capable GPU. Run it from
 * the build directory, which provides assets/shaders/directx:
 *   tests/pyrowave_selftest.exe [--adapter N] [--frames N] [--skip-4k]
 */
// platform includes
#include <winsock2.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#define VK_USE_PLATFORM_WIN32_KHR
#include <vulkan/vulkan.h>

#include <pyrowave.h>

// standard includes
#include <algorithm>
#include <array>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <functional>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

// local includes
#include "src/logging.h"
#include "src/platform/common.h"
#include "src/platform/windows/display.h"
#include "src/platform/windows/display_vram.h"
#include "src/pyrowave/pyrowave_rate_control.h"
#include "src/video.h"
#include "src/video_colorspace.h"

namespace platf::dxgi {
  int init();
}

// Production probe entry points (src/video.cpp) that the host runs for PyroWave.
namespace video {
  std::shared_ptr<platf::display_t> make_synthetic_probe_display(
    platf::mem_type_e type,
    const config_t &config,
    const std::optional<platf::adapter_id_t> &required_adapter
  );
  int validate_config(std::shared_ptr<platf::display_t> disp, const encoder_t &encoder, const config_t &config);
}  // namespace video

namespace {
  using namespace std::literals;
  using clock_type = std::chrono::steady_clock;

  int failures = 0;

  void check(bool condition, const std::string &what) {
    std::printf("  [%s] %s\n", condition ? "PASS" : "FAIL", what.c_str());
    if (!condition) {
      ++failures;
    }
  }

  // ---------------------------------------------------------------- half floats

  std::uint16_t float_to_half(float value) {
    std::uint32_t bits;
    std::memcpy(&bits, &value, sizeof(bits));
    const std::uint32_t sign = (bits >> 16) & 0x8000u;
    const int exponent = static_cast<int>((bits >> 23) & 0xFF) - 127 + 15;
    std::uint32_t mantissa = bits & 0x7FFFFFu;
    if (exponent <= 0) {
      if (exponent < -10) {
        return static_cast<std::uint16_t>(sign);
      }
      mantissa |= 0x800000u;
      const int shift = 14 - exponent;
      std::uint32_t half = mantissa >> shift;
      const std::uint32_t rest = mantissa & ((1u << shift) - 1);
      const std::uint32_t halfway = 1u << (shift - 1);
      if (rest > halfway || (rest == halfway && (half & 1u))) {
        ++half;
      }
      return static_cast<std::uint16_t>(sign | half);
    }
    if (exponent >= 31) {
      return static_cast<std::uint16_t>(sign | 0x7C00u);
    }
    std::uint32_t half = (static_cast<std::uint32_t>(exponent) << 10) | (mantissa >> 13);
    const std::uint32_t rest = mantissa & 0x1FFFu;
    if (rest > 0x1000u || (rest == 0x1000u && (half & 1u))) {
      ++half;
    }
    return static_cast<std::uint16_t>(sign | half);
  }

  float half_to_float(std::uint16_t half) {
    const std::uint32_t sign = static_cast<std::uint32_t>(half & 0x8000u) << 16;
    int exponent = (half >> 10) & 0x1F;
    std::uint32_t mantissa = half & 0x3FFu;
    std::uint32_t bits;
    if (exponent == 0) {
      if (mantissa == 0) {
        bits = sign;
      } else {
        exponent = 1;
        while (!(mantissa & 0x400u)) {
          mantissa <<= 1;
          --exponent;
        }
        mantissa &= 0x3FFu;
        bits = sign | (static_cast<std::uint32_t>(exponent + 127 - 15) << 23) | (mantissa << 13);
      }
    } else if (exponent == 31) {
      bits = sign | 0x7F800000u | (mantissa << 13);
    } else {
      bits = sign | (static_cast<std::uint32_t>(exponent + 127 - 15) << 23) | (mantissa << 13);
    }
    float value;
    std::memcpy(&value, &bits, sizeof(value));
    return value;
  }

  // ---------------------------------------------------------------- test pattern

  struct rgb_t {
    double r, g, b;
  };

  /// Display-referred sRGB-encoded colour in [0, 1] (what a desktop capture delivers).
  rgb_t pattern_srgb(int x, int y, int width, int height) {
    const int band = height / 3;
    if (y < band) {
      // 75% colour bars: white, yellow, cyan, green, magenta, red, blue, black.
      static constexpr std::array<std::array<double, 3>, 8> bars {{
        {0.75, 0.75, 0.75},
        {0.75, 0.75, 0.0},
        {0.0, 0.75, 0.75},
        {0.0, 0.75, 0.0},
        {0.75, 0.0, 0.75},
        {0.75, 0.0, 0.0},
        {0.0, 0.0, 0.75},
        {0.0, 0.0, 0.0},
      }};
      const auto &bar = bars[std::min<std::size_t>(static_cast<std::size_t>(x) * 8 / static_cast<std::size_t>(width), 7)];
      return {bar[0], bar[1], bar[2]};
    }
    if (y < 2 * band) {
      // Smooth gradients.
      const double u = static_cast<double>(x) / (width - 1);
      const double v = static_cast<double>(y - band) / (band - 1);
      return {u, v, 0.5 * (1.0 - u) + 0.5 * v};
    }
    if (x < width / 2) {
      // Tinted zone plate: detail at every frequency.
      const double cx = x - width / 4.0;
      const double cy = y - (2 * band + (height - 2 * band) / 2.0);
      const double phase = (cx * cx + cy * cy) * (3.14159265358979 / (width * 1.2));
      const double luma = 0.5 + 0.45 * std::cos(phase);
      return {luma, 0.85 * luma + 0.1, 0.6 * luma + 0.2};
    }
    // Saturated two-pixel line pairs: chroma siting shows up at every edge.
    const int column = (x - width / 2) % 8;
    if (column < 2) {
      return {1.0, 0.0, 0.0};
    }
    if (column < 4) {
      return {0.0, 0.0, 1.0};
    }
    if (column < 6) {
      return {0.0, 1.0, 0.0};
    }
    return {1.0, 1.0, 1.0};
  }

  double srgb_to_linear(double v) {
    return v <= 0.04045 ? v / 12.92 : std::pow((v + 0.055) / 1.055, 2.4);
  }

  // ---------------------------------------------------------------- CPU reference

  struct planes_t {
    int width = 0;
    int height = 0;
    int chroma_width = 0;
    int chroma_height = 0;
    std::vector<double> y, cb, cr;  ///< Normalized code values (code / max code).

    void allocate(int w, int h, bool yuv444) {
      width = w;
      height = h;
      chroma_width = yuv444 ? w : w / 2;
      chroma_height = yuv444 ? h : h / 2;
      y.assign(static_cast<std::size_t>(w) * h, 0.0);
      cb.assign(static_cast<std::size_t>(chroma_width) * chroma_height, 0.0);
      cr.assign(cb.size(), 0.0);
    }
  };

  struct matrix_t {
    double kr, kb;
    double y_offset, y_range, c_offset, c_range, max_code;
  };

  constexpr matrix_t bt709_limited_8bit {0.2126, 0.0722, 16, 219, 128, 224, 255};
  constexpr matrix_t bt2020_limited_10bit {0.2627, 0.0593, 64, 876, 512, 896, 1023};

  struct ycbcr_t {
    double y, cb, cr;
  };

  ycbcr_t to_ycbcr(const rgb_t &e, const matrix_t &m) {
    const double kg = 1.0 - m.kr - m.kb;
    const double y = m.kr * e.r + kg * e.g + m.kb * e.b;
    const double cb = (e.b - y) / (2.0 * (1.0 - m.kb));
    const double cr = (e.r - y) / (2.0 * (1.0 - m.kr));
    return {
      (m.y_offset + m.y_range * y) / m.max_code,
      (m.c_offset + m.c_range * cb) / m.max_code,
      (m.c_offset + m.c_range * cr) / m.max_code,
    };
  }

  /// SMPTE ST 2084 inverse EOTF.
  double nits_to_pq(double nits) {
    constexpr double m1 = 2610.0 / 4096.0 / 4.0;
    constexpr double m2 = 2523.0 / 4096.0 * 128.0;
    constexpr double c1 = 3424.0 / 4096.0;
    constexpr double c2 = 2413.0 / 4096.0 * 32.0;
    constexpr double c3 = 2392.0 / 4096.0 * 32.0;
    const double l = std::pow(std::clamp(nits / 10000.0, 0.0, 1.0), m1);
    return std::pow((c1 + c2 * l) / (1.0 + c3 * l), m2);
  }

  /// scRGB (linear BT.709, 1.0 = 80 nits) to BT.2020 PQ-encoded R'G'B' (ITU-R BT.2087 matrix).
  rgb_t scrgb_to_pq2020(const rgb_t &c) {
    const double r = 0.6274040 * c.r + 0.3292820 * c.g + 0.0433136 * c.b;
    const double g = 0.0690970 * c.r + 0.9195400 * c.g + 0.0113612 * c.b;
    const double b = 0.0163916 * c.r + 0.0880132 * c.g + 0.8955950 * c.b;
    return {nits_to_pq(r * 80.0), nits_to_pq(g * 80.0), nits_to_pq(b * 80.0)};
  }

  enum class siting_e {
    left,  ///< MPEG-2 style: co-sited with the left luma sample, centred vertically.
    center,  ///< Centred in the 2x2 luma quad.
  };

  /**
   * @brief Reference conversion of an input image.
   * @param input Encoder input colour at a pixel (what the shader samples).
   * @param transfer Non-linear encoding applied after chroma filtering.
   */
  planes_t reference_planes(
    int width,
    int height,
    bool yuv444,
    siting_e siting,
    const std::function<rgb_t(int, int)> &input,
    const std::function<rgb_t(const rgb_t &)> &transfer,
    const matrix_t &matrix
  ) {
    planes_t planes;
    planes.allocate(width, height, yuv444);
    for (int y = 0; y < height; ++y) {
      for (int x = 0; x < width; ++x) {
        planes.y[static_cast<std::size_t>(y) * width + x] = to_ycbcr(transfer(input(x, y)), matrix).y;
      }
    }

    const auto clamped = [&](int x, int y) {
      return input(std::clamp(x, 0, width - 1), std::clamp(y, 0, height - 1));
    };
    for (int j = 0; j < planes.chroma_height; ++j) {
      for (int i = 0; i < planes.chroma_width; ++i) {
        rgb_t sum {0, 0, 0};
        const auto add = [&](const rgb_t &c, double w) {
          sum.r += c.r * w;
          sum.g += c.g * w;
          sum.b += c.b * w;
        };
        if (yuv444) {
          add(input(i, j), 1.0);
        } else {
          for (int row = 2 * j; row <= 2 * j + 1; ++row) {
            if (siting == siting_e::left) {
              add(clamped(2 * i - 1, row), 0.125);
              add(clamped(2 * i, row), 0.25);
              add(clamped(2 * i + 1, row), 0.125);
            } else {
              add(clamped(2 * i, row), 0.25);
              add(clamped(2 * i + 1, row), 0.25);
            }
          }
        }
        const auto c = to_ycbcr(transfer(sum), matrix);
        const auto index = static_cast<std::size_t>(j) * planes.chroma_width + i;
        planes.cb[index] = c.cb;
        planes.cr[index] = c.cr;
      }
    }
    return planes;
  }

  // ---------------------------------------------------------------- capture surface

  // A display_vram_t without a desktop behind it: the self-test uploads its own
  // pattern into the shared capture texture that the encode device opens.
  class pattern_display_t final: public platf::dxgi::display_vram_t {
  public:
    pattern_display_t(platf::dxgi::adapter_t adapter_in, int w, int h, bool hdr):
        hdr_ {hdr} {
      width = logical_width = env_width = env_logical_width = w;
      height = logical_height = env_height = env_logical_height = h;
      width_before_rotation = w;
      height_before_rotation = h;
      capture_format = hdr ? DXGI_FORMAT_R16G16B16A16_FLOAT : DXGI_FORMAT_B8G8R8A8_UNORM;
      next_image_id.store(0, std::memory_order_relaxed);
      adapter = std::move(adapter_in);

      const D3D_FEATURE_LEVEL levels[] {D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0};
      if (FAILED(D3D11CreateDevice(adapter.get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT, levels, 2, D3D11_SDK_VERSION, &device, &feature_level, &device_ctx))) {
        device.reset();
        return;
      }
      DXGI_ADAPTER_DESC1 desc {};
      adapter->GetDesc1(&desc);
      captured_adapter_luid = desc.AdapterLuid;
    }

    bool valid() const {
      return static_cast<bool>(device);
    }

    bool is_hdr() override {
      return hdr_;
    }

    bool get_hdr_metadata(SS_HDR_METADATA &metadata) override {
      std::memset(&metadata, 0, sizeof(metadata));
      return hdr_;
    }

    /// Upload `pixels` into the image's capture texture as the capture side would.
    bool upload(platf::img_t &img_base, const void *pixels, UINT pitch) {
      auto &img = static_cast<platf::dxgi::img_d3d_t &>(img_base);
      if (complete_img(&img, false) || img.capture_mutex->AcquireSync(0, 1000) != S_OK) {
        return false;
      }
      device_ctx->UpdateSubresource(img.capture_texture.get(), 0, nullptr, pixels, pitch, 0);
      device_ctx->Flush();
      img.capture_mutex->ReleaseSync(0);
      img.blank = false;
      return true;
    }

    platf::capture_e capture(const push_captured_image_cb_t &, const pull_free_image_cb_t &, bool *) override {
      return platf::capture_e::error;
    }

  protected:
    platf::capture_e snapshot(const pull_free_image_cb_t &, std::shared_ptr<platf::img_t> &, std::chrono::milliseconds, bool) override {
      return platf::capture_e::error;
    }

    platf::capture_e release_snapshot() override {
      return platf::capture_e::ok;
    }

  private:
    bool hdr_;
  };

  // ---------------------------------------------------------------- container

  struct parsed_container_t {
    std::uint8_t version = 0;
    std::uint8_t flags = 0;
    std::vector<std::span<const std::uint8_t>> packets;
  };

  /// Deliberately independent from src/pyrowave/pyrowave_container.cpp.
  std::optional<parsed_container_t> parse_container(const std::vector<std::uint8_t> &bytes) {
    if (bytes.size() < 8 || std::memcmp(bytes.data(), "PYRW", 4) != 0) {
      return std::nullopt;
    }
    parsed_container_t parsed;
    parsed.version = bytes[4];
    const std::size_t count = (std::size_t {bytes[5]} << 8) | bytes[6];
    parsed.flags = bytes[7];
    std::size_t pos = 8;
    for (std::size_t i = 0; i < count; ++i) {
      if (bytes.size() - pos < 4) {
        return std::nullopt;
      }
      const std::size_t length = (std::size_t {bytes[pos]} << 24) | (std::size_t {bytes[pos + 1]} << 16) |
                                 (std::size_t {bytes[pos + 2]} << 8) | bytes[pos + 3];
      pos += 4;
      if (length == 0 || bytes.size() - pos < length) {
        return std::nullopt;
      }
      parsed.packets.emplace_back(bytes.data() + pos, length);
      pos += length;
    }
    if (count == 0 || pos != bytes.size()) {
      return std::nullopt;
    }
    return parsed;
  }

  // ---------------------------------------------------------------- decoding

  struct decoded_t {
    int width = 0;
    int height = 0;
    int chroma_width = 0;
    int chroma_height = 0;
    std::vector<std::uint8_t> y, cb, cr;
  };

  class decoder_t {
  public:
    decoder_t(pyrowave_device device, int width, int height, bool yuv444):
        width {width},
        height {height},
        yuv444 {yuv444} {
      pyrowave_decoder_create_info info {};
      info.device = device;
      info.width = width;
      info.height = height;
      info.chroma = yuv444 ? PYROWAVE_CHROMA_SUBSAMPLING_444 : PYROWAVE_CHROMA_SUBSAMPLING_420;
      info.fragment_path = false;
      if (pyrowave_decoder_create(&info, &decoder) != PYROWAVE_SUCCESS) {
        decoder = nullptr;
      }
    }

    ~decoder_t() {
      if (decoder) {
        pyrowave_decoder_destroy(decoder);
      }
    }

    decoder_t(const decoder_t &) = delete;
    decoder_t &operator=(const decoder_t &) = delete;

    bool valid() const {
      return decoder != nullptr;
    }

    /// @return true if the frame was complete and decoded.
    bool decode(const parsed_container_t &container, decoded_t &out) {
      pyrowave_decoder_clear(decoder);
      for (const auto &packet : container.packets) {
        if (pyrowave_decoder_push_packet(decoder, packet.data(), packet.size()) != PYROWAVE_SUCCESS) {
          return false;
        }
      }
      if (!pyrowave_decoder_decode_is_ready(decoder, false)) {
        return false;
      }

      out.width = width;
      out.height = height;
      out.chroma_width = yuv444 ? width : width / 2;
      out.chroma_height = yuv444 ? height : height / 2;
      out.y.assign(static_cast<std::size_t>(width) * height, 0);
      out.cb.assign(static_cast<std::size_t>(out.chroma_width) * out.chroma_height, 0);
      out.cr.assign(out.cb.size(), 0);

      pyrowave_cpu_buffer buffer {};
      buffer.data[0] = out.y.data();
      buffer.data[1] = out.cb.data();
      buffer.data[2] = out.cr.data();
      buffer.row_stride_in_bytes[0] = static_cast<std::size_t>(width);
      buffer.row_stride_in_bytes[1] = buffer.row_stride_in_bytes[2] = static_cast<std::size_t>(out.chroma_width);
      buffer.plane_size_in_bytes[0] = out.y.size();
      buffer.plane_size_in_bytes[1] = buffer.plane_size_in_bytes[2] = out.cb.size();
      buffer.width = width;
      buffer.height = height;
      buffer.format = yuv444 ? PYROWAVE_CPU_BUFFER_FORMAT_YUV444P : PYROWAVE_CPU_BUFFER_FORMAT_YUV420P;
      return pyrowave_decoder_decode_cpu_buffer_synchronous(decoder, &buffer) == PYROWAVE_SUCCESS;
    }

  private:
    pyrowave_decoder decoder = nullptr;
    int width;
    int height;
    bool yuv444;
  };

  // ---------------------------------------------------------------- metrics

  struct plane_error_t {
    double psnr = 0;
    int max_error = 0;
    double mse = 0;
  };

  /// Decoded 8-bit plane against normalized reference values, over rows [row_begin, row_end).
  plane_error_t compare(const std::vector<std::uint8_t> &decoded, const std::vector<double> &reference, int width, int row_begin, int row_end) {
    plane_error_t result;
    double sum = 0;
    const auto begin = static_cast<std::size_t>(row_begin) * width;
    const auto end = static_cast<std::size_t>(row_end) * width;
    for (std::size_t i = begin; i < end; ++i) {
      const double expected = reference[i] * 255.0;
      const double diff = decoded[i] - expected;
      sum += diff * diff;
      result.max_error = std::max(result.max_error, static_cast<int>(std::lround(std::abs(diff))));
    }
    result.mse = sum / static_cast<double>(end - begin);
    result.psnr = result.mse > 0 ? 10.0 * std::log10(255.0 * 255.0 / result.mse) : 99.0;
    return result;
  }

  struct frame_error_t {
    plane_error_t y, cb, cr;
  };

  /// @param smooth Only the colour-bar and gradient bands (top two thirds).
  frame_error_t compare(const decoded_t &decoded, const planes_t &reference, bool smooth) {
    const int luma_rows = smooth ? 2 * (decoded.height / 3) : decoded.height;
    const int chroma_rows = smooth ? luma_rows * decoded.chroma_height / decoded.height : decoded.chroma_height;
    return {
      compare(decoded.y, reference.y, decoded.width, 0, luma_rows),
      compare(decoded.cb, reference.cb, decoded.chroma_width, 0, chroma_rows),
      compare(decoded.cr, reference.cr, decoded.chroma_width, 0, chroma_rows),
    };
  }

  struct stats_t {
    double avg = 0, p50 = 0, p95 = 0, max = 0;
  };

  stats_t summarize(std::vector<double> values) {
    stats_t s;
    if (values.empty()) {
      return s;
    }
    std::sort(values.begin(), values.end());
    for (const double v : values) {
      s.avg += v;
    }
    s.avg /= static_cast<double>(values.size());
    s.p50 = values[values.size() / 2];
    s.p95 = values[std::min(values.size() - 1, values.size() * 95 / 100)];
    s.max = values.back();
    return s;
  }

  double ms(clock_type::duration d) {
    return std::chrono::duration<double, std::milli>(d).count();
  }

  // ---------------------------------------------------------------- one mode

  struct selftest_mode_t {
    std::string_view name;
    int width;
    int height;
    bool yuv444;
    bool hdr;
    bool fidelity;  ///< Check the picture, not only the timing.
  };

  struct environment_t {
    platf::dxgi::adapter_t adapter;
    DXGI_ADAPTER_DESC1 adapter_desc {};
    pyrowave_device decode_device = nullptr;
    int frames = 120;
  };

  std::shared_ptr<pattern_display_t> make_display(environment_t &env, const selftest_mode_t &mode) {
    env.adapter->AddRef();
    auto display = std::make_shared<pattern_display_t>(platf::dxgi::adapter_t {env.adapter.get()}, mode.width, mode.height, mode.hdr);
    return display->valid() ? display : nullptr;
  }

  video::config_t make_config(const selftest_mode_t &mode, int bitrate_kbps, std::size_t max_frame_bytes) {
    video::config_t config {};
    config.width = mode.width;
    config.height = mode.height;
    config.framerate = 60;
    config.encodingFramerate = 60000;
    config.bitrate = bitrate_kbps;
    config.client_requested_bitrate = bitrate_kbps;
    config.slicesPerFrame = 1;
    config.encoderCscMode = 2;  // BT.709 limited, what Artemis sends.
    config.videoFormat = 3;
    config.dynamicRange = mode.hdr ? 1 : 0;
    config.chromaSamplingType = mode.yuv444 ? 1 : 0;
    config.max_frame_bytes = max_frame_bytes;
    return config;
  }

  std::unique_ptr<platf::pyrowave_encode_device_t> make_device(pattern_display_t &display, const video::config_t &config, bool hdr) {
    auto device = display.make_pyrowave_encode_device(hdr ? platf::pix_fmt_e::p010 : platf::pix_fmt_e::nv12);
    if (!device) {
      return nullptr;
    }
    // Mirrors video::make_encode_device() for PyroWave.
    auto colorspace = video::colorspace_from_client_config(config, hdr);
    if (!video::colorspace_is_hdr(colorspace)) {
      colorspace.bit_depth = 8;
    }
    device->colorspace = colorspace;
    if (!device->init_encoder(config, colorspace)) {
      return nullptr;
    }
    return device;
  }

  bool run_mode(environment_t &env, const selftest_mode_t &mode) {
    std::printf("\n=== %.*s: %dx%d %s %s ===\n", static_cast<int>(mode.name.size()), mode.name.data(), mode.width, mode.height, mode.yuv444 ? "4:4:4" : "4:2:0", mode.hdr ? "HDR10" : "SDR");
    const int failures_before = failures;

    auto display = make_display(env, mode);
    if (!display) {
      check(false, "D3D11 capture surface");
      return false;
    }

    // The pattern as uploaded, and the colour the conversion shader samples.
    std::vector<std::uint8_t> pixels;
    UINT pitch = 0;
    std::function<rgb_t(int, int)> input;
    std::function<rgb_t(const rgb_t &)> transfer;
    const matrix_t matrix = mode.hdr ? bt2020_limited_10bit : bt709_limited_8bit;
    std::vector<std::uint16_t> half_pixels;
    if (mode.hdr) {
      // 200-nit desktop white with highlights up to 1000 nits in the gradient band.
      pitch = static_cast<UINT>(mode.width * 8);
      half_pixels.resize(static_cast<std::size_t>(mode.width) * mode.height * 4);
      for (int y = 0; y < mode.height; ++y) {
        for (int x = 0; x < mode.width; ++x) {
          const auto c = pattern_srgb(x, y, mode.width, mode.height);
          const bool highlight = y >= mode.height / 3 && y < 2 * (mode.height / 3) && x >= mode.width * 3 / 4;
          const double scale = highlight ? 12.5 : 2.5;
          auto *p = &half_pixels[(static_cast<std::size_t>(y) * mode.width + x) * 4];
          p[0] = float_to_half(static_cast<float>(srgb_to_linear(c.r) * scale));
          p[1] = float_to_half(static_cast<float>(srgb_to_linear(c.g) * scale));
          p[2] = float_to_half(static_cast<float>(srgb_to_linear(c.b) * scale));
          p[3] = float_to_half(1.0f);
        }
      }
      input = [&](int x, int y) {
        const auto *p = &half_pixels[(static_cast<std::size_t>(y) * mode.width + x) * 4];
        return rgb_t {half_to_float(p[0]), half_to_float(p[1]), half_to_float(p[2])};
      };
      transfer = scrgb_to_pq2020;
    } else {
      pitch = static_cast<UINT>(mode.width * 4);
      pixels.resize(static_cast<std::size_t>(mode.width) * mode.height * 4);
      for (int y = 0; y < mode.height; ++y) {
        for (int x = 0; x < mode.width; ++x) {
          const auto c = pattern_srgb(x, y, mode.width, mode.height);
          auto *p = &pixels[(static_cast<std::size_t>(y) * mode.width + x) * 4];
          p[0] = static_cast<std::uint8_t>(std::lround(c.b * 255));
          p[1] = static_cast<std::uint8_t>(std::lround(c.g * 255));
          p[2] = static_cast<std::uint8_t>(std::lround(c.r * 255));
          p[3] = 255;
        }
      }
      input = [&](int x, int y) {
        const auto *p = &pixels[(static_cast<std::size_t>(y) * mode.width + x) * 4];
        return rgb_t {p[2] / 255.0, p[1] / 255.0, p[0] / 255.0};
      };
      transfer = [](const rgb_t &c) {
        return c;
      };
    }

    auto img = display->alloc_img();
    if (!img || !display->upload(*img, mode.hdr ? static_cast<const void *>(half_pixels.data()) : pixels.data(), pitch)) {
      check(false, "upload test pattern");
      return false;
    }

    // Near-lossless budget first so the picture checks see the conversion, not the codec.
    const int lossless_kbps = 3'000'000;
    auto device = make_device(*display, make_config(mode, lossless_kbps, 0), mode.hdr);
    if (!device) {
      check(false, "create d3d_pyrowave_encode_device_t and encoder");
      return false;
    }

    decoder_t decoder(env.decode_device, mode.width, mode.height, mode.yuv444);
    if (!decoder.valid()) {
      check(false, "create PyroWave decoder");
      return false;
    }

    const auto encode_once = [&](platf::pyrowave_encoded_frame_t &frame) {
      return device->convert(*img) == 0 && device->encode_frame(frame);
    };

    const auto expect_container = [&](const platf::pyrowave_encoded_frame_t &frame, std::string_view label) -> std::optional<parsed_container_t> {
      auto parsed = parse_container(frame.data);
      check(parsed.has_value(), std::string(label) + ": PYRW container well-formed (" + std::to_string(frame.data.size()) + " bytes)");
      if (!parsed) {
        return std::nullopt;
      }
      check(parsed->version == 1, std::string(label) + ": version 1");
      check(parsed->flags == (mode.hdr ? 0x01 : 0x00), std::string(label) + ": flags 0x0" + std::to_string(parsed->flags) + (mode.hdr ? " (HDR10)" : " (SDR)"));
      std::printf("    %llu packets\n", static_cast<unsigned long long>(parsed->packets.size()));
      return parsed;
    };

    if (mode.fidelity) {
      const auto ref_left = reference_planes(mode.width, mode.height, mode.yuv444, siting_e::left, input, transfer, matrix);
      const auto ref_center = mode.yuv444 ? ref_left : reference_planes(mode.width, mode.height, false, siting_e::center, input, transfer, matrix);

      platf::pyrowave_encoded_frame_t frame;
      if (!encode_once(frame)) {
        check(false, "encode (near-lossless)");
        return false;
      }
      const auto parsed = expect_container(frame, "near-lossless");
      decoded_t decoded;
      if (!parsed || !decoder.decode(*parsed, decoded)) {
        check(false, "decode (near-lossless)");
        return false;
      }

      // The wavelet codec is exact on the flat bars and smooth gradients at this
      // budget, so those bands test the colour conversion itself.
      const auto smooth = compare(decoded, ref_left, true);
      const auto full = compare(decoded, ref_left, false);
      std::printf("    near-lossless vs CPU reference, bars+gradients: Y max %d, Cb max %d, Cr max %d (PSNR %.2f/%.2f/%.2f dB)\n", smooth.y.max_error, smooth.cb.max_error, smooth.cr.max_error, smooth.y.psnr, smooth.cb.psnr, smooth.cr.psnr);
      std::printf("    near-lossless vs CPU reference, full frame:     Y max %d, Cb max %d, Cr max %d (PSNR %.2f/%.2f/%.2f dB)\n", full.y.max_error, full.cb.max_error, full.cr.max_error, full.y.psnr, full.cb.psnr, full.cr.psnr);
      const int tolerance = mode.hdr ? 3 : 2;
      check(smooth.y.max_error <= tolerance && smooth.cb.max_error <= tolerance && smooth.cr.max_error <= tolerance, "bars and gradients match the reference conversion within " + std::to_string(tolerance) + " code values");
      check(full.y.psnr >= 40.0 && full.cb.psnr >= 40.0 && full.cr.psnr >= 40.0, "full frame within 40 dB of the reference");

      if (!mode.yuv444) {
        const auto center = compare(decoded, ref_center, false);
        std::printf("    chroma MSE left-cosited %.3f/%.3f vs centre-sited %.3f/%.3f (Cb/Cr)\n", full.cb.mse, full.cr.mse, center.cb.mse, center.cr.mse);
        check(full.cb.mse * 4 < center.cb.mse && full.cr.mse * 4 < center.cr.mse, "chroma is left-cosited");
      }

      // Patch values: colour bars in the top band.
      const int bar_y = mode.height / 6;
      const std::array<std::string_view, 8> bar_names {"white75", "yellow75", "cyan75", "green75", "magenta75", "red75", "blue75", "black"};
      for (int bar = 0; bar < 8; bar += (mode.hdr ? 7 : 1)) {
        const int bar_x = (2 * bar + 1) * mode.width / 16;
        const auto luma_index = static_cast<std::size_t>(bar_y) * mode.width + bar_x;
        const int cx = mode.yuv444 ? bar_x : bar_x / 2;
        const int cy = mode.yuv444 ? bar_y : bar_y / 2;
        const auto chroma_index = static_cast<std::size_t>(cy) * decoded.chroma_width + cx;
        const double scale = mode.hdr ? 1023.0 : 255.0;
        std::printf("    %-9.*s expected Y/Cb/Cr %6.1f %6.1f %6.1f (%s)  decoded(8-bit) %3d %3d %3d\n", static_cast<int>(bar_names[bar].size()), bar_names[bar].data(), ref_left.y[luma_index] * scale, ref_left.cb[chroma_index] * scale, ref_left.cr[chroma_index] * scale, mode.hdr ? "10-bit" : "8-bit", decoded.y[luma_index], decoded.cb[chroma_index], decoded.cr[chroma_index]);
      }

      // Realistic LAN bitrates.
      for (const int kbps : {200'000, 500'000}) {
        device->set_bitrate(kbps);
        platf::pyrowave_encoded_frame_t lossy;
        const auto label = std::to_string(kbps / 1000) + " Mbit/s";
        decoded_t lossy_decoded;
        std::optional<parsed_container_t> lossy_parsed;
        if (!encode_once(lossy) || !(lossy_parsed = expect_container(lossy, label)) || !decoder.decode(*lossy_parsed, lossy_decoded)) {
          check(false, label + ": encode/decode");
          continue;
        }
        const auto budget = pyrowave::rate_control::frame_budget_bytes(kbps, 60);
        const auto lossy_error = compare(lossy_decoded, ref_left, false);
        std::printf("    %s: %llu bytes (budget %llu), PSNR Y %.2f / Cb %.2f / Cr %.2f dB\n", label.c_str(), static_cast<unsigned long long>(lossy.data.size()), static_cast<unsigned long long>(budget), lossy_error.y.psnr, lossy_error.cb.psnr, lossy_error.cr.psnr);
        check(lossy.data.size() <= budget + pyrowave::rate_control::container_overhead_bytes(budget), label + ": frame within budget");
        check(lossy_error.y.psnr >= 30.0, label + ": luma PSNR >= 30 dB");
      }
    }

    // Steady-state timing at 500 Mbit/s: convert + encode + packetize the same image.
    device->set_bitrate(500'000);
    std::vector<double> convert_ms, encode_ms, gpu_to_packet_ms, total_ms;
    std::size_t bytes = 0;
    for (int i = 0; i < env.frames + 10; ++i) {
      platf::pyrowave_encoded_frame_t frame;
      const auto t0 = clock_type::now();
      if (device->convert(*img)) {
        check(false, "timing: convert");
        return false;
      }
      const auto t1 = clock_type::now();
      if (!device->encode_frame(frame)) {
        check(false, "timing: encode");
        return false;
      }
      const auto t2 = clock_type::now();
      if (i < 10) {
        continue;
      }
      convert_ms.push_back(ms(t1 - t0));
      encode_ms.push_back(ms(t2 - t1));
      gpu_to_packet_ms.push_back(ms(frame.output - frame.submitted));
      total_ms.push_back(ms(t2 - t0));
      bytes = frame.data.size();
    }
    const auto c = summarize(convert_ms);
    const auto e = summarize(encode_ms);
    const auto g = summarize(gpu_to_packet_ms);
    const auto t = summarize(total_ms);
    std::printf("    timing over %d frames @500 Mbit/s (%llu-byte frames):\n", env.frames, static_cast<unsigned long long>(bytes));
    std::printf("      convert (D3D11 submit)        avg %.3f  p50 %.3f  p95 %.3f  max %.3f ms\n", c.avg, c.p50, c.p95, c.max);
    std::printf("      encode_frame (wait+packetize) avg %.3f  p50 %.3f  p95 %.3f  max %.3f ms\n", e.avg, e.p50, e.p95, e.max);
    std::printf("        of which submit->packetized avg %.3f  p50 %.3f  p95 %.3f  max %.3f ms\n", g.avg, g.p50, g.p95, g.max);
    std::printf("      convert -> container          avg %.3f  p50 %.3f  p95 %.3f  max %.3f ms\n", t.avg, t.p50, t.p95, t.max);

    return failures == failures_before;
  }

  /// The transport cap must bind for a bitrate FEC cannot protect.
  void run_fec_cap(environment_t &env) {
    std::printf("\n=== FEC cap: 1920x1080 4:2:0 SDR at 1500 Mbit/s, 1392-byte packets, 20%% FEC ===\n");
    const selftest_mode_t mode {"fec-cap", 1920, 1080, false, false, false};
    auto display = make_display(env, mode);
    if (!display) {
      check(false, "D3D11 capture surface");
      return;
    }
    std::vector<std::uint8_t> pixels(static_cast<std::size_t>(mode.width) * mode.height * 4);
    for (int y = 0; y < mode.height; ++y) {
      for (int x = 0; x < mode.width; ++x) {
        // Worst case for the budget: full-frame noise-like detail.
        auto *p = &pixels[(static_cast<std::size_t>(y) * mode.width + x) * 4];
        const auto h = (static_cast<std::uint32_t>(x) * 2654435761u) ^ (static_cast<std::uint32_t>(y) * 2246822519u);
        p[0] = static_cast<std::uint8_t>(h);
        p[1] = static_cast<std::uint8_t>(h >> 8);
        p[2] = static_cast<std::uint8_t>(h >> 16);
        p[3] = 255;
      }
    }
    auto img = display->alloc_img();
    if (!img || !display->upload(*img, pixels.data(), static_cast<UINT>(mode.width * 4))) {
      check(false, "upload noise pattern");
      return;
    }

    // stream.cpp: blocksize = packetsize + 16 (RTP header room), minus the 32-byte packet header.
    const auto limit = pyrowave::rate_control::fec_frame_limit_bytes(1392 + 16 - 32, 20, 4, 255, 8);
    auto device = make_device(*display, make_config(mode, 1'500'000, limit), false);
    platf::pyrowave_encoded_frame_t frame;
    if (!device || device->convert(*img) || !device->encode_frame(frame)) {
      check(false, "encode");
      return;
    }
    std::printf("    FEC limit %llu bytes, container %llu bytes\n", static_cast<unsigned long long>(limit), static_cast<unsigned long long>(frame.data.size()));
    check(frame.data.size() <= limit, "frame fits the FEC limit");
    check(parse_container(frame.data).has_value(), "container well-formed");
  }

  /// The host's own probe path: synthetic probe surface + validate_config() with the PyroWave encoder.
  void run_probe_path(environment_t &env) {
    std::printf("\n=== Host probe path (make_synthetic_probe_display + validate_config) ===\n");
    const platf::adapter_id_t adapter {
      .high_part = env.adapter_desc.AdapterLuid.HighPart,
      .low_part = env.adapter_desc.AdapterLuid.LowPart,
    };
    struct probe_case_t {
      std::string_view name;
      bool yuv444;
      bool hdr;
      bool expected;
    };
    const std::array cases {
      probe_case_t {"SDR 4:2:0", false, false, true},
      probe_case_t {"SDR 4:4:4", true, false, true},
      probe_case_t {"HDR10 4:2:0", false, true, true},
      // No pixel format exists for it; the encoder must refuse rather than guess.
      probe_case_t {"HDR10 4:4:4", true, true, false},
    };
    for (const auto &probe : cases) {
      const auto config = make_config(selftest_mode_t {probe.name, 1920, 1080, probe.yuv444, probe.hdr, false}, 200'000, 0);
      auto display = video::make_synthetic_probe_display(platf::mem_type_e::dxgi, config, adapter);
      const bool passed = display && video::validate_config(display, video::pyrowave, config) >= 0;
      check(passed == probe.expected, std::string(probe.name) + (probe.expected ? " validates" : " is refused"));
    }
  }
}  // namespace

int main(int argc, char **argv) {
  int adapter_index = -1;
  int frames = 120;
  bool run_4k = true;
  for (int i = 1; i < argc; ++i) {
    const std::string_view arg {argv[i]};
    if (arg == "--adapter"sv && i + 1 < argc) {
      adapter_index = std::atoi(argv[++i]);
    } else if (arg == "--frames"sv && i + 1 < argc) {
      frames = std::max(1, std::atoi(argv[++i]));
    } else if (arg == "--skip-4k"sv) {
      run_4k = false;
    } else {
      std::fprintf(stderr, "usage: %s [--adapter N] [--frames N] [--skip-4k]\n", argv[0]);
      return 2;
    }
  }

  auto log_deinit = logging::init_single_file(2, "pyrowave_selftest.log");
  setvbuf(stdout, nullptr, _IONBF, 0);

  std::uint32_t major = 0, minor = 0, patch = 0;
  pyrowave_get_api_version(&major, &minor, &patch);
  std::printf("PyroWave C API %u.%u.%u\n", major, minor, patch);

  if (platf::dxgi::init()) {
    std::printf("shader compilation failed (run from the build directory)\n");
    return 1;
  }

  environment_t env;
  env.frames = frames;
  platf::dxgi::factory1_t factory;
  if (FAILED(CreateDXGIFactory1(IID_IDXGIFactory1, reinterpret_cast<void **>(&factory)))) {
    return 1;
  }
  for (UINT i = 0;; ++i) {
    IDXGIAdapter1 *candidate = nullptr;
    if (factory->EnumAdapters1(i, &candidate) == DXGI_ERROR_NOT_FOUND) {
      break;
    }
    platf::dxgi::adapter_t owned {candidate};
    DXGI_ADAPTER_DESC1 desc {};
    owned->GetDesc1(&desc);
    if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) {
      continue;
    }
    if (adapter_index < 0 || static_cast<int>(i) == adapter_index) {
      env.adapter = std::move(owned);
      env.adapter_desc = desc;
      break;
    }
  }
  if (!env.adapter) {
    std::printf("no hardware DXGI adapter\n");
    return 1;
  }
  std::printf("adapter: %ls [%04x:%04x] LUID %ld:%lu\n", env.adapter_desc.Description, env.adapter_desc.VendorId, env.adapter_desc.DeviceId, env.adapter_desc.AdapterLuid.HighPart, env.adapter_desc.AdapterLuid.LowPart);

  if (pyrowave_create_device_by_compat(env.adapter_desc.VendorId, env.adapter_desc.DeviceId, nullptr, nullptr, nullptr, &env.decode_device) != PYROWAVE_SUCCESS) {
    std::printf("could not create a PyroWave device for decoding\n");
    return 1;
  }

  const std::array modes {
    selftest_mode_t {"sdr420", 1920, 1080, false, false, true},
    selftest_mode_t {"sdr444", 1920, 1080, true, false, true},
    selftest_mode_t {"hdr420", 1920, 1080, false, true, true},
  };
  for (const auto &mode : modes) {
    run_mode(env, mode);
  }
  run_fec_cap(env);
  run_probe_path(env);
  if (run_4k) {
    run_mode(env, selftest_mode_t {"sdr420-4k", 3840, 2160, false, false, false});
  }

  pyrowave_device_destroy(env.decode_device);
  std::printf("\n%s: %d failed check(s)\n", failures ? "FAILED" : "PASSED", failures);
  return failures ? 1 : 0;
}
