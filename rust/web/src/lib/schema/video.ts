// Settings for the categories named in settings-schema.ts: video and encoders.
import type { ConfigValue } from '../api';
import type { Setting } from '../settings-types';

type Values = Record<string, ConfigValue>;

/** A text setting as the host reads it: unset or blank means the default. */
function read(values: Values, key: string, fallback: string): string {
  const value = values[key];
  if (value === undefined || value === null) return fallback;
  const text = String(value).trim();
  return text === '' ? fallback : text;
}

/** A boolean setting, accepting the spellings the host accepts. */
function flag(values: Values, key: string, fallback: boolean): boolean {
  const value = values[key];
  if (typeof value === 'boolean') return value;
  if (value === undefined || value === null) return fallback;
  switch (String(value).trim().replace(/^"(.*)"$/, '$1').toLowerCase()) {
    case 'true':
    case 'yes':
    case '1':
    case 'enable':
    case 'enabled':
    case 'on':
      return true;
    case 'false':
    case 'no':
    case '0':
    case 'disable':
    case 'disabled':
    case 'off':
      return false;
    default:
      return fallback;
  }
}

const capture = (values: Values) => read(values, 'capture', 'auto').toLowerCase();
const gridPacing = (values: Values) =>
  ['grid', 'fixed'].includes(read(values, 'frame_pacing', 'arrival').toLowerCase());

/** The encoder the host resolves `encoder` to, including names from older hosts. */
function encoder(values: Values): string {
  const name = read(values, 'encoder', 'auto');
  switch (name) {
    case 'amdvce':
    case 'amdvce_experimental':
    case 'amdvce_ffmpeg':
    case 'amdvce_legacy':
      return 'amf';
    case 'nvenc_experimental':
      return 'nvenc';
    case 'amf':
    case 'nvenc':
    case 'nvenc_legacy':
    case 'quicksync':
    case 'qsv':
    case 'software':
      return name;
    default:
      return 'auto';
  }
}
const nvenc = (values: Values) => ['auto', 'nvenc', 'nvenc_legacy'].includes(encoder(values));
const amf = (values: Values) => ['auto', 'amf'].includes(encoder(values));
const qsv = (values: Values) => ['qsv', 'quicksync'].includes(encoder(values));
const software = (values: Values) => encoder(values) === 'software';

