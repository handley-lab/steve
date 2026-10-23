export type CyberAccessProgram = "standard" | "daybreak_blue" | "daybreak_red";

export type TurnOptions = {
  /**
   * Explicit experimental Cyber selection for this turn.
   * Omission leaves selection to the configured Daybreak behavior.
   */
  cyberAccessProgram?: CyberAccessProgram;
  /** JSON schema describing the expected agent output. */
  outputSchema?: unknown;
  /** AbortSignal to cancel the turn. */
  signal?: AbortSignal;
};
