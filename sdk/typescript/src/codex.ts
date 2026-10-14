import { SteveOptions } from "./codexOptions";
import { CodexExec } from "./exec";
import { Thread } from "./thread";
import { ThreadOptions } from "./threadOptions";

/**
 * Steve is the main class for interacting with the Steve agent.
 *
 * Use the `startThread()` method to start a new thread or `resumeThread()` to resume a previously started thread.
 */
export class Steve {
  private exec: CodexExec;
  private options: SteveOptions;

  constructor(options: SteveOptions = {}) {
    const { stevePathOverride, env, config, configOverrides } = options;
    this.exec = new CodexExec(stevePathOverride, env, config, configOverrides);
    this.options = options;
  }

  /**
   * Starts a new conversation with an agent.
   * @returns A new thread instance.
   */
  startThread(options: ThreadOptions = {}): Thread {
    return new Thread(this.exec, this.options, options);
  }

  /**
   * Resumes a conversation with an agent based on the thread id.
   * Threads are persisted in ~/.steve/sessions.
   *
   * @param id The id of the thread to resume.
   * @returns A new thread instance.
   */
  resumeThread(id: string, options: ThreadOptions = {}): Thread {
    return new Thread(this.exec, this.options, options, id);
  }
}