const video: Setting[] = [
  {
    key: 'capture',
    label: 'Capture method',
    description:
      'Automatic uses Desktop Duplication, or Windows Graphics Capture on a virtual display when the stream uses VRR or game frame generation. Choose a method only to work around a capture problem.',
    category: 'video',
    group: 'Capture',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Automatic' },
        { value: 'ddx', label: 'Desktop Duplication' },
        { value: 'wgc', label: 'Windows Graphics Capture' },
      ],
    },
    default: 'auto',
  },
  {
    key: 'capture_poll_interval_us',
    label: 'Capture poll interval',
    description:
      'How long the capture thread waits before checking again when no new frame is ready. Shorter waits notice new frames sooner and use more CPU time.',
    category: 'video',
    group: 'Capture',
    control: { kind: 'number', min: 100, max: 1000, step: 50, unit: 'µs' },
    default: 500,
    advanced: true,
  },
  {
    key: 'capture_predictive_poll',
    label: 'Predictive polling',
    description:
      'With Desktop Duplication, checks for frames more often around the time the next one is expected, based on the recent frame rhythm. Can pick up frames sooner from games with a steady frame rate.',
    category: 'video',
    group: 'Capture',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: (values) => !['wgc', 'wgcc'].includes(capture(values)),
    advanced: true,
  },
  {
    key: 'wgc_slot_aligned_publish',
    label: 'Align capture to encode slots',
    description:
      'With Windows Graphics Capture, holds a new frame until just before the next encode slot when a newer frame could arrive first. Helps when the display refreshes much faster than the stream; VRR streams ignore it.',
    category: 'video',
    group: 'Capture',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: (values) => !['ddx', 'dxgi'].includes(capture(values)),
    advanced: true,
  },
  {
    key: 'wgc_direct_encoder_input',
    label: 'Encode from GPU memory',
    description:
      'Hands captured frames straight to the encoder on the GPU. Turn off only to troubleshoot an encoder: frames are then copied through system memory, which adds latency and CPU load.',
    category: 'video',
    group: 'Capture',
    control: { kind: 'toggle' },
    default: true,
    advanced: true,
  },
  {
    key: 'gpu_compute_conversion',
    label: 'Copy and convert on a compute queue',
    description:
      'On supported AMD GPUs, copies Desktop Duplication and Windows Graphics Capture frames and converts their colours on compute queues beside a game. Unsupported capture textures fall back to the graphics queue. Turn off to compare or troubleshoot.',
    category: 'video',
    group: 'Capture',
    control: { kind: 'toggle' },
    default: true,
    advanced: true,
  },
  {
    key: 'hevc_mode',
    label: 'HEVC support',
    description:
      'Which HEVC profiles clients may request. A profile the encoder fails when the host starts is never offered.',
    category: 'video',
    group: 'Codecs',
    control: {
      kind: 'select',
      options: [
        { value: '0', label: 'Automatic' },
        { value: '1', label: 'Off' },
        { value: '2', label: 'SDR only' },
        { value: '3', label: 'SDR and HDR' },
      ],
    },
    default: 0,
    restart: true,
  },
  {
    key: 'av1_mode',
    label: 'AV1 support',
    description:
      'Which AV1 profiles clients may request. A profile the encoder fails when the host starts is never offered.',
    category: 'video',
    group: 'Codecs',
    control: {
      kind: 'select',
      options: [
        { value: '0', label: 'Automatic' },
        { value: '1', label: 'Off' },
        { value: '2', label: 'SDR only' },
        { value: '3', label: 'SDR and HDR' },
      ],
    },
    default: 0,
    restart: true,
  },
  {
    key: 'pyrowave',
    label: 'PyroWave',
    description:
      'Lets compatible clients request PyroWave, a GPU codec that encodes each frame on its own with very low latency. It needs hundreds of Mbps for a clean picture, so use it on wired networks.',
    category: 'video',
    group: 'Codecs',
    control: { kind: 'toggle' },
    default: true,
    restart: true,
  },
  {
    key: 'prefer_sdr_10bit',
    label: '10-bit SDR instead of HDR',
    description:
      'When a client asks for HDR, keeps the display in SDR and sends 10-bit SDR video, which reduces banding without switching the screen to HDR. Not applied while RTX HDR converts the stream; devices and apps can override it.',
    category: 'video',
    group: 'Codecs',
    control: { kind: 'toggle' },
    default: false,
  },
  {
    key: 'frame_pacing',
    label: 'Frame pacing',
    description:
      'On arrival encodes each new frame as soon as it is captured, never faster than the stream rate. Fixed grid encodes the newest frame at fixed intervals; VRR streams ignore this setting.',
    category: 'video',
    group: 'Frame pacing',
    control: {
      kind: 'select',
      options: [
        { value: 'arrival', label: 'On arrival' },
        { value: 'grid', label: 'Fixed grid' },
      ],
    },
    default: 'arrival',
  },
  {
    key: 'wgc_pacing_smoothing',
    label: 'Steady grid timing',
    description:
      'Keeps the fixed grid on a steady schedule. When off, each interval starts from the previous encode, so one late frame delays the ones after it.',
    category: 'video',
    group: 'Frame pacing',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: gridPacing,
  },
  {
    key: 'minimum_fps_target',
    label: 'Minimum frame rate',
    description:
      'How often an unchanged picture is sent again; lower values save bandwidth on a still desktop. 0 uses a fifth of the stream rate, at least 10, and PyroWave streams resend at the full rate unless this is set.',
    category: 'video',
    group: 'Frame pacing',
    control: { kind: 'number', min: 0, max: 1000, step: 0.1, unit: 'fps' },
    default: 20,
  },
  {
    key: 'limit_framerate',
    label: 'Use the launch frame rate',
    description:
      'Encodes at the frame rate the client asked for when it launched the app rather than the rate in its stream request. If the stream request is twice that rate or more, the bitrate is raised to match.',
    category: 'video',
    group: 'Frame pacing',
    control: { kind: 'toggle' },
    default: true,
  },
  {
    key: 'max_bitrate',
    label: 'Maximum bitrate',
    description: "Caps the bitrate a client can request. 0 uses the client's bitrate.",
    category: 'video',
    group: 'Bitrate and network',
    control: { kind: 'number', min: 0, step: 1, unit: 'Kbps' },
    default: 0,
  },
  {
    key: 'fec_percentage',
    label: 'Error correction',
    description:
      'Recovery packets sent with each video frame, as a share of its data packets. Higher values survive more packet loss but leave less of the bitrate for the picture; PyroWave does not use it.',
    category: 'video',
    group: 'Bitrate and network',
    control: { kind: 'number', min: 0, max: 100, step: 1, unit: '%' },
    default: 20,
  },
  {
    key: 'pyrowave_critical_fec_percentage',
    label: 'PyroWave error correction',
    description:
      'Recovery packets for the start of each PyroWave frame, which holds its coarsest detail; losing it drops the whole frame. 0 turns it off, and only clients that support it use it.',
    category: 'video',
    group: 'Bitrate and network',
    control: { kind: 'number', min: 0, max: 255, step: 1, unit: '%' },
    default: 20,
    visibleWhen: (values) => flag(values, 'pyrowave', true),
  },
  {
    key: 'packetsize',
    label: 'Packet size',
    description:
      'Largest video packet the host sends, from 256 to 1400 bytes. 0 uses the size the client asks for; lower it if your network drops large packets.',
    category: 'video',
    group: 'Bitrate and network',
    control: { kind: 'number', min: 0, max: 1400, step: 1, unit: 'bytes' },
    default: 0,
  },
  {
    key: 'video_max_batch_size_kb',
    label: 'Send batch size',
    description:
      'Largest burst of video packets sent at once. Smaller batches can help switches, routers or Wi-Fi that drop bursts, at the cost of under a millisecond of delay.',
    category: 'video',
    group: 'Bitrate and network',
    control: {
      kind: 'select',
      options: [
        { value: '64', label: '64 KiB' },
        { value: '32', label: '32 KiB' },
        { value: '16', label: '16 KiB' },
      ],
    },
    default: 64,
  },
  {
    key: 'pacing_max_bitrate_kbps',
    label: 'Pacing rate',
    description:
      "Spreads each frame's packets so they leave no faster than this rate. 0 uses up to 800 Mbps. A known Ethernet link limits pacing to 80% of its speed. A set rate is raised to at least 1.1 times the stream bitrate, within that link limit.",
    category: 'video',
    group: 'Bitrate and network',
    control: { kind: 'number', min: 0, step: 1, unit: 'Kbps' },
    default: 0,
    advanced: true,
  },
];

