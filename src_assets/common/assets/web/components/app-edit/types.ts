export interface PrepCmd {
  do: string;
  undo: string;
  elevated?: boolean;
}

export type FrameGenerationProvider = 'nvidia-smooth-motion' | 'game-provided';
export type FrameGenerationMode = 'off' | FrameGenerationProvider;
export type AppVirtualDisplayMode = 'disabled' | 'per_client' | 'shared';
export type RtxHdrMode = 'inherit' | 'enabled' | 'disabled';
export type AppVirtualDisplayLayout =
  | 'exclusive'
  | 'extended'
  | 'extended_primary'
  | 'extended_isolated'
  | 'extended_primary_isolated';

export interface AppForm {
  index: number;
  uuid?: string;
  name: string;
  output: string;
  cmd: string;
  workingDir: string;
  imagePath: string;
  excludeGlobalPrepCmd: boolean;
  excludeGlobalStateCmd: boolean;
  configOverrides: Record<string, unknown>;
  elevated: boolean;
  autoDetach: boolean;
  waitAll: boolean;
  terminateOnPause: boolean;
  allowClientCommands: boolean;
  useAppIdentity: boolean;
  perClientAppIdentity: boolean;
  gamepad: string;
  scaleFactor: number;
  frameGenLimiterFix: boolean;
  exitTimeout: number;
  prepCmd: PrepCmd[];
  stateCmd: PrepCmd[];
  detached: string[];
  virtualScreen: boolean;
  virtualDisplayMode: AppVirtualDisplayMode | null;
  virtualDisplayLayout: AppVirtualDisplayLayout | null;
  frameGenerationProvider: FrameGenerationProvider;
  frameGenerationMode: FrameGenerationMode;
  rtxHdrMode: RtxHdrMode;
  rtxHdrValuesOverride: boolean;
  rtxHdrForceSdr: boolean;
  rtxHdrPeakBrightness: number;
  rtxHdrMiddleGray: number;
  rtxHdrContrast: number;
  rtxHdrSaturation: number;
  ddConfigurationOption?:
    | 'disabled'
    | 'verify_only'
    | 'ensure_active'
    | 'ensure_primary'
    | 'ensure_only_display'
    | null;
}

export interface ServerApp {
  name?: string;
  output?: string;
  'display-output'?: string;
  cmd?: string | string[];
  uuid?: string;
  'working-dir'?: string;
  'image-path'?: string;
  'exclude-global-prep-cmd'?: boolean;
  'config-overrides'?: Record<string, unknown>;
  elevated?: boolean;
  'auto-detach'?: boolean;
  'wait-all'?: boolean;
  'exclude-global-state-cmd'?: boolean;
  'state-cmd'?: Array<{ do?: string; undo?: string; elevated?: boolean }>;
  'terminate-on-pause'?: boolean;
  'virtual-display'?: boolean;
  'allow-client-commands'?: boolean;
  'use-app-identity'?: boolean;
  'per-client-app-identity'?: boolean;
  gamepad?: string;
  'scale-factor'?: number | string;
  'frame-gen-limiter-fix'?: boolean;
  'exit-timeout'?: number;
  'prep-cmd'?: Array<{ do?: string; undo?: string; elevated?: boolean }>;
  detached?: string[];
  'virtual-screen'?: boolean;
  'gen1-framegen-fix'?: boolean;
  'gen2-framegen-fix'?: boolean;
  'dlss-framegen-capture-fix'?: boolean;
  'frame-generation-provider'?: string;
  'frame-generation-mode'?: string;
  'virtual-display-mode'?: string;
  'virtual-display-layout'?: string;
  'dd-configuration-option'?: string;
}

export type FrameGenRequirementStatus = 'pass' | 'warn' | 'fail' | 'unknown';

export interface FrameGenDisplayTarget {
  fps: number;
  requiredHz: number;
  supported: boolean | null;
}

export interface FrameGenHealth {
  checkedAt: number;
  os: {
    status: FrameGenRequirementStatus;
    buildNumber: number | null;
    message: string;
  };
  capture: {
    status: FrameGenRequirementStatus;
    method: string;
    message: string;
  };
  rtss: {
    status: FrameGenRequirementStatus;
    installed: boolean;
    running: boolean;
    hooksDetected: boolean;
    message: string;
  };
  display: {
    status: FrameGenRequirementStatus;
    deviceLabel: string;
    deviceId: string;
    currentHz: number | null;
    targets: FrameGenDisplayTarget[];
    virtualActive: boolean;
    message: string;
    error?: string | null;
  };
  suggestion?: {
    message: string;
    emphasis: 'info' | 'warning';
  };
}
