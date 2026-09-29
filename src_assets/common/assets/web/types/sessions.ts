export interface SessionStatus {
  activeSessions: number;
  appRunning: boolean;
  appName: string;
  paused: boolean;
  lastEncoderProbeFailed: boolean;
  status: boolean;
}