const encoders: Setting[] = [
  {
    key: 'encoder',
    label: 'Encoder',
    description:
      'Automatic uses NVENC on NVIDIA GPUs and AMF on AMD Radeon GPUs, then tries Quick Sync and finally software encoding. A chosen hardware encoder must be on the GPU that captures the display; PyroWave streams always use their own encoder.',
    category: 'encoders',
    group: 'Encoder',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Automatic' },
        { value: 'nvenc', label: 'NVIDIA NVENC' },
        { value: 'nvenc_legacy', label: 'NVIDIA NVENC (FFmpeg)' },
        { value: 'amf', label: 'AMD AMF' },
        { value: 'quicksync', label: 'Intel Quick Sync' },
        { value: 'software', label: 'Software' },
        { value: 'pyrowave', label: 'PyroWave (automatic for other codecs)' },
      ],
    },
    default: 'auto',
    restart: true,
  },
  {
    key: 'adapter_name',
    label: 'GPU',
    description:
      'The GPU that captures and encodes the stream. Automatic uses the GPU the streamed display is connected to.',
    category: 'encoders',
    group: 'Encoder',
    control: { kind: 'adapter' },
    default: '',
  },
  {
    key: 'adapter_pnp_id',
    label: 'GPU device ID',
    description:
      'Chooses the GPU by its Windows device instance ID instead of its name, for systems with two identical GPUs. When set, it takes precedence over the GPU name.',
    category: 'encoders',
    group: 'Encoder',
    control: { kind: 'text', placeholder: 'PCI\\VEN_10DE&DEV_2684&...', mono: true },
    default: '',
    advanced: true,
  },
];

