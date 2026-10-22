# Steve SDK

Embed the Steve agent in your workflows and apps.

The TypeScript SDK wraps the `steve` CLI from `@handley-lab/steve`. It spawns the CLI and exchanges JSONL events over stdin/stdout.

## Installation

The package name is `@handley-lab/steve-sdk`; registry publication is pending.
Build it from this checkout for local use.

Requires Node.js 18+.

## Quickstart

Configure a model provider, endpoint, and credentials in the Steve CLI configuration or through the SDK's `config` or
`configOverrides` options. The SDK does not supply a missing `model_provider`.

```typescript
import { Steve } from "@handley-lab/steve-sdk";

const steve = new Steve();
const thread = steve.startThread();
const turn = await thread.run("Diagnose the test failure and propose a fix");

console.log(turn.finalResponse);
console.log(turn.items);
```

Call `run()` repeatedly on the same `Thread` instance to continue that conversation.

```typescript
const nextTurn = await thread.run("Implement the fix");
```

### Streaming responses

`run()` buffers events until the turn finishes. To react to intermediate progress—tool calls, streaming responses, and file change notifications—use `runStreamed()` instead, which returns an async generator of structured events.

```typescript
const { events } = await thread.runStreamed("Diagnose the test failure and propose a fix");

for await (const event of events) {
  switch (event.type) {
    case "item.completed":
      console.log("item", event.item);
      break;
    case "turn.completed":
      console.log("usage", event.usage);
      break;
  }
}
```

### Structured output

The Steve agent can produce a JSON response that conforms to a specified schema. The schema can be provided for each turn as a plain JSON object.

```typescript
const schema = {
  type: "object",
  properties: {
    summary: { type: "string" },
    status: { type: "string", enum: ["ok", "action_required"] },
  },
  required: ["summary", "status"],
  additionalProperties: false,
} as const;

const turn = await thread.run("Summarize repository status", { outputSchema: schema });
console.log(turn.finalResponse);
```

You can also create a JSON schema from a [Zod schema](https://github.com/colinhacks/zod) using the [`zod-to-json-schema`](https://www.npmjs.com/package/zod-to-json-schema) package and setting the `target` to `"openAi"`.

```typescript
const schema = z.object({
  summary: z.string(),
  status: z.enum(["ok", "action_required"]),
});

const turn = await thread.run("Summarize repository status", {
  outputSchema: zodToJsonSchema(schema, { target: "openAi" }),
});
console.log(turn.finalResponse);
```

### Attaching images

Provide structured input entries when you need to include images alongside text. Text entries are concatenated into the final prompt while image entries are passed to the Steve CLI via `--image`.

```typescript
const turn = await thread.run([
  { type: "text", text: "Describe these screenshots" },
  { type: "local_image", path: "./ui.png" },
  { type: "local_image", path: "./diagram.jpg" },
]);
```

### Resuming an existing thread

Threads are persisted in `~/.steve/sessions`. If you lose the in-memory `Thread` object, reconstruct it with `resumeThread()` and keep going.

```typescript
const savedThreadId = process.env.STEVE_THREAD_ID!;
const thread = steve.resumeThread(savedThreadId);
await thread.run("Implement the fix");
```

### Working directory controls

Steve runs in the current working directory by default. To avoid unrecoverable errors, Steve requires the working directory to be a Git repository. You can skip the Git repository check by passing the `skipGitRepoCheck` option when creating a thread.

```typescript
const thread = steve.startThread({
  workingDirectory: "/path/to/project",
  skipGitRepoCheck: true,
});
```

### Controlling the Steve CLI environment

By default, the Steve CLI inherits the Node.js process environment. Provide the optional `env` parameter when instantiating the
`Steve` client to fully control which variables the CLI receives—useful for sandboxed hosts like Electron apps.

```typescript
const steve = new Steve({
  env: {
    PATH: "/usr/local/bin",
  },
});
```

If you supply `apiKey`, the SDK sets `STEVE_API_KEY` in the child environment. The selected provider must declare that
credential source, for example with `env_key = "STEVE_API_KEY"`, to use it. If you set `baseUrl`, the SDK passes it as a
`--config openai_base_url=...` override; it does not change a custom provider's `model_providers.<id>.base_url`.

### Passing `--config` overrides

Use the `config` option to provide additional Steve CLI configuration overrides. The SDK accepts a JSON object, flattens it
into dotted paths, and serializes values as TOML literals before passing them as repeated `--config key=value` flags.

```typescript
const steve = new Steve({
  config: {
    show_raw_agent_reasoning: true,
    sandbox_workspace_write: { network_access: true },
  },
});
```

For configuration keys that cannot be expressed as dotted paths, pass raw TOML overrides with `configOverrides`. Each entry
is forwarded unchanged as a separate `--config` argument, without modifying `STEVE_HOME`:

```typescript
const steve = new Steve({
  config: { default_permissions: "audit" },
  configOverrides: ['permissions.audit.filesystem={":root"="read","/path/to/project/.env"="deny"}'],
});
```

Raw overrides are applied after structured `config` overrides and take precedence over them. SDK-managed settings, such as
`baseUrl`, and thread-specific options are applied afterward and take precedence.
