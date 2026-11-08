export interface UpdateCheckResult {
  available: boolean;
  /** Update checks and installs are off in this build (D-037). */
  disabled?: boolean;
  version?: string;
  date?: string;
  body?: string;
}