const nvidia: Setting[] = [
  {
    key: 'nvenc_preset',
    label: 'Preset',
    description:
      'Higher presets compress better at the same bitrate but take longer to encode each frame. Raise it only when the network or the client limits the bitrate.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: {
      kind: 'select',
      options: [
        { value: '1', label: 'P1 (fastest)' },
        { value: '2', label: 'P2' },
        { value: '3', label: 'P3' },
        { value: '4', label: 'P4' },
        { value: '5', label: 'P5' },
        { value: '6', label: 'P6' },
        { value: '7', label: 'P7 (slowest)' },
      ],
    },
    default: 1,
    visibleWhen: nvenc,
  },
  {
    key: 'nvenc_twopass',
    label: 'Multi-pass encoding',
    description:
      'A first pass over each frame spreads bits better and keeps frames within the bitrate. Turning it off can cause bitrate spikes and packet loss.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: {
      kind: 'select',
      options: [
        { value: 'disabled', label: 'Off' },
        { value: 'quarter_res', label: 'Quarter resolution' },
        { value: 'full_res', label: 'Full resolution' },
      ],
    },
    default: 'quarter_res',
    visibleWhen: nvenc,
  },
  {
    key: 'nvenc_spatial_aq',
    label: 'Spatial adaptive quantization',
    description:
      'Gives flat areas of the picture fewer bits and detailed areas more. Can help at low bitrates.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: nvenc,
  },
  {
    key: 'nvenc_temporal_aq',
    label: 'Temporal adaptive quantization',
    description:
      'Spends more bits on parts of the picture that stay the same across frames. Ignored on GPUs that do not support it.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: nvenc,
  },
  {
    key: 'nvenc_vbv_increase',
    label: 'Frame size headroom',
    description:
      'Lets a single frame exceed the average frame size by this much, which helps busy scenes. Can cause packet loss on networks without buffer headroom; 400 allows frames five times the average.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'number', min: 0, max: 400, step: 1, unit: '%' },
    default: 0,
    visibleWhen: nvenc,
  },
  {
    key: 'nvenc_h264_cavlc',
    label: 'Use CAVLC for H.264',
    description:
      'Uses the simpler CAVLC entropy coding instead of CABAC. It needs about 10% more bitrate for the same quality and only very old decoders need it.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: nvenc,
  },
  {
    key: 'nvenc_split_encode',
    label: 'Split-frame encoding',
    description:
      'Splits each HEVC or AV1 frame across several encoder engines on GPUs that have them. Automatic lets the driver decide, which it usually does at 4K and above.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Automatic' },
        { value: 'enabled', label: 'On' },
        { value: 'disabled', label: 'Off' },
      ],
    },
    default: 'auto',
    visibleWhen: nvenc,
  },
  {
    key: 'nvenc_realtime_hags',
    label: 'Realtime GPU priority with HAGS',
    description:
      'With hardware-accelerated GPU scheduling on, the host runs at realtime GPU priority. Turn off if the encoder freezes when video memory is nearly full; the host then uses high priority.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: nvenc,
  },
  {
    key: 'nvenc_latency_over_power',
    label: 'Prefer latency over power saving',
    description:
      'Sets the NVIDIA driver to prefer maximum performance for the host while streaming, so power saving does not slow encoding. The driver setting is restored after the last stream.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: nvenc,
  },
  {
    key: 'nvenc_opengl_vulkan_on_dxgi',
    label: 'Present OpenGL and Vulkan through DXGI',
    description:
      'Sets the NVIDIA driver to present OpenGL and Vulkan games through DXGI while streaming, so full-screen games can be captured at their full frame rate. The driver setting is restored after the last stream.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'toggle' },
    default: true,
    visibleWhen: nvenc,
  },
  {
    key: 'nvenc_weighted_prediction',
    label: 'Weighted prediction',
    description:
      'Improves quality during fades and brightness changes on GPUs that support it, but turns off split-frame encoding for HEVC. Not used by NVENC (FFmpeg).',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: nvenc,
    advanced: true,
  },
  {
    key: 'nvenc_enable_min_qp',
    label: 'Minimum quantizer',
    description:
      'Stops the encoder from spending bits beyond a set quality level on easy scenes, leaving headroom for hard ones. Not used by NVENC (FFmpeg).',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: nvenc,
    advanced: true,
  },
  {
    key: 'nvenc_min_qp_h264',
    label: 'Minimum QP for H.264',
    description: 'The lowest quantizer H.264 frames may use. Higher values cap quality sooner.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'number', min: 0, max: 51, step: 1 },
    default: 19,
    visibleWhen: (values) => nvenc(values) && flag(values, 'nvenc_enable_min_qp', false),
    advanced: true,
  },
  {
    key: 'nvenc_min_qp_hevc',
    label: 'Minimum QP for HEVC',
    description: 'The lowest quantizer HEVC frames may use. Higher values cap quality sooner.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'number', min: 0, max: 51, step: 1 },
    default: 23,
    visibleWhen: (values) => nvenc(values) && flag(values, 'nvenc_enable_min_qp', false),
    advanced: true,
  },
  {
    key: 'nvenc_min_qp_av1',
    label: 'Minimum QP for AV1',
    description: 'The lowest quantizer AV1 frames may use, from 0 to 255. Higher values cap quality sooner.',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'number', min: 0, max: 255, step: 1 },
    default: 23,
    visibleWhen: (values) => nvenc(values) && flag(values, 'nvenc_enable_min_qp', false),
    advanced: true,
  },
  {
    key: 'nvenc_insert_filler_data',
    label: 'Insert filler data',
    description:
      'Pads frames so the stream holds a constant bitrate even when the picture is simple, which uses more bandwidth. Not used by NVENC (FFmpeg).',
    category: 'encoders',
    group: 'NVIDIA NVENC',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: nvenc,
    advanced: true,
  },
];

