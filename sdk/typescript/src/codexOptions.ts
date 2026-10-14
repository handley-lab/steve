export type SteveConfigValue = string | number | boolean | SteveConfigValue[] | SteveConfigObject;

export type SteveConfigObject = { [key: string]: SteveConfigValue };

export type SteveOptions = {
  stevePathOverride?: string;
  baseUrl?: string;
  apiKey?: string;
  /**
   * Additional `--config key=value` overrides to pass to the Steve CLI.
   *
   * Provide a JSON object and the SDK will flatten it into dotted paths and
   * serialize values as TOML literals so they are compatible with the CLI's
   * `--config` parsing.
   */
  config?: SteveConfigObject;
  /**
   * Raw `--config key=value` overrides to pass unchanged to the Steve CLI after
   * structured configuration and before SDK-managed or thread-specific overrides.
   */
  configOverrides?: string[];
  /**
   * Environment variables passed to the Steve CLI process. When provided, the SDK
   * will not inherit variables from `process.env`.
   */
  env?: Record<string, string>;
};
