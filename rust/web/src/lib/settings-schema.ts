// The settings the Rust host reads, with its real defaults. Each part file
// covers some categories.
import type { Category, Schema } from './settings-types';
import { settings as basics } from './schema/basics';
import { settings as video } from './schema/video';
import { settings as display } from './schema/display';
import { settings as library } from './schema/library';

const categories: Category[] = [
  { id: 'general', label: 'General', description: 'Host name, language, tray icon, updates and logging.' },
  { id: 'network', label: 'Network', description: 'Ports, addresses, discovery, encryption and who may use this console.' },
  { id: 'video', label: 'Video', description: 'Capture method, video formats, data rate and frame timing.' },
  { id: 'encoders', label: 'Encoders', description: 'Video compression with NVIDIA NVENC, AMD AMF, Intel Quick Sync or software.' },
  { id: 'display', label: 'Display', description: 'Which display streams, virtual displays and how the layout changes during a stream.' },
  { id: 'frame-limiting', label: 'Frame limiting', description: 'Game frame rates, RivaTuner Statistics Server (RTSS) and generated frames.' },
  { id: 'hdr', label: 'HDR', description: 'High dynamic range (HDR) on the display and NVIDIA RTX HDR conversion.' },
  { id: 'audio', label: 'Audio', description: 'Which device is captured and where the host plays sound.' },
  { id: 'input', label: 'Input', description: 'Keyboard, mouse, touch, pen and controllers.' },
  { id: 'commands', label: 'Commands', description: 'Commands run before and after apps, or from a device.' },
  { id: 'library', label: 'Library', description: 'Games added from Steam and Playnite, and Lossless Scaling.' },
  { id: 'advanced', label: 'Advanced', description: 'Compatibility options and custom settings.' },
];

export const schema: Schema = {
  categories,
  settings: [...basics, ...video, ...display, ...library],
};