/** AMF properties the host can leave to the driver. */
const tristate = [
  { value: 'auto', label: 'Driver default' },
  { value: 'enabled', label: 'On' },
  { value: 'disabled', label: 'Off' },
];

const amd: Setting[] = [
  {
    key: 'amd_usage',
    label: 'Usage',
    description:
      'The base encoding profile; the settings below override parts of it. Profiles other than the low-latency ones add delay.',
    category: 'encoders',
    group: 'AMD AMF',
    control: {
      kind: 'select',
      options: [
        { value: 'ultralowlatency', label: 'Ultra low latency' },
        { value: 'lowlatency', label: 'Low latency' },
        { value: 'lowlatency_high_quality', label: 'Low latency, high quality' },
        { value: 'high_quality', label: 'High quality' },
        { value: 'webcam', label: 'Webcam' },
        { value: 'transcoding', label: 'Transcoding' },
        { value: 'auto', label: 'Driver default' },
      ],
    },
    default: 'ultralowlatency',
    visibleWhen: amf,
  },
  {
    key: 'amd_quality',
    label: 'Quality preset',
    description:
      'Trades encoding speed for picture quality. Driver default lets the usage profile decide.',
    category: 'encoders',
    group: 'AMD AMF',
    control: {
      kind: 'select',
      options: [
        { value: 'speed', label: 'Speed' },
        { value: 'balanced', label: 'Balanced' },
        { value: 'quality', label: 'Quality' },
        { value: 'auto', label: 'Driver default' },
      ],
    },
    default: 'speed',
    visibleWhen: amf,
  },
  {
    key: 'amd_rc',
    label: 'Rate control',
    description:
      'How the encoder keeps to the stream bitrate; constant QP ignores it. The quality modes turn on pre-analysis, and HDR streams use peak-constrained VBR instead of them.',
    category: 'encoders',
    group: 'AMD AMF',
    control: {
      kind: 'select',
      options: [
        { value: 'vbr_latency', label: 'Latency-constrained VBR' },
        { value: 'vbr_peak', label: 'Peak-constrained VBR' },
        { value: 'cbr', label: 'Constant bitrate' },
        { value: 'qvbr', label: 'Quality VBR' },
        { value: 'hqvbr', label: 'High-quality VBR' },
        { value: 'hqcbr', label: 'High-quality CBR' },
        { value: 'cqp', label: 'Constant QP' },
        { value: 'auto', label: 'Driver default' },
      ],
    },
    default: 'vbr_latency',
    visibleWhen: amf,
  },
  {
    key: 'amd_qvbr_quality_level',
    label: 'Quality VBR level',
    description:
      'Target quality for quality VBR, from 1 (lowest) to 51 (highest). 0 keeps the driver default.',
    category: 'encoders',
    group: 'AMD AMF',
    control: { kind: 'number', min: 0, max: 51, step: 1 },
    default: 0,
    visibleWhen: (values) => amf(values) && read(values, 'amd_rc', 'vbr_latency') === 'qvbr',
  },
  {
    key: 'amd_preanalysis',
    label: 'Pre-analysis',
    description:
      'Analyzes each frame before encoding to improve rate control, at the cost of extra latency. Not used for HDR streams.',
    category: 'encoders',
    group: 'AMD AMF',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: amf,
  },
  {
    key: 'amd_vbaq',
    label: 'Variance-based adaptive quantization',
    description:
      'Gives more bits to smooth areas, where artifacts are easiest to see, and fewer to busy textures. Not used with constant QP.',
    category: 'encoders',
    group: 'AMD AMF',
    control: { kind: 'select', options: tristate },
    default: 'enabled',
    visibleWhen: amf,
  },
  {
    key: 'amd_enforce_hrd',
    label: 'Enforce HRD',
    description:
      'Holds rate control to the decoder buffer model, which greatly reduces bitrate spikes. Can cause artifacts or lower quality on some GPUs.',
    category: 'encoders',
    group: 'AMD AMF',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: amf,
  },
  {
    key: 'amd_coder',
    label: 'H.264 entropy coding',
    description: 'CABAC gives better quality per bit; CAVLC is simpler to decode. H.264 only.',
    category: 'encoders',
    group: 'AMD AMF',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Driver default' },
        { value: 'cabac', label: 'CABAC' },
        { value: 'cavlc', label: 'CAVLC' },
      ],
    },
    default: 'auto',
    visibleWhen: amf,
  },
  {
    key: 'amd_input_queue_size',
    label: 'Input queue size',
    description:
      'How many frames may wait for the encoder. 0 uses the driver default, or 1 for VRR streams.',
    category: 'encoders',
    group: 'AMD AMF',
    control: { kind: 'number', min: 0, max: 32, step: 1, unit: 'frames' },
    default: 0,
    visibleWhen: amf,
  },
  {
    key: 'amd_ltr_frames',
    label: 'Long-term reference frames',
    description:
      'Frames the encoder keeps as long-term references. With 1 or more, the host can recover from packet loss without sending a full keyframe.',
    category: 'encoders',
    group: 'AMD AMF',
    control: { kind: 'number', min: 0, max: 4, step: 1, unit: 'frames' },
    default: 0,
    visibleWhen: amf,
  },
  {
    key: 'amd_lowlatency_mode',
    label: 'Low-latency mode',
    description: "Turns the encoder's internal low-latency mode on or off for H.264 and HEVC.",
    category: 'encoders',
    group: 'AMD AMF',
    control: { kind: 'select', options: tristate },
    default: 'auto',
    visibleWhen: amf,
  },
  {
    key: 'amd_high_motion_quality_boost',
    label: 'High-motion quality boost',
    description: 'Improves quality in fast motion on drivers that support it.',
    category: 'encoders',
    group: 'AMD AMF',
    control: { kind: 'select', options: tristate },
    default: 'auto',
    visibleWhen: amf,
  },
  {
    key: 'amd_smart_access_video',
    label: 'SmartAccess Video',
    description:
      'Lets the driver share encoding between AMD integrated and discrete graphics on systems that support SmartAccess Video.',
    category: 'encoders',
    group: 'AMD AMF',
    control: { kind: 'select', options: tristate },
    default: 'auto',
    visibleWhen: amf,
  },
  {
    key: 'amd_av1_latency_mode',
    label: 'AV1 latency mode',
    description: 'How quickly each AV1 frame is finished. Lower latency uses more power.',
    category: 'encoders',
    group: 'AMD AMF',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Driver default' },
        { value: 'none', label: 'Balance latency and power' },
        { value: 'power_saving', label: 'Real time, power saving' },
        { value: 'realtime', label: 'Real time' },
        { value: 'lowest', label: 'Lowest latency' },
      ],
    },
    default: 'auto',
    visibleWhen: amf,
  },
  {
    key: 'amd_av1_screen_content',
    label: 'AV1 screen content tools',
    description:
      'AV1 coding tools for desktop content, which can make text and interface edges clearer. AV1 only.',
    category: 'encoders',
    group: 'AMD AMF',
    control: { kind: 'select', options: tristate },
    default: 'auto',
    visibleWhen: amf,
  },
  {
    key: 'amd_av1_tiles',
    label: 'AV1 tiles',
    description: "Tiles per AV1 frame. Automatic follows the client's slice request, up to 4.",
    category: 'encoders',
    group: 'AMD AMF',
    control: {
      kind: 'select',
      options: [
        { value: '0', label: 'Automatic' },
        { value: '1', label: '1' },
        { value: '2', label: '2' },
        { value: '4', label: '4' },
      ],
    },
    default: 0,
    visibleWhen: amf,
    advanced: true,
  },
];

