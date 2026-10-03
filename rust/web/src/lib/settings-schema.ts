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
  { id: 'video', label: 'Video', description: 'Capture method, codecs, bitrate and frame pacing.' },
  { id: 'encoders', label: 'Encoders', description: 'Which encoder runs and how NVENC, AMF, Quick Sync or software encoding is tuned.' },
  { id: 'display', label: 'Display', description: 'Which display streams, virtual displays and how the layout changes during a stream.' },
  { id: 'frame-limiting', label: 'Frame limiting', description: 'Frame limiters, RTSS and frame generation.' },
  { id: 'hdr', label: 'HDR', description: 'HDR on the display and NVIDIA RTX HDR tuning.' },
  { id: 'audio', label: 'Audio', description: 'Which device is captured and where the host plays sound.' },
  { id: 'input', label: 'Input', description: 'Keyboard, mouse, touch, pen and controllers.' },
  { id: 'commands', label: 'Commands', description: 'Commands run around every app and commands clients may run.' },
  { id: 'library', label: 'Game library', description: 'Games added from Steam and other launchers.' },
  { id: 'advanced', label: 'Advanced', description: 'Rarely needed options and settings this console does not describe.' },
];

export const schema: Schema = {
  categories,
  settings: [...basics, ...video, ...display, ...library],
};
