export interface ReadinessMetadata {
  platform?: string;
  encoder_status?: {
    state?: 'ready' | 'failed' | 'unknown';
    h264?: boolean;
    hevc?: boolean;
    av1?: boolean;
    pyrowave?: boolean;
  };
  capture_status?: {
    configured_backend?: string;
    observed_backend?: string;
    managed_event_driven?: boolean;
    virtual_display_configured?: boolean;
  };
  virtual_display?: {
    capable?: boolean;
    ready?: boolean;
    reason?: string;
    hdr?: string;
    modes?: string[];
    layouts?: string[];
  };
  virtual_display_driver?: {
    active?: string | null;
    status?: string;
    status_code?: number;
  };
}
export function hostReadiness(
  metadata: ReadinessMetadata | null,
  streaming: boolean,
  failed: boolean,
): 'healthy' | 'streaming' | 'warning' | 'unknown' {
  if (failed) return 'warning';
  if (streaming) return 'streaming';
  if (!metadata) return 'unknown';
  if (metadata.encoder_status?.state === 'failed') return 'warning';
  return metadata.encoder_status?.state === 'ready' ? 'healthy' : 'unknown';
}