const intel: Setting[] = [
  {
    key: 'qsv_preset',
    label: 'Preset',
    description:
      'Slower presets give better quality at the same bitrate and take longer to encode each frame.',
    category: 'encoders',
    group: 'Intel Quick Sync',
    control: {
      kind: 'select',
      options: [
        { value: 'veryfast', label: 'Very fast' },
        { value: 'faster', label: 'Faster' },
        { value: 'fast', label: 'Fast' },
        { value: 'medium', label: 'Medium' },
        { value: 'slow', label: 'Slow' },
        { value: 'slower', label: 'Slower' },
        { value: 'veryslow', label: 'Very slow' },
      ],
    },
    default: 'medium',
    visibleWhen: qsv,
  },
  {
    key: 'qsv_coder',
    label: 'H.264 entropy coding',
    description: 'CABAC gives better quality per bit; CAVLC is simpler to decode. H.264 only.',
    category: 'encoders',
    group: 'Intel Quick Sync',
    control: {
      kind: 'select',
      options: [
        { value: 'auto', label: 'Automatic' },
        { value: 'cabac', label: 'CABAC' },
        { value: 'cavlc', label: 'CAVLC' },
      ],
    },
    default: 'auto',
    visibleWhen: qsv,
  },
  {
    key: 'qsv_slow_hevc',
    label: 'Allow slow HEVC encoding',
    description:
      'Uses the general-purpose HEVC encoder instead of the low-power one. This enables HEVC on older Intel GPUs at the cost of higher GPU load.',
    category: 'encoders',
    group: 'Intel Quick Sync',
    control: { kind: 'toggle' },
    default: false,
    visibleWhen: qsv,
  },
];

