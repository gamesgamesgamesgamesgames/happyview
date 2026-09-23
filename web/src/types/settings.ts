export type SettingEntry = {
  key: string
  value: string
  source: "database" | "env"
}

export type InstanceSettings = {
  app_name: string
  client_uri: string
  logo_uri: string
  tos_uri: string
  policy_uri: string
}

export const INSTANCE_SETTING_KEYS = [
  "app_name",
  "client_uri",
  "logo_uri",
  "tos_uri",
  "policy_uri",
] as const satisfies readonly (keyof InstanceSettings)[]

export type ScriptLimitSettings = {
  script_instruction_limit: string
  script_wall_clock_seconds: string
}

export const SCRIPT_LIMIT_SETTING_KEYS = [
  "script_instruction_limit",
  "script_wall_clock_seconds",
] as const satisfies readonly (keyof ScriptLimitSettings)[]

/** What each limit is when neither a setting nor its env var is set. */
export const SCRIPT_LIMIT_DEFAULTS: ScriptLimitSettings = {
  script_instruction_limit: "1000000",
  script_wall_clock_seconds: "10",
}

/**
 * Mirrors `INSTRUCTION_LIMIT_RANGE` and `WALL_CLOCK_SECONDS_RANGE` in
 * `src/lua/limits.rs`, which is what the server enforces; a Rust test pins
 * this copy to those constants.
 */
export const SCRIPT_LIMIT_BOUNDS: Record<
  keyof ScriptLimitSettings,
  { min: number; max: number }
> = {
  script_instruction_limit: { min: 1000, max: 1000000000 },
  script_wall_clock_seconds: { min: 1, max: 300 },
}