const cpu: Setting[] = [
  {
    key: 'sw_preset',
    label: 'Preset',
    description:
      'Faster presets use less CPU; slower ones give better quality at the same bitrate. Applies to H.264, HEVC and AV1.',
    category: 'encoders',
    group: 'Software',
    control: {
      kind: 'select',
      options: [
        { value: 'ultrafast', label: 'Ultra fast' },
        { value: 'superfast', label: 'Super fast' },
        { value: 'veryfast', label: 'Very fast' },
        { value: 'faster', label: 'Faster' },
        { value: 'fast', label: 'Fast' },
        { value: 'medium', label: 'Medium' },
        { value: 'slow', label: 'Slow' },
        { value: 'slower', label: 'Slower' },
        { value: 'veryslow', label: 'Very slow' },
      ],
    },
    default: 'superfast',
    visibleWhen: software,
  },
  {
    key: 'sw_tune',
    label: 'Tuning',
    description:
      'Adjusts the H.264 and HEVC encoders for a kind of content. Zero latency suits streaming; film and still image work with H.264 only.',
    category: 'encoders',
    group: 'Software',
    control: {
      kind: 'select',
      options: [
        { value: 'zerolatency', label: 'Zero latency' },
        { value: 'film', label: 'Film' },
        { value: 'animation', label: 'Animation' },
        { value: 'grain', label: 'Grain' },
        { value: 'stillimage', label: 'Still image' },
        { value: 'fastdecode', label: 'Fast decode' },
      ],
    },
    default: 'zerolatency',
    visibleWhen: software,
  },
  {
    key: 'min_threads',
    label: 'CPU threads',
    description:
      'Threads the software encoder uses. More threads cost a little compression efficiency; use the lowest number that keeps up with your stream.',
    category: 'encoders',
    group: 'Software',
    control: { kind: 'number', min: 1, max: 64, step: 1 },
    default: 2,
    visibleWhen: software,
  },
];

export const settings: Setting[] = [...video, ...encoders, ...nvidia, ...amd, ...intel, ...cpu];
